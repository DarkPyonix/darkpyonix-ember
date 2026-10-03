package dev.darkpyonix.intellij.editor

import com.intellij.openapi.editor.event.EditorFactoryEvent
import com.intellij.openapi.editor.event.EditorFactoryListener
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.util.Key
import dev.darkpyonix.intellij.notebook.NotebookFiles
import dev.darkpyonix.intellij.notebook.NotebookService

/** Attaches a [CellDecorator] to every editor that shows a DarkPyonix notebook. */
class NotebookEditorFactoryListener : EditorFactoryListener {
    companion object {
        private val DECORATOR = Key.create<CellDecorator>("dev.darkpyonix.intellij.decorator")
    }

    override fun editorCreated(event: EditorFactoryEvent) {
        val editor = event.editor
        val project = editor.project ?: return
        val file = FileDocumentManager.getInstance().getFile(editor.document) ?: return
        if (!NotebookFiles.isNotebook(file, editor.document.charsSequence)) return
        val session = NotebookService.getInstance(project).getOrCreate(file, editor.document)
        val decorator = CellDecorator(editor, session)
        Disposer.register(session, decorator)
        editor.putUserData(DECORATOR, decorator)
        session.editorOpened()
    }

    override fun editorReleased(event: EditorFactoryEvent) {
        val editor = event.editor
        val decorator = editor.getUserData(DECORATOR) ?: return
        editor.putUserData(DECORATOR, null)
        val session = decorator.session
        Disposer.dispose(decorator)
        session.editorClosed()
    }
}
