package dev.darkpyonix.intellij.protocol

import dev.darkpyonix.intellij.json.bool
import dev.darkpyonix.intellij.json.int
import dev.darkpyonix.intellij.json.jsonObject
import dev.darkpyonix.intellij.json.list
import dev.darkpyonix.intellij.json.long
import dev.darkpyonix.intellij.json.obj
import dev.darkpyonix.intellij.json.str

/** A change to the shared document caused by an event, for mirroring into the editor. */
sealed class DocChange {
    abstract val by: Actor?

    data class Created(val cell: DocCell, override val by: Actor?) : DocChange()
    data class Updated(val before: DocCell?, val cell: DocCell, override val by: Actor?) : DocChange()
    data class Deleted(val cellId: String, override val by: Actor?) : DocChange()
    data class Moved(val cell: DocCell, override val by: Actor?) : DocChange()
    data class Reloaded(val cells: List<DocCell>, override val by: Actor? = null) : DocChange()
}

data class ApplyResult(
    val kind: Kind,
    val docChange: DocChange? = null,
    val outputsChanged: Boolean = false,
    val locksChanged: Boolean = false,
    val presenceChanged: Boolean = false,
    val runChanged: Boolean = false,
) {
    enum class Kind {
        APPLIED,

        /** Already seen (seq not newer than the last applied one) or not understood. */
        IGNORED,

        /** The event stream lost events (`replay_truncated`); fetch a new snapshot. */
        RESYNC,
    }

    companion object {
        val IGNORED = ApplyResult(Kind.IGNORED)
        val RESYNC = ApplyResult(Kind.RESYNC)
    }
}

/**
 * Client-side mirror of a kernel's shared document: the `GET /kernels/{id}/document` snapshot
 * (SPEC FR-S1, FR-R4) advanced by the events of PROTOCOL §3.4 and §4.
 *
 * Not thread-safe by itself; [apply] and [loadSnapshot] are `@Synchronized` and readers take
 * copies through [cells] / [presence].
 */
class DocumentState {
    private val cellList = ArrayList<DocCell>()
    private val presenceMap = LinkedHashMap<String, Presence>()

    /** `wait=true` clears waiting for the next output of that cell (nbformat `clear_output`). */
    private val pendingClear = HashSet<Int>()

    var docVersion: Long = 0
        private set

    /** Highest event seq applied (or covered by the snapshot). */
    var lastSeq: Long = 0
        private set

    var kernelStatus: String? = null
        private set

    var currentRunId: String? = null
        private set

    var currentRunStartedBy: Actor? = null
        private set

    var lastRunStatus: String? = null
        private set

    var path: String? = null
        private set

    var loaded: Boolean = false
        private set

    @get:Synchronized
    val cells: List<DocCell>
        get() = ArrayList(cellList)

    @get:Synchronized
    val presence: List<Presence>
        get() = ArrayList(presenceMap.values)

    @Synchronized
    fun cell(cellId: String): DocCell? = cellList.firstOrNull { it.cellId == cellId }

    @Synchronized
    fun cellAt(index: Int): DocCell? = cellList.firstOrNull { it.index == index }

    /**
     * Replaces everything with a `Document` snapshot. The snapshot's `seq` is taken as the last
     * event it already reflects: subscribe with `since = seq` and events up to `seq` are ignored.
     */
    @Synchronized
    fun loadSnapshot(doc: Map<String, Any?>) {
        cellList.clear()
        doc.list("cells")?.mapNotNull { it.jsonObject()?.let { m -> DocCell.from(m) } }?.let { cellList.addAll(it) }
        cellList.sortBy { it.index }
        presenceMap.clear()
        doc.list("presence")?.mapNotNull { Presence.from(it.jsonObject()) }?.forEach { presenceMap[it.clientId] = it }
        docVersion = doc.long("doc_version") ?: 0
        lastSeq = doc.long("seq") ?: 0
        path = doc.str("path")
        pendingClear.clear()
        doc.obj("latest_run")?.let { lastRunStatus = it.str("status") }
        loaded = true
    }

    /** Snapshot seq to pass as `since` when subscribing. */
    val subscribeSince: Long
        @Synchronized get() = lastSeq

    /**
     * Applies one event. [seq] is the SSE `id` (null for `replay_truncated`); [type] the SSE
     * `event`; [data] the decoded SSE `data`.
     */
    @Synchronized
    fun apply(seq: Long?, type: String, data: Map<String, Any?>): ApplyResult {
        if (type == "replay_truncated") return ApplyResult.RESYNC
        if (seq != null) {
            if (seq <= lastSeq) return ApplyResult.IGNORED
            lastSeq = seq
        }
        data.long("doc_version")?.let { if (it > docVersion) docVersion = it }
        return when (type) {
            "kernel.status" -> {
                kernelStatus = data.str("status")
                data.str("run_id")?.let { currentRunId = it }
                ApplyResult(ApplyResult.Kind.APPLIED, runChanged = true)
            }
            "run.queued" -> ApplyResult(ApplyResult.Kind.APPLIED, runChanged = true)
            "run.started" -> {
                currentRunId = data.str("run_id")
                currentRunStartedBy = Actor.from(data["started_by"])
                val runId = currentRunId
                // Cells about to run show as queued until their cell.started.
                data.list("cells")?.mapNotNull { (it as? Number)?.toInt() }?.forEach { idx ->
                    replaceAt(idx) { it.copy(status = "queued", runId = runId) }
                }
                ApplyResult(ApplyResult.Kind.APPLIED, outputsChanged = true, runChanged = true)
            }
            "run.finished" -> {
                val runId = data.str("run_id")
                val status = data.str("status")
                lastRunStatus = status
                for (i in cellList.indices) {
                    val c = cellList[i]
                    if (c.runId == runId && (c.status == "running" || c.status == "queued")) {
                        cellList[i] = c.copy(status = if (c.status == "queued") null else status)
                    }
                }
                if (currentRunId == runId) {
                    currentRunId = null
                    currentRunStartedBy = null
                }
                ApplyResult(ApplyResult.Kind.APPLIED, outputsChanged = true, runChanged = true)
            }
            "cell.started" -> {
                val idx = data.int("index") ?: return ApplyResult.IGNORED
                pendingClear.remove(idx)
                replaceAt(idx) {
                    it.copy(
                        outputs = emptyList(), executionCount = data.long("execution_count"),
                        status = "running", stale = false, runId = data.str("run_id"),
                        outputsSourceSha256 = it.sourceSha256,
                    )
                }
                ApplyResult(ApplyResult.Kind.APPLIED, outputsChanged = true)
            }
            "cell.finished" -> {
                val idx = data.int("index") ?: return ApplyResult.IGNORED
                replaceAt(idx) { it.copy(status = data.str("status")) }
                ApplyResult(ApplyResult.Kind.APPLIED, outputsChanged = true)
            }
            "output" -> {
                val idx = data.int("index") ?: return ApplyResult.IGNORED
                val output = data.obj("output") ?: return ApplyResult.IGNORED
                val clear = pendingClear.remove(idx)
                replaceAt(idx) {
                    val list = if (clear) ArrayList() else ArrayList(it.outputs)
                    Outputs.append(list, output)
                    it.copy(outputs = list, runId = data.str("run_id") ?: it.runId)
                }
                ApplyResult(ApplyResult.Kind.APPLIED, outputsChanged = true)
            }
            "output.clear" -> {
                val idx = data.int("index") ?: return ApplyResult.IGNORED
                if (data.bool("wait") == true) {
                    pendingClear.add(idx)
                } else {
                    pendingClear.remove(idx)
                    replaceAt(idx) { it.copy(outputs = emptyList()) }
                }
                ApplyResult(ApplyResult.Kind.APPLIED, outputsChanged = true)
            }
            "doc.cell.created" -> {
                val m = data.obj("cell") ?: return ApplyResult.IGNORED
                val cell = DocCell.from(m) ?: return ApplyResult.IGNORED
                cellList.removeAll { it.cellId == cell.cellId }
                insertAt(cell)
                ApplyResult(ApplyResult.Kind.APPLIED, docChange = DocChange.Created(cell, Actor.from(data["by"])))
            }
            "doc.cell.updated" -> {
                val m = data.obj("cell") ?: return ApplyResult.IGNORED
                val incoming = DocCell.from(m) ?: return ApplyResult.IGNORED
                val i = cellList.indexOfFirst { it.cellId == incoming.cellId }
                if (i < 0) {
                    insertAt(incoming)
                    return ApplyResult(ApplyResult.Kind.APPLIED, docChange = DocChange.Created(incoming, Actor.from(data["by"])))
                }
                val before = cellList[i]
                if (incoming.version < before.version) return ApplyResult.IGNORED
                val merged = before.withDocumentFieldsOf(incoming, m.containsKey("outputs"), m.containsKey("lock"))
                cellList[i] = merged
                if (merged.index != before.index) reorder()
                ApplyResult(ApplyResult.Kind.APPLIED, docChange = DocChange.Updated(before, merged, Actor.from(data["by"])))
            }
            "doc.cell.deleted" -> {
                val id = data.str("cell_id") ?: data.obj("cell")?.str("cell_id") ?: return ApplyResult.IGNORED
                val removed = cellList.removeAll { it.cellId == id }
                if (!removed) return ApplyResult.IGNORED
                renumber()
                ApplyResult(ApplyResult.Kind.APPLIED, docChange = DocChange.Deleted(id, Actor.from(data["by"])))
            }
            "doc.cell.moved" -> {
                val m = data.obj("cell") ?: return ApplyResult.IGNORED
                val incoming = DocCell.from(m) ?: return ApplyResult.IGNORED
                val i = cellList.indexOfFirst { it.cellId == incoming.cellId }
                val moved = if (i >= 0) {
                    val old = cellList.removeAt(i)
                    old.withDocumentFieldsOf(incoming, m.containsKey("outputs"), m.containsKey("lock"))
                } else {
                    incoming
                }
                insertAt(moved)
                ApplyResult(ApplyResult.Kind.APPLIED, docChange = DocChange.Moved(cell(moved.cellId) ?: moved, Actor.from(data["by"])))
            }
            "doc.lock" -> {
                val id = data.str("cell_id") ?: return ApplyResult.IGNORED
                val lock = CellLock.from(data.obj("lock"), id) ?: Actor.from(data["by"])?.let {
                    CellLock(id, it.clientId ?: "", it.user ?: "", it.nickname, null, null, null)
                }
                replaceById(id) { it.copy(lock = lock) }
                ApplyResult(ApplyResult.Kind.APPLIED, locksChanged = true)
            }
            "doc.unlock" -> {
                val id = data.str("cell_id") ?: return ApplyResult.IGNORED
                // Unlocking resolves an FR-S5 conflict too.
                replaceById(id) { it.copy(lock = null, conflict = null) }
                ApplyResult(ApplyResult.Kind.APPLIED, locksChanged = true)
            }
            "doc.reloaded" -> {
                val incoming = data.list("cells")?.mapNotNull { it.jsonObject() } ?: return ApplyResult.IGNORED
                val old = cellList.associateBy { it.cellId }
                val next = incoming.mapNotNull { m ->
                    val c = DocCell.from(m) ?: return@mapNotNull null
                    val prev = old[c.cellId] ?: return@mapNotNull c
                    val merged = prev.withDocumentFieldsOf(c, m.containsKey("outputs"), m.containsKey("lock"))
                    val changed = prev.sourceSha256 != c.sourceSha256
                    if (!m.containsKey("outputs") && changed && prev.outputs.isNotEmpty()) merged.copy(stale = true) else merged
                }
                cellList.clear()
                cellList.addAll(next)
                cellList.sortBy { it.index }
                ApplyResult(ApplyResult.Kind.APPLIED, docChange = DocChange.Reloaded(cells), outputsChanged = true, locksChanged = true)
            }
            "doc.conflict" -> {
                val id = data.str("cell_id") ?: return ApplyResult.IGNORED
                val disk = data.obj("disk")?.str("source")
                replaceById(id) { it.copy(conflict = mapOf("disk_source" to disk, "local" to data.obj("local"))) }
                ApplyResult(ApplyResult.Kind.APPLIED, locksChanged = true)
            }
            "presence.update" -> {
                val id = data.str("client_id") ?: return ApplyResult.IGNORED
                val prev = presenceMap[id]
                val next = prev?.merge(data) ?: Presence.from(data) ?: return ApplyResult.IGNORED
                presenceMap[id] = next
                ApplyResult(ApplyResult.Kind.APPLIED, presenceChanged = true)
            }
            "presence.leave" -> {
                val id = data.str("client_id") ?: return ApplyResult.IGNORED
                presenceMap.remove(id)
                ApplyResult(ApplyResult.Kind.APPLIED, presenceChanged = true)
            }
            else -> ApplyResult.IGNORED // PR-4: unknown events are ignored
        }
    }

    /** Replaces one cell with the server's answer to our own edit (keeps outputs). */
    @Synchronized
    fun upsertFromResponse(m: Map<String, Any?>) {
        val incoming = DocCell.from(m) ?: return
        val i = cellList.indexOfFirst { it.cellId == incoming.cellId }
        if (i < 0) {
            insertAt(incoming)
            return
        }
        if (incoming.version < cellList[i].version) return
        val merged = cellList[i].withDocumentFieldsOf(incoming, m.containsKey("outputs"), m.containsKey("lock"))
        cellList[i] = merged
        reorder()
    }

    @Synchronized
    fun removeCell(cellId: String) {
        if (cellList.removeAll { it.cellId == cellId }) renumber()
    }

    @Synchronized
    fun setLock(cellId: String, lock: CellLock?) = replaceById(cellId) { it.copy(lock = lock) }

    private inline fun replaceAt(index: Int, f: (DocCell) -> DocCell) {
        val i = cellList.indexOfFirst { it.index == index }
        if (i >= 0) cellList[i] = f(cellList[i])
    }

    private inline fun replaceById(id: String, f: (DocCell) -> DocCell) {
        val i = cellList.indexOfFirst { it.cellId == id }
        if (i >= 0) cellList[i] = f(cellList[i])
    }

    /** Inserts [cell] at its `index` (clamped; never before the preamble) and renumbers. */
    private fun insertAt(cell: DocCell) {
        val pos = cell.index.coerceIn(if (cellList.firstOrNull()?.index == 0 && cell.index > 0) 1 else 0, cellList.size)
        cellList.add(pos, cell)
        renumber()
    }

    private fun reorder() {
        val sorted = cellList.sortedBy { it.index }
        cellList.clear()
        cellList.addAll(sorted)
        renumber()
    }

    private fun renumber() {
        for (i in cellList.indices) {
            if (cellList[i].index != i) cellList[i] = cellList[i].copy(index = i)
        }
    }
}
