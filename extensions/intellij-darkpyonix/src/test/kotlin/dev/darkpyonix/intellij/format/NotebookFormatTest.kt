package dev.darkpyonix.intellij.format

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Expected values were produced with the kernel's reference parser
 * (darkpyonix-core `kernel/darkpyonix/format`), so the plugin and the manager agree on cell
 * boundaries and `source_sha256` (SPEC FR-R4).
 */
class NotebookFormatTest {
    private val sample = "\"\"\"doc\"\"\"\nimport darkpyonix\n\n\n" +
        "# %% [code]  \n# @width: 1fr\n# @collapsed: true\n# @id: \"c-3f2a\"\nx = 1\n\n" +
        "# %% 데이터 로드 [markdown]\ndarkpyonix.markdown(\"\"\"\n# hi\n\"\"\")\n" +
        "# %%\nprint(x)\n" +
        "# %% [Concorrunt]\npass\n"

    @Test
    fun `cells, types, titles and metadata match the reference parser`() {
        val cells = NotebookFormat.parse(sample).cells
        assertEquals(5, cells.size)

        val pre = cells[0]
        assertEquals(0, pre.index)
        assertEquals("preamble", pre.type)
        assertEquals("\"\"\"doc\"\"\"\nimport darkpyonix\n\n\n", pre.source)
        assertEquals("c459898379cfb7f3c6c780686f784510399e7405183225a7fecfe6f3f92212d9", pre.sourceSha256)
        assertEquals(-1, pre.markerLine)

        val code = cells[1]
        assertEquals("code", code.type)
        assertEquals("code", code.rawType)
        assertNull(code.title)
        assertEquals(mapOf("width" to "1fr", "collapsed" to true, "id" to "c-3f2a"), code.metadata)
        assertEquals("c-3f2a", code.id)
        assertEquals("x = 1\n\n", code.source)
        assertEquals("8ff436def1451285599a1b1ad70800493b8dcafde2912e1a38345633054e4c26", code.sourceSha256)

        val md = cells[2]
        assertEquals("markdown", md.type)
        assertEquals("데이터 로드", md.title)
        assertEquals("darkpyonix.markdown(\"\"\"\n# hi\n\"\"\")\n", md.source)
        assertEquals("344fba6cb976fd184e37b253307f6b3ebfc232e61e137b3d318525a242a73c89", md.sourceSha256)

        val bare = cells[3]
        assertEquals("code", bare.type)
        assertNull(bare.rawType)
        assertEquals("print(x)\n", bare.source)
        assertEquals("a6794f6e727657a32f6157b080160ce3ee9ac935d200e6e5df8397265c281f9a", bare.sourceSha256)

        val alias = cells[4]
        assertEquals("concurrent", alias.type)
        assertEquals("Concorrunt", alias.rawType)
        assertEquals("d74ff0ee8da3b9806b18c877dbf29bbde50b5bd8e4dad7a3a725000feb82e8f1", alias.sourceSha256)
    }

    @Test
    fun `offsets and lines cover the text without gaps`() {
        val cells = NotebookFormat.parse(sample).cells
        assertEquals(0, cells[0].startOffset)
        for (i in 1 until cells.size) assertEquals(cells[i - 1].endOffset, cells[i].startOffset)
        assertEquals(sample.length, cells.last().endOffset)
        for (c in cells) {
            assertEquals(c.header, sample.substring(c.startOffset, c.bodyStartOffset))
            assertEquals(c.source, sample.substring(c.bodyStartOffset, c.endOffset))
        }
        // Marker of cell 1 is line 4; its three metadata lines push the body to line 8.
        assertEquals(4, cells[1].markerLine)
        assertEquals(8, cells[1].bodyStartLine)
        assertEquals(10, cells[1].endLine)
        assertEquals(10, cells[2].markerLine)
    }

    @Test
    fun `sha ignores CRLF and trailing blank lines`() {
        val crlf = NotebookFormat.parse("# %% [code]\r\nx = 1\r\n\r\n").cells[1]
        assertEquals("8ff436def1451285599a1b1ad70800493b8dcafde2912e1a38345633054e4c26", crlf.sourceSha256)
        assertEquals(NotebookFormat.sha256Hex("x = 1"), NotebookFormat.sourceSha256("x = 1\n   \n\t\n"))
    }

    @Test
    fun `empty text has an empty preamble`() {
        val cells = NotebookFormat.parse("").cells
        assertEquals(1, cells.size)
        assertEquals("", cells[0].source)
        assertEquals("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", cells[0].sourceSha256)
    }

    @Test
    fun `near-markers are not markers`() {
        assertEquals(1, NotebookFormat.parse("# %%x\n# %%%\n#%%\n").cells.size)
        assertTrue(NotebookFormat.isMarkerLine("# %%"))
        assertTrue(NotebookFormat.isMarkerLine("# %% [code]   "))
        assertTrue(NotebookFormat.isMarkerLine("# %%[shell]"))
        assertFalse(NotebookFormat.isMarkerLine(" # %%"))
        assertFalse(NotebookFormat.hasMarkers("print('# %%')\n"))
    }

    @Test
    fun `metadata stops at the first non-metadata line`() {
        val cell = NotebookFormat.parse("# %%\n# @a: 1\nx = 2\n# @b: 3\n").cells[1]
        assertEquals(mapOf<String, Any?>("a" to 1L), cell.metadata)
        assertEquals("x = 2\n# @b: 3\n", cell.source)
    }

    @Test
    fun `a file starting with a marker still has preamble 0`() {
        val cells = NotebookFormat.parse("# %% [code]\nx\n").cells
        assertEquals(2, cells.size)
        assertTrue(cells[0].isPreamble)
        assertEquals(1, cells[1].index)
        assertEquals(0, cells[1].markerLine)
    }

    @Test
    fun `BOM is kept out of the preamble but counted in offsets`() {
        val parsed = NotebookFormat.parse("﻿import x\n# %%\ny\n")
        assertTrue(parsed.bom)
        assertEquals("import x\n", parsed.cells[0].source)
        assertEquals(1, parsed.cells[0].startOffset)
        assertEquals(10, parsed.cells[1].startOffset)
    }

    @Test
    fun `formatHeader mirrors format_header`() {
        assertEquals("# %% [code]\n", NotebookFormat.formatHeader(null, "code", emptyMap()))
        assertEquals(
            "# %% Load [markdown]\n# @width: 1fr\n# @collapsed: true\n# @id: \"5\"\n",
            NotebookFormat.formatHeader("Load", "markdown", linkedMapOf("width" to "1fr", "collapsed" to true, "id" to "5")),
        )
    }

    @Test
    fun `kernel id is k_ plus 20 hex of the canonical path`() {
        assertEquals("k_7570bb3a04bd69b917b4", NotebookFormat.kernelIdFor("/home/u/exp/train.py"))
    }
}
