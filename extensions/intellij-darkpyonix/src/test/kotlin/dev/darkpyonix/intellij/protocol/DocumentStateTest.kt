package dev.darkpyonix.intellij.protocol

import dev.darkpyonix.intellij.json.Json
import dev.darkpyonix.intellij.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DocumentStateTest {
    private fun obj(json: String): Map<String, Any?> = Json.parse(json).jsonObject()!!

    private fun cellJson(id: String, index: Int, source: String, version: Int = 1, extra: String = "") =
        """{"cell_id":"$id","index":$index,"type":"${if (index == 0) "preamble" else "code"}","title":null,
           "metadata":{},"source":${Json.stringify(source)},"source_sha256":"sha-$id-$version","version":$version$extra}"""

    /** A snapshot like `GET /kernels/{id}/document` (FR-S1, FR-R4). */
    private fun snapshot(seq: Int = 10): DocumentState {
        val s = DocumentState()
        s.loadSnapshot(
            obj(
                """{"path":"/w/a.py","doc_version":4,"seq":$seq,"presence":[
                   {"client_id":"other-1","nickname":"mac","user":"alice","permission":"editor","last_seen":"t"}],
                   "cells":[${cellJson("p", 0, "import darkpyonix\n")},
                            ${cellJson("c1", 1, "x = 1\n", extra = ""","outputs":[{"output_type":"stream","name":"stdout","text":"old\n"}],"execution_count":3,"status":"ok","stale":true""")},
                            ${cellJson("c2", 2, "print(x)\n")}]}""",
            ),
        )
        return s
    }

    @Test
    fun `snapshot loads cells, outputs, presence and seq`() {
        val s = snapshot()
        assertEquals(listOf("p", "c1", "c2"), s.cells.map { it.cellId })
        val c1 = s.cell("c1")!!
        assertEquals(3L, c1.executionCount)
        assertEquals("old\n", c1.outputs.single()["text"])
        assertTrue(c1.stale)
        assertEquals(4L, s.docVersion)
        assertEquals(10L, s.subscribeSince)
        assertEquals("alice (mac)", s.presence.single().displayName)
    }

    @Test
    fun `events at or before the snapshot seq are ignored`() {
        val s = snapshot(seq = 10)
        assertEquals(ApplyResult.Kind.IGNORED, s.apply(10, "kernel.status", obj("""{"status":"busy"}""")).kind)
        assertNull(s.kernelStatus)
        assertEquals(ApplyResult.Kind.APPLIED, s.apply(11, "kernel.status", obj("""{"status":"busy"}""")).kind)
        assertEquals("busy", s.kernelStatus)
        // A duplicate delivery after a reconnect is ignored too.
        assertEquals(ApplyResult.Kind.IGNORED, s.apply(11, "kernel.status", obj("""{"status":"idle"}""")).kind)
    }

    @Test
    fun `a live run replaces outputs, merges streams and honours clear_output wait`() {
        val s = snapshot()
        s.apply(11, "run.started", obj("""{"run_id":"r1","mode":"cells","cells":[1],"params":{}}"""))
        assertEquals("queued", s.cell("c1")!!.status)
        s.apply(12, "cell.started", obj("""{"run_id":"r1","index":1,"execution_count":4}"""))
        var c1 = s.cell("c1")!!
        assertEquals("running", c1.status)
        assertTrue(c1.outputs.isEmpty())
        assertEquals(false, c1.stale)
        assertEquals("sha-c1-1", c1.outputsSourceSha256)

        s.apply(13, "output", obj("""{"run_id":"r1","index":1,"output":{"output_type":"stream","name":"stdout","text":"a"}}"""))
        s.apply(14, "output", obj("""{"run_id":"r1","index":1,"output":{"output_type":"stream","name":"stdout","text":["b","c\n"]}}"""))
        assertEquals("abc\n", s.cell("c1")!!.outputs.single()["text"])

        s.apply(15, "output.clear", obj("""{"run_id":"r1","index":1,"wait":true}"""))
        assertEquals(1, s.cell("c1")!!.outputs.size) // kept until the next output
        s.apply(16, "output", obj("""{"run_id":"r1","index":1,"output":{"output_type":"execute_result","data":{"text/plain":"2"},"metadata":{},"execution_count":4}}"""))
        c1 = s.cell("c1")!!
        assertEquals(1, c1.outputs.size)
        assertEquals("execute_result", c1.outputs.single()["output_type"])

        s.apply(17, "cell.finished", obj("""{"run_id":"r1","index":1,"status":"ok","duration":0.1}"""))
        assertEquals("ok", s.cell("c1")!!.status)
        s.apply(18, "run.finished", obj("""{"run_id":"r1","status":"ok","duration":0.2}"""))
        assertNull(s.currentRunId)
        assertEquals("ok", s.lastRunStatus)
    }

    @Test
    fun `output clear without wait empties at once`() {
        val s = snapshot()
        s.apply(11, "output.clear", obj("""{"run_id":"r1","index":1,"wait":false}"""))
        assertTrue(s.cell("c1")!!.outputs.isEmpty())
    }

    @Test
    fun `interrupted run settles running cells`() {
        val s = snapshot()
        s.apply(11, "run.started", obj("""{"run_id":"r2","mode":"all","cells":[1,2],"params":{}}"""))
        s.apply(12, "cell.started", obj("""{"run_id":"r2","index":1,"execution_count":5}"""))
        s.apply(13, "run.finished", obj("""{"run_id":"r2","status":"interrupted","duration":1,"interrupted_by":{"client_id":"other-1","user":"alice"}}"""))
        assertEquals("interrupted", s.cell("c1")!!.status)
        assertNull(s.cell("c2")!!.status) // never started
    }

    @Test
    fun `edit events from another client converge on the same document (FR-S1, FR-S2)`() {
        val s = snapshot()
        val created = s.apply(11, "doc.cell.created", obj("""{"doc_version":5,"cell":${cellJson("n1", 2, "y = 2\n")},"by":{"client_id":"other-1","user":"alice"}}"""))
        assertEquals(listOf("p", "c1", "n1", "c2"), s.cells.map { it.cellId })
        assertEquals(listOf(0, 1, 2, 3), s.cells.map { it.index })
        val change = created.docChange as DocChange.Created
        assertEquals("other-1", change.by?.clientId)

        s.apply(12, "doc.cell.updated", obj("""{"doc_version":6,"cell":${cellJson("c1", 1, "x = 10\n", version = 2)},"by":{"client_id":"other-1"}}"""))
        val c1 = s.cell("c1")!!
        assertEquals("x = 10\n", c1.source)
        assertEquals(2L, c1.version)
        assertEquals("old\n", c1.outputs.single()["text"]) // outputs survive a source edit

        // A stale (older version) update is ignored.
        assertEquals(ApplyResult.Kind.IGNORED, s.apply(13, "doc.cell.updated", obj("""{"doc_version":6,"cell":${cellJson("c1", 1, "x = 0\n", version = 1)},"by":"x"}""")).kind)
        assertEquals("x = 10\n", s.cell("c1")!!.source)

        s.apply(14, "doc.cell.moved", obj("""{"doc_version":7,"cell":${cellJson("c2", 1, "print(x)\n")},"by":{"client_id":"other-1"}}"""))
        assertEquals(listOf("p", "c2", "c1", "n1"), s.cells.map { it.cellId })
        assertEquals(listOf(0, 1, 2, 3), s.cells.map { it.index })

        s.apply(15, "doc.cell.deleted", obj("""{"doc_version":8,"cell_id":"n1","by":{"client_id":"other-1"}}"""))
        assertEquals(listOf("p", "c2", "c1"), s.cells.map { it.cellId })
        assertEquals(8L, s.docVersion)

        // `deleted` may also carry the id inside `cell`.
        s.apply(16, "doc.cell.deleted", obj("""{"doc_version":9,"cell":{"cell_id":"c1"},"by":{"client_id":"other-1"}}"""))
        assertEquals(listOf("p", "c2"), s.cells.map { it.cellId })
    }

    @Test
    fun `locks, conflicts and unlock (FR-S3, FR-S5)`() {
        val s = snapshot()
        s.apply(11, "doc.lock", obj("""{"doc_version":4,"cell_id":"c2","lock":{"cell_id":"c2","locked_by":"other-1","user":"alice","nickname":"mac","locked_at":"t","last_activity":"t"},"by":{"client_id":"other-1"}}"""))
        val lock = s.cell("c2")!!.lock
        assertNotNull(lock)
        assertEquals("other-1", lock!!.lockedBy)
        assertEquals("alice (mac)", lock.displayName)

        s.apply(12, "doc.conflict", obj("""{"cell_id":"c2","local":{"source":"a","version":2,"by":{"client_id":"other-1"}},"disk":{"source":"b"}}"""))
        assertEquals("b", s.cell("c2")!!.conflict!!["disk_source"])

        s.apply(13, "doc.unlock", obj("""{"doc_version":5,"cell_id":"c2","by":{"client_id":"other-1"},"reason":"idle"}"""))
        assertNull(s.cell("c2")!!.lock)
        assertNull(s.cell("c2")!!.conflict)
    }

    @Test
    fun `presence update merges and leave removes (FR-S4)`() {
        val s = snapshot()
        s.apply(11, "presence.update", obj("""{"client_id":"other-1","nickname":"mac","user":"alice","focused_cell_id":"c2","cursor":{"cell_id":"c2","line":1,"column":4}}"""))
        val p = s.presence.single()
        assertEquals("c2", p.focusedCellId)
        assertEquals(1, p.cursor!!.line)
        assertEquals("editor", p.permission) // kept from the snapshot

        s.apply(12, "presence.update", obj("""{"client_id":"new-2","nickname":"phone","user":"bob"}"""))
        assertEquals(2, s.presence.size)
        s.apply(13, "presence.leave", obj("""{"client_id":"other-1","nickname":"mac","user":"alice"}"""))
        assertEquals(listOf("new-2"), s.presence.map { it.clientId })
    }

    @Test
    fun `reload after an outside edit keeps outputs and marks changed cells stale`() {
        val s = snapshot()
        s.apply(11, "doc.reloaded", obj("""{"doc_version":9,"cause":"external","cells":[
            ${cellJson("p", 0, "import darkpyonix\n")}, ${cellJson("c2", 1, "print(x)\n")},
            {"cell_id":"c1","index":2,"type":"code","metadata":{},"source":"x = 3\n","source_sha256":"other","version":3}]}"""))
        assertEquals(listOf("p", "c2", "c1"), s.cells.map { it.cellId })
        val c1 = s.cell("c1")!!
        assertEquals("old\n", c1.outputs.single()["text"])
        assertTrue(c1.stale)
        assertEquals(9L, s.docVersion)
    }

    @Test
    fun `replay_truncated asks for a resync and unknown events are ignored`() {
        val s = snapshot()
        assertEquals(ApplyResult.Kind.RESYNC, s.apply(null, "replay_truncated", obj("""{"oldest_seq":50}""")).kind)
        assertEquals(ApplyResult.Kind.IGNORED, s.apply(11, "something.new", obj("{}")).kind)
    }
}
