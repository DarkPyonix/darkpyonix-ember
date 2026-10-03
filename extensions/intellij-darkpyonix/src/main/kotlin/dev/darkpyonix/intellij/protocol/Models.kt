package dev.darkpyonix.intellij.protocol

import dev.darkpyonix.intellij.json.bool
import dev.darkpyonix.intellij.json.int
import dev.darkpyonix.intellij.json.jsonList
import dev.darkpyonix.intellij.json.jsonObject
import dev.darkpyonix.intellij.json.list
import dev.darkpyonix.intellij.json.long
import dev.darkpyonix.intellij.json.obj
import dev.darkpyonix.intellij.json.str

/** `Lock` of manager.openapi.yaml (SPEC FR-S3). */
data class CellLock(
    val cellId: String,
    val lockedBy: String,
    val user: String,
    val nickname: String?,
    val lockedAt: String?,
    val lastActivity: String?,
    val expiresAt: String?,
) {
    companion object {
        fun from(m: Map<String, Any?>?, cellIdFallback: String? = null): CellLock? {
            if (m == null) return null
            val lockedBy = m.str("locked_by") ?: return null
            return CellLock(
                cellId = m.str("cell_id") ?: cellIdFallback ?: return null,
                lockedBy = lockedBy,
                user = m.str("user") ?: "",
                nickname = m.str("nickname"),
                lockedAt = m.str("locked_at"),
                lastActivity = m.str("last_activity"),
                expiresAt = m.str("expires_at"),
            )
        }
    }

    val displayName: String get() = listOfNotNull(user.ifEmpty { null }, nickname?.let { "($it)" }).joinToString(" ")
}

/** `Cursor` of manager.openapi.yaml. Lines and columns are 0-based within the cell source. */
data class CellCursor(val cellId: String, val line: Int, val column: Int, val selection: List<List<Int>>? = null) {
    fun toJson(): Map<String, Any?> = linkedMapOf<String, Any?>(
        "cell_id" to cellId, "line" to line, "column" to column,
    ).also { if (selection != null) it["selection"] = selection }

    companion object {
        fun from(m: Map<String, Any?>?): CellCursor? {
            if (m == null) return null
            return CellCursor(
                cellId = m.str("cell_id") ?: return null,
                line = m.int("line") ?: 0,
                column = m.int("column") ?: 0,
                selection = m.list("selection")?.mapNotNull { p -> p.jsonList()?.mapNotNull { (it as? Number)?.toInt() } },
            )
        }
    }
}

/** `Presence` of manager.openapi.yaml (SPEC FR-S4). */
data class Presence(
    val clientId: String,
    val nickname: String,
    val user: String,
    val avatar: String?,
    val permission: String?,
    val focusedCellId: String?,
    val focusedAt: String?,
    val cursor: CellCursor?,
    val lastSeen: String?,
) {
    val displayName: String get() = if (nickname.isEmpty()) user else "$user ($nickname)"

    /** Applies a `presence.update` payload: present keys replace, absent keys are kept. */
    fun merge(m: Map<String, Any?>): Presence = copy(
        nickname = m.str("nickname") ?: nickname,
        user = m.str("user") ?: user,
        avatar = if (m.containsKey("avatar")) m.str("avatar") else avatar,
        permission = m.str("permission") ?: permission,
        focusedCellId = if (m.containsKey("focused_cell_id")) m.str("focused_cell_id") else focusedCellId,
        focusedAt = if (m.containsKey("focused_at")) m.str("focused_at") else focusedAt,
        cursor = if (m.containsKey("cursor")) CellCursor.from(m.obj("cursor")) else cursor,
        lastSeen = m.str("last_seen") ?: lastSeen,
    )

    companion object {
        fun from(m: Map<String, Any?>?): Presence? {
            if (m == null) return null
            return Presence(
                clientId = m.str("client_id") ?: return null,
                nickname = m.str("nickname") ?: "",
                user = m.str("user") ?: "",
                avatar = m.str("avatar"),
                permission = m.str("permission"),
                focusedCellId = m.str("focused_cell_id"),
                focusedAt = m.str("focused_at"),
                cursor = CellCursor.from(m.obj("cursor")),
                lastSeen = m.str("last_seen"),
            )
        }
    }
}

/**
 * A cell of the kernel's shared document (`DocumentCell`), with the latest outputs mapped onto
 * it (SPEC FR-R4) and kept current from live events.
 */
data class DocCell(
    val cellId: String,
    val index: Int,
    val type: String,
    val title: String?,
    val metadata: Map<String, Any?>,
    val source: String,
    val sourceSha256: String,
    val version: Long,
    val outputs: List<Map<String, Any?>> = emptyList(),
    val executionCount: Long? = null,
    /** `ok` / `error` / `interrupted` from the record, or `running` / `queued` while live. */
    val status: String? = null,
    val stale: Boolean = false,
    val runId: String? = null,
    val lock: CellLock? = null,
    /** FR-S5: the file changed on disk while this cell was locked; holds `disk_source`. */
    val conflict: Map<String, Any?>? = null,
    /** source_sha256 of the source the outputs were produced from (live runs only). */
    val outputsSourceSha256: String? = null,
) {
    companion object {
        /** Parses a `DocumentCell`. Missing output fields default to "no outputs". */
        fun from(m: Map<String, Any?>): DocCell? {
            val cellId = m.str("cell_id") ?: return null
            return DocCell(
                cellId = cellId,
                index = m.int("index") ?: 0,
                type = m.str("type") ?: "code",
                title = m.str("title"),
                metadata = m.obj("metadata") ?: emptyMap(),
                source = m.str("source") ?: "",
                sourceSha256 = m.str("source_sha256") ?: "",
                version = m.long("version") ?: 1,
                outputs = m.list("outputs")?.mapNotNull { it.jsonObject() }?.let { Outputs.normalize(it) } ?: emptyList(),
                executionCount = m.long("execution_count"),
                status = m.str("status"),
                stale = m.bool("stale") ?: false,
                runId = m.str("run_id"),
                lock = CellLock.from(m.obj("lock"), cellId),
                conflict = m.obj("conflict"),
            )
        }
    }

    /**
     * The document fields of [other] (an edit event or response) applied over this cell; the
     * outputs are kept unless [other] carried an `outputs` key.
     */
    fun withDocumentFieldsOf(other: DocCell, otherHadOutputs: Boolean, otherHadLock: Boolean): DocCell = copy(
        index = other.index,
        type = other.type,
        title = other.title,
        metadata = other.metadata,
        source = other.source,
        sourceSha256 = other.sourceSha256,
        version = other.version,
        outputs = if (otherHadOutputs) other.outputs else outputs,
        executionCount = if (otherHadOutputs) other.executionCount else executionCount,
        status = if (otherHadOutputs) other.status else status,
        stale = if (otherHadOutputs) other.stale else stale,
        runId = if (otherHadOutputs) other.runId else runId,
        lock = if (otherHadLock) other.lock else lock,
        conflict = other.conflict,
    )
}

/** Who did something (`by`, `started_by`, `interrupted_by`). */
data class Actor(val clientId: String?, val user: String?, val nickname: String?) {
    val displayName: String
        get() = listOfNotNull(user?.ifEmpty { null }, nickname?.ifEmpty { null }?.let { "($it)" })
            .joinToString(" ").ifEmpty { clientId ?: "someone" }

    companion object {
        fun from(v: Any?): Actor? = when (v) {
            is String -> Actor(v, null, null)
            is Map<*, *> -> v.jsonObject()?.let { Actor(it.str("client_id"), it.str("user"), it.str("nickname")) }
            else -> null
        }
    }
}

/** nbformat 4 output helpers. */
object Outputs {
    /** nbformat allows multi-line strings as lists of strings; join them. */
    fun text(v: Any?): String = when (v) {
        null -> ""
        is String -> v
        is List<*> -> v.joinToString("") { it?.toString() ?: "" }
        else -> v.toString()
    }

    fun normalize(outputs: List<Map<String, Any?>>): List<Map<String, Any?>> {
        val out = ArrayList<Map<String, Any?>>()
        for (o in outputs) append(out, o)
        return out
    }

    /** Appends [o], merging consecutive `stream` outputs of the same name (as Jupyter does). */
    fun append(list: MutableList<Map<String, Any?>>, o: Map<String, Any?>) {
        val last = list.lastOrNull()
        if (o.str("output_type") == "stream" && last != null && last.str("output_type") == "stream" &&
            last.str("name") == o.str("name")
        ) {
            list[list.size - 1] = LinkedHashMap(last).apply { put("text", text(last["text"]) + text(o["text"])) }
        } else {
            list.add(o)
        }
    }

    private val ANSI = Regex("\u001B\\[[0-9;?]*[ -/]*[@-~]")

    fun stripAnsi(s: String): String = ANSI.replace(s, "")

    /** A plain-text rendering of one output, for tool windows and tooltips. */
    fun plainText(o: Map<String, Any?>): String = when (o.str("output_type")) {
        "stream" -> text(o["text"])
        "error" -> {
            val tb = o.list("traceback")?.joinToString("\n") { it?.toString() ?: "" }
            stripAnsi(tb?.ifEmpty { null } ?: "${o.str("ename")}: ${o.str("evalue")}")
        }
        else -> {
            val data = o.obj("data") ?: emptyMap()
            text(data["text/plain"] ?: data["text/markdown"] ?: data["text/html"])
        }
    }
}
