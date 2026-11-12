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
import dev.darkpyonix.intellij.manager.SpawnedManager
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

        private val spawnLock = Any()

        /** The ephemeral manager this IDE process started; it ends on its own idle timeout. */
        @Volatile
        private var spawned: SpawnedManager? = null
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
     * The manager to talk to (SPEC FR-C1, like the CLI): the settings URL + token, else
     * `DARKPYONIX_MANAGER_URL` + `DARKPYONIX_TOKEN`, else the ephemeral manager this IDE spawned
     * (spawned on first use when [spawn]; one per IDE process, shared by every project).
     * Blocking.
     */
    fun managerClient(spawn: Boolean): ManagerClient? {
        val settings = DarkPyonixSettings.getInstance()
        val identity = settings.identity()
        synchronized(managerLock) {
            manager?.let { if (it.health()) return it }
            manager = null
            val discovery = ManagerDiscovery()
            val given = discovery.configured(settings.state.managerUrl, settings.state.managerToken)
            if (given != null) {
                val c = ManagerClient(given.url, given.token, identity)
                if (!c.health()) throw IOException("The manager at ${given.url} does not answer /health.")
                manager = c
                return c
            }
            synchronized(spawnLock) {
                spawned?.let {
                    val c = ManagerClient(it.announcement.url, it.announcement.token, identity)
                    if (it.process.isAlive && c.health()) {
                        manager = c
                        return c
                    }
                }
                spawned = null
                if (!spawn) return null
                val s = discovery.spawnEphemeral(settings.spawnCommand(), identity)
                spawned = s
                manager = s.client
                return s.client
            }
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
