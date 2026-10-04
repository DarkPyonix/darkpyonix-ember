package dev.darkpyonix.intellij.notebook

import com.intellij.notification.NotificationAction
import com.intellij.notification.NotificationGroupManager
import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.components.Service
import com.intellij.openapi.editor.Document
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.vfs.VirtualFile
import dev.darkpyonix.intellij.format.NotebookFormat
import dev.darkpyonix.intellij.manager.ManagerClient
import dev.darkpyonix.intellij.manager.ManagerDiscovery
import dev.darkpyonix.intellij.settings.DarkPyonixSettings
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArrayList

object NotebookFiles {
    /** `.pynb` always; `.py` when it has at least one `# %%` marker (FORMAT §1.3). */
    fun isNotebook(file: VirtualFile?, text: CharSequence?): Boolean {
        if (file == null || file.isDirectory) return false
        return when (file.extension?.lowercase()) {
            "pynb" -> true
            "py" -> text != null && NotebookFormat.hasMarkers(text)
            else -> false
        }
    }
}

/** Per-project registry of open notebook sessions and the manager connection they share. */
@Service(Service.Level.PROJECT)
class NotebookService(val project: Project) : Disposable {
    private val sessions = ConcurrentHashMap<String, NotebookSession>()
    private val sessionListeners = CopyOnWriteArrayList<() -> Unit>()
    private val managerLock = Any()

    @Volatile
    private var manager: ManagerClient? = null

    companion object {
        fun getInstance(project: Project): NotebookService = project.getService(NotebookService::class.java)

        const val NOTIFICATION_GROUP = "DarkPyonix"
    }

    fun session(file: VirtualFile?): NotebookSession? = file?.let { sessions[it.path] }

    fun allSessions(): Collection<NotebookSession> = sessions.values

    fun getOrCreate(file: VirtualFile, document: Document): NotebookSession {
        sessions[file.path]?.let { return it }
        val created = synchronized(sessions) {
            sessions[file.path] ?: NotebookSession(project, this, file, document).also {
                sessions[file.path] = it
                Disposer.register(this, it)
            }
        }
        sessionListeners.forEach { it() }
        return created
    }

    internal fun sessionClosed(session: NotebookSession) {
        sessions.remove(session.file.path, session)
        sessionListeners.forEach { it() }
    }

    /** Called (on the EDT) when sessions are added or removed. */
    fun addSessionsListener(parent: Disposable, listener: () -> Unit) {
        sessionListeners += listener
        Disposer.register(parent) { sessionListeners -= listener }
    }

    /**
     * The manager to talk to: the configured dedicated manager, else a registered live one
     * (FR-C1). With [spawn], starts an ephemeral manager when none is registered. Blocking.
     */
    fun managerClient(spawn: Boolean): ManagerClient? {
        val settings = DarkPyonixSettings.getInstance()
        val identity = settings.identity()
        synchronized(managerLock) {
            manager?.let { if (it.health()) return it }
            manager = null
            val url = settings.state.managerUrl?.trim().orEmpty()
            if (url.isNotEmpty()) {
                val c = ManagerClient(url, settings.state.managerToken.orEmpty(), identity)
                if (c.health()) manager = c
                return manager ?: throw IOException("The dedicated manager at $url does not answer /health.")
            }
            val discovery = ManagerDiscovery()
            manager = discovery.findLive(identity)
            if (manager == null && spawn) manager = discovery.spawnEphemeral(settings.spawnCommand(), identity)
            return manager
        }
    }

    fun notify(content: String, type: NotificationType = NotificationType.INFORMATION, vararg actions: Pair<String, () -> Unit>) {
        val n = NotificationGroupManager.getInstance().getNotificationGroup(NOTIFICATION_GROUP)
            .createNotification(content, type)
        for ((text, run) in actions) n.addAction(NotificationAction.createSimpleExpiring(text) { run() })
        n.notify(project)
    }

    override fun dispose() {
        sessions.clear()
    }
}
