package dev.darkpyonix.intellij.protocol

import dev.darkpyonix.intellij.format.NotebookCell
import kotlin.math.abs

/**
 * Pairs the cells of the editor buffer with the cells of the kernel's shared document, in the
 * FR-R4 order: `# @id` metadata, then `source_sha256`, then `index`. The preamble always pairs
 * with the preamble.
 */
object CellMatcher {
    /** For each local cell (by list position), the matched server `cell_id`, or null. */
    fun match(local: List<NotebookCell>, server: List<DocCell>): List<String?> {
        val result = arrayOfNulls<String>(local.size)
        val claimed = HashSet<String>()

        fun claim(i: Int, c: DocCell) {
            result[i] = c.cellId
            claimed += c.cellId
        }

        // Preamble.
        if (local.isNotEmpty() && local[0].isPreamble) {
            server.firstOrNull { it.index == 0 }?.let { claim(0, it) }
        }
        // 1. id
        for ((i, l) in local.withIndex()) {
            if (result[i] != null) continue
            val id = l.id ?: continue
            server.firstOrNull { it.cellId !in claimed && (it.cellId == id || it.metadata["id"] == id) }?.let { claim(i, it) }
        }
        // 2. source_sha256, nearest index first when several cells share a body.
        for ((i, l) in local.withIndex()) {
            if (result[i] != null || l.isPreamble) continue
            server.filter { it.cellId !in claimed && it.index != 0 && it.sourceSha256 == l.sourceSha256 }
                .minByOrNull { abs(it.index - l.index) }
                ?.let { claim(i, it) }
        }
        // 3. index
        for ((i, l) in local.withIndex()) {
            if (result[i] != null || l.isPreamble) continue
            server.firstOrNull { it.cellId !in claimed && it.index == l.index && it.index != 0 }?.let { claim(i, it) }
        }
        return result.toList()
    }
}

/** One edit request that brings the kernel's document in line with the editor buffer. */
sealed class SyncOp {
    /** `PATCH /cells/{cell_id}`; null fields are not sent. */
    data class Update(
        val cellId: String,
        val baseVersion: Long,
        val source: String?,
        val type: String?,
        val metadata: Map<String, Any?>?,
    ) : SyncOp()

    /**
     * `POST /cells`. [ref] names the new cell for later [Move]s (`new:<local position>`);
     * [afterRef] is the cell it follows (an existing `cell_id` or another `new:` ref).
     */
    data class Create(
        val ref: String,
        val afterRef: String?,
        val type: String,
        val source: String,
        val metadata: Map<String, Any?>,
    ) : SyncOp()

    data class Delete(val cellId: String, val baseVersion: Long) : SyncOp()

    /** `POST /cells/{cell_id}/move` with `to_index` (preamble is 0, so >= 1). */
    data class Move(val ref: String, val toIndex: Int) : SyncOp()
}

/**
 * Computes the edit requests (SPEC FR-S2) that turn the server document into the editor
 * buffer: deletes, then updates, then creates, then moves. Pure, so it can be unit-tested.
 */
object CellReconciler {
    fun newRef(localPosition: Int) = "new:$localPosition"

    fun reconcile(local: List<NotebookCell>, server: List<DocCell>, match: List<String?>): List<SyncOp> {
        val ops = ArrayList<SyncOp>()
        val byId = server.associateBy { it.cellId }
        val matched = match.filterNotNull().toSet()

        // Deletes: server cells nothing in the buffer maps to (never the preamble).
        for (s in server) {
            if (s.cellId !in matched && s.index != 0) ops += SyncOp.Delete(s.cellId, s.version)
        }

        // Updates of matched cells.
        for ((i, l) in local.withIndex()) {
            val s = byId[match[i] ?: continue] ?: continue
            val source = if (l.source != s.source) l.source else null
            val type = if (!l.isPreamble && l.type != s.type) l.type else null
            val metadata = if (!l.isPreamble && !sameMetadata(l.metadata, s.metadata)) l.metadata else null
            if (source != null || type != null || metadata != null) {
                ops += SyncOp.Update(s.cellId, s.version, source, type, metadata)
            }
        }

        // Creates, each after the buffer cell before it, and the order they leave behind.
        val order = server.filter { it.cellId in matched || it.index == 0 }.sortedBy { it.index }
            .map { it.cellId }.toMutableList()
        val refs = local.indices.map { i -> match[i] ?: newRef(i) }
        for ((i, l) in local.withIndex()) {
            if (match[i] != null) continue
            if (l.isPreamble) continue // a server document always has a preamble
            val after = if (i > 0) refs[i - 1] else null
            ops += SyncOp.Create(refs[i], after, l.type, l.source, l.metadata)
            val pos = if (after == null) 0 else order.indexOf(after) + 1
            order.add(pos.coerceIn(0, order.size), refs[i])
        }

        // Moves for cells whose relative order differs from the buffer's.
        val target = refs.filterIndexed { i, _ -> !(local[i].isPreamble && match[i] == null) }
        for ((pos, ref) in target.withIndex()) {
            if (pos >= order.size) break
            if (order[pos] == ref) continue
            if (pos == 0) continue // the preamble never moves
            order.remove(ref)
            order.add(pos, ref)
            ops += SyncOp.Move(ref, pos)
        }
        return ops
    }

    private fun sameMetadata(a: Map<String, Any?>, b: Map<String, Any?>): Boolean {
        if (a.size != b.size) return false
        return a.all { (k, v) -> b.containsKey(k) && norm(b[k]) == norm(v) }
    }

    /** Numbers compare by value whatever their boxed type. */
    private fun norm(v: Any?): Any? = when (v) {
        is Int, is Long, is Short, is Byte -> (v as Number).toLong()
        is Double -> if (v == Math.rint(v)) v.toLong() else v
        is Map<*, *> -> v.mapValues { norm(it.value) }
        is List<*> -> v.map { norm(it) }
        else -> v
    }
}
