package dev.darkpyonix.intellij.settings

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.BaseState
import com.intellij.openapi.components.Service
import com.intellij.openapi.components.SimplePersistentStateComponent
import com.intellij.openapi.components.State
import com.intellij.openapi.components.Storage
import dev.darkpyonix.intellij.manager.ClientIdentity
import java.net.InetAddress
import java.util.UUID

/** Application settings (Settings | Tools | DarkPyonix). */
@Service(Service.Level.APP)
@State(name = "DarkPyonixSettings", storages = [Storage("darkpyonix.xml")])
class DarkPyonixSettings : SimplePersistentStateComponent<DarkPyonixSettings.Settings>(Settings()) {
    class Settings : BaseState() {
        /** The darkpyonix CLI (a full path when the IDE's PATH does not contain it). */
        var command by string("darkpyonix")

        /** Arguments that start an ephemeral manager (FR-C1). */
        var spawnArgs by string("manager --ephemeral")

        /** A dedicated manager to use instead of discovery, e.g. https://main.example.darkpyonix.dev. */
        var managerUrl by string("")

        /** Token for [managerUrl] (master or share token). */
        var managerToken by string("")

        /** Interpreter for new kernels; empty = the manager's default. */
        var python by string("")

        /** Device name shown to other clients (FR-S4). */
        var nickname by string("")

        /** Stable per-installation client id (`X-DarkPyonix-Client`). */
        var clientId by string("")

        /** Queue a run when the kernel is busy instead of refusing it (`on_busy`). */
        var queueWhenBusy by property(true)

        /** Attach to an already running kernel when a notebook is opened (never starts one). */
        var attachOnOpen by property(true)
    }

    companion object {
        fun getInstance(): DarkPyonixSettings =
            ApplicationManager.getApplication().getService(DarkPyonixSettings::class.java)
    }

    fun identity(): ClientIdentity {
        val s = state
        var id = s.clientId
        if (id.isNullOrBlank() || !Regex("^[A-Za-z0-9_-]{8,64}$").matches(id)) {
            id = "ij-" + UUID.randomUUID().toString().replace("-", "").take(20)
            s.clientId = id
        }
        val nick = s.nickname?.takeIf { it.isNotBlank() } ?: defaultNickname()
        return ClientIdentity(id, nick.take(64))
    }

    fun spawnCommand(): List<String> {
        val cmd = state.command?.takeIf { it.isNotBlank() } ?: "darkpyonix"
        val args = (state.spawnArgs ?: "manager --ephemeral").trim().split(Regex("\\s+")).filter { it.isNotEmpty() }
        return listOf(cmd) + args
    }

    private fun defaultNickname(): String = try {
        "IntelliJ@" + InetAddress.getLocalHost().hostName.substringBefore('.')
    } catch (_: Exception) {
        "IntelliJ"
    }
}
