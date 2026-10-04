package dev.darkpyonix.intellij.editor

import com.intellij.icons.AllIcons
import com.intellij.openapi.Disposable
import com.intellij.openapi.actionSystem.ActionGroup
import com.intellij.openapi.actionSystem.AnAction
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.editor.EditorCustomElementRenderer
import com.intellij.openapi.editor.Inlay
import com.intellij.openapi.editor.colors.EditorFontType
import com.intellij.openapi.editor.event.CaretEvent
import com.intellij.openapi.editor.event.CaretListener
import com.intellij.openapi.editor.markup.GutterIconRenderer
import com.intellij.openapi.editor.markup.HighlighterLayer
import com.intellij.openapi.editor.markup.HighlighterTargetArea
import com.intellij.openapi.editor.markup.RangeHighlighter
import com.intellij.openapi.editor.markup.TextAttributes
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.util.Disposer
import com.intellij.ui.JBColor
import dev.darkpyonix.intellij.notebook.NotebookSession
import java.awt.Color
import java.awt.Font
import java.awt.Graphics
import java.awt.Rectangle
import javax.swing.Icon

/** DarkPyonix palette (extensions/vscode-darkpyonix-theme): light / dark. */
object Palette {
    val ember = JBColor(Color(0xC65F3C), Color(0xE07A55))
    val muted = JBColor(Color(0xA89883), Color(0x8C7B67))
    val lockedBackground = JBColor(Color(0xE8DECA), Color(0x2A221B))
    val markerBackground = JBColor(Color(0xF5EFE0), Color(0x211A15))
    val error = JBColor(Color(0xC0392B), Color(0xF07068))
}

/**
 * Per-editor decorations: a run/interrupt gutter icon on each cell marker, a tint on cells
 * locked by other clients, and an end-of-line note with execution state and who is there.
 */
class CellDecorator(val editor: Editor, val session: NotebookSession) : Disposable {
    private val highlighters = ArrayList<RangeHighlighter>()
    private val inlays = ArrayList<Inlay<*>>()

    init {
        session.addListener(this) { refresh() }
        editor.caretModel.addCaretListener(object : CaretListener {
            override fun caretPositionChanged(event: CaretEvent) = session.onCaret(editor)
        }, this)
        refresh()
    }

    fun refresh() {
        if (editor.isDisposed) return
        clear()
        val markup = editor.markupModel
        val others = session.others()
        val runningRun = session.state.currentRunId
        for ((position, pair) in session.mappedCells().withIndex()) {
            val (local, cell) = pair
            if (local.isPreamble) continue
            if (local.markerLine < 0 || local.markerLine >= editor.document.lineCount) continue

            val running = cell?.status == "running" || (cell?.status == "queued" && runningRun != null)
            val notes = ArrayList<String>()
            cell?.executionCount?.let { notes += "[$it]" }
            when (cell?.status) {
                "running" -> notes += "running"
                "queued" -> notes += "queued"
                "error" -> notes += "error"
                "interrupted" -> notes += "interrupted"
            }
            if (cell != null && session.isStale(local, cell)) notes += "stale"

            val lock = cell?.lock?.takeIf { it.lockedBy != session.identity.clientId }
            if (lock != null) {
                notes += "locked by ${lock.displayName}"
                if (local.endOffset > local.startOffset) {
                    highlighters += markup.addRangeHighlighter(
                        local.startOffset, local.endOffset, HighlighterLayer.SELECTION - 1,
                        TextAttributes(null, Palette.lockedBackground, null, null, Font.PLAIN),
                        HighlighterTargetArea.LINES_IN_RANGE,
                    )
                }
            }
            if (cell?.conflict != null) notes += "changed on disk while locked"
            val here = others.filter { it.focusedCellId != null && it.focusedCellId == cell?.cellId }
            for (p in here) {
                val line = p.cursor?.takeIf { it.cellId == cell?.cellId }?.line
                notes += p.displayName + (line?.let { " · line ${it + 1}" } ?: "")
            }

            val title = buildString {
                append("Cell ").append(local.index)
                local.title?.let { append(": ").append(it) }
                append(" [").append(local.type).append(']')
                if (notes.isNotEmpty()) append("\n").append(notes.joinToString(", "))
            }
            val h = markup.addLineHighlighter(local.markerLine, HighlighterLayer.ADDITIONAL_SYNTAX, null)
            h.gutterIconRenderer = CellGutterRenderer(session, position, running, title)
            highlighters += h

            if (notes.isNotEmpty()) {
                val offset = editor.document.getLineEndOffset(local.markerLine)
                val color = if (cell?.status == "error" || lock != null) Palette.ember else Palette.muted
                editor.inlayModel.addAfterLineEndElement(offset, true, NoteRenderer("  " + notes.joinToString(" · "), color))
                    ?.let { inlays += it }
            }
        }
    }

    private fun clear() {
        for (h in highlighters) editor.markupModel.removeHighlighter(h)
        highlighters.clear()
        for (i in inlays) Disposer.dispose(i)
        inlays.clear()
    }

    override fun dispose() {
        if (!editor.isDisposed) clear()
    }
}

/** Run (or, while the cell runs, interrupt) from the gutter; right-click for more. */
class CellGutterRenderer(
    private val session: NotebookSession,
    private val position: Int,
    private val running: Boolean,
    private val tooltip: String,
) : GutterIconRenderer() {
    override fun getIcon(): Icon = if (running) AllIcons.Actions.Suspend else AllIcons.Actions.Execute

    override fun getTooltipText(): String = tooltip + "\n" + if (running) "Click to interrupt" else "Click to run this cell"

    override fun isNavigateAction(): Boolean = true

    override fun getAlignment(): Alignment = Alignment.LEFT

    override fun getClickAction(): AnAction = object : DumbAwareAction() {
        override fun actionPerformed(e: AnActionEvent) {
            if (running) session.interrupt() else session.run(listOf(position))
        }
    }

    override fun getPopupMenuActions(): ActionGroup = DefaultActionGroup(
        object : DumbAwareAction("Run Cell", null, AllIcons.Actions.Execute) {
            override fun actionPerformed(e: AnActionEvent) = session.run(listOf(position))
        },
        object : DumbAwareAction("Run All Cells", null, AllIcons.Actions.Execute) {
            override fun actionPerformed(e: AnActionEvent) = session.run(null)
        },
        object : DumbAwareAction("Interrupt Kernel", null, AllIcons.Actions.Suspend) {
            override fun actionPerformed(e: AnActionEvent) = session.interrupt()
        },
    )

    override fun equals(other: Any?): Boolean =
        other is CellGutterRenderer && other.session === session && other.position == position &&
            other.running == running && other.tooltip == tooltip

    override fun hashCode(): Int = (position * 31 + running.hashCode()) * 31 + tooltip.hashCode()
}

/** Small italic text after the end of a marker line. */
class NoteRenderer(private val text: String, private val color: Color) : EditorCustomElementRenderer {
    private fun font(inlay: Inlay<*>): Font = inlay.editor.colorsScheme.getFont(EditorFontType.ITALIC)

    override fun calcWidthInPixels(inlay: Inlay<*>): Int =
        inlay.editor.contentComponent.getFontMetrics(font(inlay)).stringWidth(text) + 4

    override fun paint(inlay: Inlay<*>, g: Graphics, targetRegion: Rectangle, textAttributes: TextAttributes) {
        val f = font(inlay)
        g.font = f
        g.color = color
        val fm = g.getFontMetrics(f)
        val y = targetRegion.y + (targetRegion.height - fm.height) / 2 + fm.ascent
        g.drawString(text, targetRegion.x + 2, y)
    }
}
