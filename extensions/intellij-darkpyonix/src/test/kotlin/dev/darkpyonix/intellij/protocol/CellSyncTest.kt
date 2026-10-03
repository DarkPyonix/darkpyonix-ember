package dev.darkpyonix.intellij.protocol

import dev.darkpyonix.intellij.format.NotebookFormat
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class CellSyncTest {
    private fun local(text: String) = NotebookFormat.parse(text).cells

    /** Server cells mirroring [text], with ids s0, s1, ... and version 1. */
    private fun server(text: String, ids: List<String>? = null) = local(text).mapIndexed { i, c ->
        DocCell(
            cellId = ids?.get(i) ?: "s$i", index = c.index, type = c.type, title = c.title, metadata = c.metadata,
            source = c.source, sourceSha256 = c.sourceSha256, version = 1,
        )
    }

    private val base = "import x\n# %%\na = 1\n# %%\nb = 2\n# %% [markdown]\nm()\n"

    @Test
    fun `identical documents need no ops`() {
        val l = local(base)
        val s = server(base)
        val m = CellMatcher.match(l, s)
        assertEquals(listOf("s0", "s1", "s2", "s3"), m)
        assertTrue(CellReconciler.reconcile(l, s, m).isEmpty())
    }

    @Test
    fun `a body edit is one update with base_version`() {
        val l = local(base.replace("b = 2", "b = 3"))
        val s = server(base)
        val m = CellMatcher.match(l, s)
        assertEquals(listOf("s0", "s1", "s2", "s3"), m) // falls back to index for the edited cell
        assertEquals(listOf(SyncOp.Update("s2", 1, "b = 3\n", null, null)), CellReconciler.reconcile(l, s, m))
    }

    @Test
    fun `matching prefers id metadata, then sha, then index (FR-R4)`() {
        val s = server(base, listOf("s0", "keep", "s2", "s3"))
        // Cell with id "keep" moved to the end and got a new body.
        val text = "import x\n# %%\nb = 2\n# %% [markdown]\nm()\n# %%\n# @id: \"keep\"\nchanged\n"
        val m = CellMatcher.match(local(text), s)
        assertEquals(listOf("s0", "s2", "s3", "keep"), m)
    }

    @Test
    fun `inserting a cell creates it after its neighbour`() {
        val text = "import x\n# %%\na = 1\n# %% [shell]\nls\n# %%\nb = 2\n# %% [markdown]\nm()\n"
        val l = local(text)
        val s = server(base)
        val m = CellMatcher.match(l, s)
        assertEquals(listOf("s0", "s1", null, "s2", "s3"), m)
        assertEquals(
            listOf(SyncOp.Create("new:2", "s1", "shell", "ls\n", emptyMap())),
            CellReconciler.reconcile(l, s, m),
        )
    }

    @Test
    fun `removing a cell deletes it`() {
        val text = "import x\n# %%\na = 1\n# %% [markdown]\nm()\n"
        val l = local(text)
        val s = server(base)
        val ops = CellReconciler.reconcile(l, s, CellMatcher.match(l, s))
        assertEquals(listOf(SyncOp.Delete("s2", 1)), ops)
    }

    @Test
    fun `reordering emits moves`() {
        val text = "import x\n# %%\nb = 2\n# %%\na = 1\n# %% [markdown]\nm()\n"
        val l = local(text)
        val s = server(base)
        val m = CellMatcher.match(l, s)
        assertEquals(listOf("s0", "s2", "s1", "s3"), m)
        assertEquals(listOf(SyncOp.Move("s2", 1)), CellReconciler.reconcile(l, s, m))
    }

    @Test
    fun `type and metadata changes are updates, the preamble is never deleted`() {
        val text = "import x\n# %% [shell]\n# @width: 1fr\na = 1\n"
        val l = local(text)
        val s = server(base)
        val ops = CellReconciler.reconcile(l, s, CellMatcher.match(l, s))
        assertTrue(ops.contains(SyncOp.Update("s1", 1, null, "shell", mapOf("width" to "1fr"))))
        assertTrue(ops.contains(SyncOp.Delete("s2", 1)))
        assertTrue(ops.contains(SyncOp.Delete("s3", 1)))
        assertTrue(ops.none { it is SyncOp.Delete && it.cellId == "s0" })
    }
}
