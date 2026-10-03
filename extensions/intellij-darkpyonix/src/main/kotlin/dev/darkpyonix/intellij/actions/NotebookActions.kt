package dev.darkpyonix.intellij.actions

import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.fileEditor.FileEditorManager
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.ui.Messages
import dev.darkpyonix.intellij.notebook.NotebookService
import dev.darkpyonix.intellij.notebook.NotebookSession

/** Base for actions on the notebook in the current editor (or the selected one). */
abstract class NotebookAction : DumbAwareAction() {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    protected fun session(e: AnActionEvent): NotebookSession? {
        val project = e.project ?: return null
        val file = e.getData(CommonDataKeys.VIRTUAL_FILE)
            ?: FileEditorManager.getInstance(project).selectedFiles.firstOrNull()
        return NotebookService.getInstance(project).session(file)
    }

    override fun update(e: AnActionEvent) {
        e.presentation.isEnabledAndVisible = session(e) != null
    }
}

class RunCellAction : NotebookAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val session = session(e) ?: return
        val editor = e.getData(CommonDataKeys.EDITOR)
            ?: e.project?.let { FileEditorManager.getInstance(it).selectedTextEditor }
            ?: return
        session.run(listOf(session.positionAt(editor.caretModel.offset)))
    }
}

class RunAllAction : NotebookAction() {
    override fun actionPerformed(e: AnActionEvent) {
        session(e)?.run(null)
    }
}

class InterruptAction : NotebookAction() {
    override fun actionPerformed(e: AnActionEvent) {
        session(e)?.interrupt()
    }
}

class ConnectAction : NotebookAction() {
    override fun actionPerformed(e: AnActionEvent) {
        session(e)?.connectAsync(startKernel = true)
    }
}

class RestartKernelAction : NotebookAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val session = session(e) ?: return
        val answer = Messages.showYesNoCancelDialog(
            e.project,
            "Soft restart clears the namespace; hard restart re-executes the kernel process.",
            "Restart DarkPyonix Kernel",
            "Soft Restart",
            "Hard Restart",
            "Cancel",
            Messages.getQuestionIcon(),
        )
        when (answer) {
            Messages.YES -> session.restart(hard = false)
            Messages.NO -> session.restart(hard = true)
        }
    }
}
