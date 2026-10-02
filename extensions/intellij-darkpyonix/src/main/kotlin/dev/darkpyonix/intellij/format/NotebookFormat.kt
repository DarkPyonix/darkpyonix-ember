package dev.darkpyonix.intellij.format

import dev.darkpyonix.intellij.json.Json
import java.security.MessageDigest

/**
 * One cell of a DarkPyonix notebook file (docs/FORMAT.md §2).
 *
 * Offsets and lines refer to the parsed text. [startOffset] is the marker line (or 0 for the
 * preamble), [bodyStartOffset] the first line after the metadata lines, [endOffset] the start of
 * the next marker (or the end of the text). Like the kernel's parser, blank lines between cells
 * belong to the preceding cell's [source].
 */
data class NotebookCell(
    val index: Int,
    /** Canonical type: lower-cased, aliases resolved (`concorrunt` -> `concurrent`). */
    val type: String,
    /** Type as written between `[ ]`, or null when the marker has none. */
    val rawType: String?,
    val title: String?,
    val metadata: Map<String, Any?>,
    /** Marker + metadata lines, line endings included (empty for the preamble). */
    val header: String,
    val source: String,
    val sourceSha256: String,
    val startOffset: Int,
    val bodyStartOffset: Int,
    val endOffset: Int,
    /** Line of the marker, or -1 for the preamble. */
    val markerLine: Int,
    val bodyStartLine: Int,
    /** Exclusive. */
    val endLine: Int,
) {
    /** `# @id` metadata, if present (FORMAT §2.4). */
    val id: String?
        get() = metadata["id"]?.let { it as? String ?: Json.stringify(it) }

    val isPreamble: Boolean get() = index == 0

    fun containsOffset(offset: Int): Boolean = offset in startOffset until endOffset
}

/**
 * Parser for `.py` / `.pynb` notebooks. A line-for-line port of the kernel's reference parser
 * (`kernel/darkpyonix/format/_parser.py`), so cell boundaries, metadata and `source_sha256`
 * agree with what the manager reports (SPEC FR-R4).
 */
object NotebookFormat {
    const val PREAMBLE = "preamble"
    const val CODE = "code"

    val KNOWN_TYPES = listOf(
        "preamble", "code", "markdown", "argparse", "binding", "shell",
        "parallel", "concurrent", "cinterop", "cppinterop", "rustinterop",
        "sql", "toml", "yaml", "json",
    )
    private val TYPE_ALIASES = mapOf("concorrunt" to "concurrent")

    /** FORMAT §2.2, verbatim. `(?d)`: only `\n` ends a line, as in Python. */
    val MARKER_RE = Regex(
        "(?d)^# %%(?:[ \\t]+(?<title>[^\\[\\n]*?))?(?:[ \\t]*\\[(?<type>[A-Za-z_][A-Za-z0-9_-]*)\\])?[ \\t]*$",
    )

    /** FORMAT §2.3: `# @key: value`. `(?s)` so `.` also takes a stray `\r`, as in Python. */
    private val METADATA_RE = Regex(
        "(?ds)^# @(?<key>[A-Za-z_][A-Za-z0-9_.-]*):(?:[ \\t]*(?<value>.*?))?[ \\t]*$",
    )

    private const val BOM = '﻿'

    data class Parsed(val cells: List<NotebookCell>, val bom: Boolean)

    fun isMarkerLine(line: String): Boolean = MARKER_RE.find(content(line)) != null

    fun hasMarkers(text: CharSequence): Boolean =
        text.lineSequence().any { MARKER_RE.find(content(it)) != null }

    fun parse(input: String): Parsed {
        val bom = input.isNotEmpty() && input[0] == BOM
        val text = if (bom) input.substring(1) else input
        val offsetBase = if (bom) 1 else 0

        val cells = ArrayList<NotebookCell>()

        // State of the cell being collected.
        var match: MatchResult? = null
        var items = LinkedHashMap<String, Any?>()
        val header = StringBuilder()
        val body = StringBuilder()
        var cellStart = 0
        var bodyStart = 0
        var markerLine = -1
        var bodyStartLine = 0
        var inMetadata = false

        fun finish(endOffset: Int, endLine: Int) {
            val source = body.toString()
            val index = cells.size
            val m = match
            if (m == null) {
                cells += NotebookCell(
                    index = 0, type = PREAMBLE, rawType = null, title = null, metadata = emptyMap(),
                    header = header.toString(), source = source, sourceSha256 = sourceSha256(source),
                    startOffset = offsetBase + cellStart, bodyStartOffset = offsetBase + bodyStart,
                    endOffset = offsetBase + endOffset, markerLine = -1, bodyStartLine = bodyStartLine,
                    endLine = endLine,
                )
            } else {
                val rawType = m.groups["type"]?.value
                val type = rawType?.lowercase()?.let { TYPE_ALIASES[it] ?: it } ?: CODE
                val title = m.groups["title"]?.value?.takeIf { it.isNotEmpty() }
                cells += NotebookCell(
                    index = index, type = type, rawType = rawType, title = title, metadata = items,
                    header = header.toString(), source = source, sourceSha256 = sourceSha256(source),
                    startOffset = offsetBase + cellStart, bodyStartOffset = offsetBase + bodyStart,
                    endOffset = offsetBase + endOffset, markerLine = markerLine, bodyStartLine = bodyStartLine,
                    endLine = endLine,
                )
            }
        }

        var offset = 0
        var lineNo = 0
        for (line in splitLines(text)) {
            val c = content(line)
            val marker = MARKER_RE.find(c)
            if (marker != null) {
                finish(offset, lineNo)
                match = marker
                items = LinkedHashMap()
                header.setLength(0)
                body.setLength(0)
                header.append(line)
                cellStart = offset
                markerLine = lineNo
                bodyStart = offset + line.length
                bodyStartLine = lineNo + 1
                inMetadata = true
            } else {
                var consumed = false
                if (inMetadata) {
                    val md = METADATA_RE.find(c)
                    if (md != null) {
                        items[md.groups["key"]!!.value] = metadataValue(md.groups["value"]?.value ?: "")
                        header.append(line)
                        bodyStart = offset + line.length
                        bodyStartLine = lineNo + 1
                        consumed = true
                    } else {
                        inMetadata = false
                    }
                }
                if (!consumed) body.append(line)
            }
            offset += line.length
            lineNo++
        }
        finish(offset, lineNo)
        return Parsed(cells, bom)
    }

    /** SHA-256 of a cell body: `\r\n` -> `\n`, trailing blank lines and the final newline dropped. */
    fun sourceSha256(source: String): String {
        val lines = source.replace("\r\n", "\n").split("\n").toMutableList()
        while (lines.isNotEmpty() && lines.last().isBlank()) lines.removeAt(lines.size - 1)
        return sha256Hex(lines.joinToString("\n"))
    }

    fun sha256Hex(text: String): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(text.toByteArray(Charsets.UTF_8))
        return digest.joinToString("") { "%02x".format(it) }
    }

    /** `k_` + the first 20 hex digits of SHA-256(canonical path) (PROTOCOL §2.6). */
    fun kernelIdFor(canonicalPath: String): String = "k_" + sha256Hex(canonicalPath).substring(0, 20)

    /** Marker and metadata lines for a cell that has no recorded header (`format_header`). */
    fun formatHeader(title: String?, type: String?, metadata: Map<String, Any?>): String {
        val marker = StringBuilder("# %%")
        if (!title.isNullOrEmpty()) marker.append(' ').append(title)
        if (!type.isNullOrEmpty()) marker.append(" [").append(type).append(']')
        val lines = mutableListOf(marker.toString())
        for ((key, value) in metadata) {
            val text = if (value is String && metadataValue(value) == value) value else Json.stringify(value)
            lines += "# @$key: $text"
        }
        return lines.joinToString("\n") + "\n"
    }

    /** Metadata values are JSON when they parse as JSON, otherwise the raw string. */
    fun metadataValue(raw: String): Any? = try {
        Json.parse(raw)
    } catch (_: Json.ParseException) {
        raw
    }

    /** Split on `\n` only, keeping line endings (Python `_split_lines`). */
    fun splitLines(text: String): List<String> {
        val out = ArrayList<String>()
        var start = 0
        while (true) {
            val nl = text.indexOf('\n', start)
            if (nl < 0) break
            out += text.substring(start, nl + 1)
            start = nl + 1
        }
        if (start < text.length) out += text.substring(start)
        return out
    }

    private fun content(line: String): String = when {
        line.endsWith("\r\n") -> line.substring(0, line.length - 2)
        line.endsWith("\n") -> line.substring(0, line.length - 1)
        else -> line
    }
}
