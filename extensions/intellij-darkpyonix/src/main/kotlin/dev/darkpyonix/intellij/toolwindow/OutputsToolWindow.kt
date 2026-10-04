package dev.darkpyonix.intellij.toolwindow

import com.intellij.openapi.Disposable
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.fileEditor.FileEditorManagerEvent
import com.intellij.openapi.fileEditor.FileEditorManagerListener
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.ui.components.JBLabel
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.content.ContentFactory
import com.intellij.util.ui.JBUI
import com.intellij.util.ui.UIUtil
import dev.darkpyonix.intellij.editor.Palette
import dev.darkpyonix.intellij.format.NotebookCell
import dev.darkpyonix.intellij.json.obj
import dev.darkpyonix.intellij.json.str
import dev.darkpyonix.intellij.notebook.NotebookService
import dev.darkpyonix.intellij.notebook.NotebookSession
import dev.darkpyonix.intellij.protocol.DocCell
import dev.darkpyonix.intellij.protocol.Outputs
import java.awt.BorderLayout
import java.awt.Component
import java.awt.Cursor
import java.awt.Font
import java.awt.event.MouseAdapter
import java.awt.event.MouseEvent
import java.util.Base64
import javax.swing.BoxLayout
import javax.swing.ImageIcon
import javax.swing.JComponent
import javax.swing.JEditorPane
import javax.swing.JPanel
import javax.swing.JTextArea

class OutputsToolWindowFactory : ToolWindowFactory, DumbAware {
    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = OutputsPanel(project)
        val content = ContentFactory.getInstance().createContent(panel.component, "", false)
        content.setDisposer(panel)
        toolWindow.contentManager.addContent(content)
    }
}

/**
 * Latest outputs per cell of the notebook in the selected editor: the document snapshot's
 * outputs (FR-R4) kept live by the event stream, with stale outputs marked.
 */
class OutputsPanel(private val project: Project) : Disposable {
    private val root = JPanel(BorderLayout())
    private val header = JBLabel()
    private val presence = JBLabel()
    private val cellsPanel = JPanel()
    val component: JComponent get() = root

    private var session: NotebookSession? = null
    private var sessionSubscription: Disposable? = null

    init {
        val toolbarGroup = DefaultActionGroup().apply {
            ActionManager.getInstance().getAction("DarkPyonix.Connect")?.let { add(it) }
            ActionManager.getInstance().getAction("DarkPyonix.RunAll")?.let { add(it) }
            ActionManager.getInstance().getAction("DarkPyonix.Interrupt")?.let { add(it) }
            ActionManager.getInstance().getAction("DarkPyonix.Restart")?.let { add(it) }
        }
        val toolbar = ActionManager.getInstance().createActionToolbar("DarkPyonixOutputs", toolbarGroup, true)
        toolbar.targetComponent = root

        val top = JPanel()
        top.layout = BoxLayout(top, BoxLayout.Y_AXIS)
        top.add(toolbar.component.also { it.alignmentX = Component.LEFT_ALIGNMENT })
        header.border = JBUI.Borders.empty(2, 8)
        presence.border = JBUI.Borders.empty(0, 8, 4, 8)
        presence.foreground = Palette.muted
        top.add(header.also { it.alignmentX = Component.LEFT_ALIGNMENT })
        top.add(presence.also { it.alignmentX = Component.LEFT_ALIGNMENT })

        cellsPanel.layout = BoxLayout(cellsPanel, BoxLayout.Y_AXIS)
        cellsPanel.border = JBUI.Borders.empty(4, 8)
        val holder = JPanel(BorderLayout()).apply { add(cellsPanel, BorderLayout.NORTH) }

        root.add(top, BorderLayout.NORTH)
        root.add(JBScrollPane(holder), BorderLayout.CENTER)

        val connection = project.messageBus.connect(this)
        connection.subscribe(FileEditorManagerListener.FILE_EDITOR_MANAGER, object : FileEditorManagerListener {
            override fun selectionChanged(event: FileEditorManagerEvent) = bindToSelection()
        })
        NotebookService.getInstance(project).addSessionsListener(this) { bindToSelection() }
        bindToSelection()
    }

    private fun bindToSelection() {
        val file = FileEditorManager.getInstance(project).selectedFiles.firstOrNull()
        val next = NotebookService.getInstance(project).session(file) ?: session?.takeIf { !it.project.isDisposed && NotebookService.getInstance(project).allSessions().contains(it) }
        if (next === session && sessionSubscription != null) {
            rebuild()
            return
        }
        sessionSubscription?.let { Disposer.dispose(it) }
        sessionSubscription = null
        session = next
        if (next != null) {
            val sub = Disposer.newDisposable("DarkPyonix outputs subscription")
            Disposer.register(this, sub)
            next.addListener(sub) { rebuild() }
            sessionSubscription = sub
        }
        rebuild()
    }

    private fun rebuild() {
        cellsPanel.removeAll()
        val s = session
        if (s == null) {
            header.text = "Open a DarkPyonix notebook (.pynb, or .py with # %% cells)."
            presence.text = ""
        } else {
            val st = s.state
            val kernel = st.kernelStatus ?: if (s.isConnected) "attached" else "no kernel"
            val run = st.currentRunId?.let { " · run $it" + (st.currentRunStartedBy?.let { b -> " by ${b.displayName}" } ?: "") } ?: ""
            header.text = "${s.file.name} · $kernel$run · ${s.statusText}" + (s.permission?.let { " · $it" } ?: "")
            val others = s.others()
            presence.text = if (others.isEmpty()) "" else "Here: " + others.joinToString(", ") { p ->
                val cell = p.focusedCellId?.let { id -> s.state.cell(id)?.index }
                p.displayName + (cell?.let { " (cell $it)" } ?: "")
            }
            for ((local, cell) in s.mappedCells()) {
                if (local.isPreamble && (cell == null || cell.outputs.isEmpty())) continue
                cellsPanel.add(cellView(s, local, cell))
            }
        }
        cellsPanel.revalidate()
        cellsPanel.repaint()
    }

    private fun cellView(s: NotebookSession, local: NotebookCell, cell: DocCell?): JComponent {
        val box = JPanel()
        box.layout = BoxLayout(box, BoxLayout.Y_AXIS)
        box.alignmentX = Component.LEFT_ALIGNMENT
        box.border = JBUI.Borders.emptyBottom(8)

        val title = buildString {
            append(if (local.isPreamble) "Preamble" else "[${cell?.executionCount ?: " "}] Cell ${local.index}")
            local.title?.let { append(": ").append(it) }
            append("  ").append(local.type)
            cell?.status?.let { append(" · ").append(it) }
            if (cell != null && s.isStale(local, cell)) append(" · stale (source changed since this output)")
            cell?.lock?.takeIf { it.lockedBy != s.identity.clientId }?.let { append(" · locked by ").append(it.displayName) }
        }
        val label = JBLabel(title)
        label.font = label.font.deriveFont(Font.BOLD)
        label.foreground = if (cell?.status == "error") Palette.ember else UIUtil.getLabelForeground()
        label.cursor = Cursor.getPredefinedCursor(Cursor.HAND_CURSOR)
        label.alignmentX = Component.LEFT_ALIGNMENT
        label.addMouseListener(object : MouseAdapter() {
            override fun mouseClicked(e: MouseEvent) = navigate(local)
        })
        box.add(label)

        if (cell == null) {
            box.add(note("Not in the kernel's document yet."))
        } else if (cell.outputs.isEmpty()) {
            if (cell.status == null) box.add(note("No output."))
        } else {
            for (o in cell.outputs) box.add(outputView(o))
        }
        return box
    }

    private fun navigate(local: NotebookCell) {
        val editor = FileEditorManager.getInstance(project).selectedTextEditor ?: return
        val offset = local.bodyStartOffset.coerceAtMost(editor.document.textLength)
        editor.caretModel.moveToOffset(offset)
        editor.scrollingModel.scrollToCaret(com.intellij.openapi.editor.ScrollType.CENTER)
        editor.contentComponent.requestFocusInWindow()
    }

    private fun note(text: String): JComponent = JBLabel(text).apply {
        foreground = Palette.muted
        alignmentX = Component.LEFT_ALIGNMENT
    }

    private fun outputView(o: Map<String, Any?>): JComponent {
        when (o.str("output_type")) {
            "stream" -> {
                val area = textArea(Outputs.stripAnsi(Outputs.text(o["text"])))
                if (o.str("name") == "stderr") area.foreground = Palette.error
                return area
            }
            "error" -> return textArea(Outputs.plainText(o)).also { it.foreground = Palette.error }
        }
        val data = o.obj("data") ?: return textArea(Outputs.plainText(o))
        data["image/png"]?.let { png ->
            try {
                val bytes = Base64.getMimeDecoder().decode(Outputs.text(png))
                return JBLabel(ImageIcon(bytes)).apply { alignmentX = Component.LEFT_ALIGNMENT }
            } catch (_: IllegalArgumentException) {
            }
        }
        data["text/html"]?.let { html ->
            return JEditorPane("text/html", Outputs.text(html)).apply {
                isEditable = false
                isOpaque = false
                alignmentX = Component.LEFT_ALIGNMENT
            }
        }
        return textArea(Outputs.plainText(o))
    }

    private fun textArea(text: String): JTextArea = JTextArea(text.trimEnd('\n')).apply {
        isEditable = false
        lineWrap = false
        font = Font(Font.MONOSPACED, Font.PLAIN, UIUtil.getLabelFont().size)
        border = JBUI.Borders.empty(2, 12, 2, 0)
        isOpaque = false
        alignmentX = Component.LEFT_ALIGNMENT
    }

    override fun dispose() {
        sessionSubscription?.let { Disposer.dispose(it) }
        session = null
    }
}
