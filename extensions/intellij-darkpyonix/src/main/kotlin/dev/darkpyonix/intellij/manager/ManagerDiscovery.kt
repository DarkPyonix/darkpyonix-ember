package dev.darkpyonix.intellij.manager

import dev.darkpyonix.intellij.json.Json
import dev.darkpyonix.intellij.json.jsonObject
import dev.darkpyonix.intellij.json.long
import dev.darkpyonix.intellij.json.str
import java.io.BufferedReader
import java.io.File
import java.io.IOException
import java.io.InputStreamReader
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.attribute.PosixFilePermissions
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/** A manager's address and token: what the CLI reads from env or from the spawned manager. */
data class ManagerEndpoint(val url: String, val token: String)

/** The first stdout line of `darkpyonix manager`: `{url, token, pid}` (SPEC FR-C1, FR-M3). */
data class ManagerAnnouncement(val url: String, val token: String, val pid: Long) {
    companion object {
        fun parse(line: String): ManagerAnnouncement? {
            val m = Json.parseOrNull(line.trim()).jsonObject() ?: return null
            return ManagerAnnouncement(
                url = m.str("url")?.takeIf { it.isNotEmpty() } ?: return null,
                token = m.str("token")?.takeIf { it.isNotEmpty() } ?: return null,
                pid = m.long("pid") ?: return null,
            )
        }
    }
}

/** A manager this IDE started; [process] stays alive for the IDE session. */
class SpawnedManager(val announcement: ManagerAnnouncement, val process: Process, val client: ManagerClient)

/**
 * Finding a manager the way the `darkpyonix` CLI does (SPEC FR-C1, INTENT D4). There are no
 * registry files: a given manager comes from the settings or from `DARKPYONIX_MANAGER_URL` +
 * `DARKPYONIX_TOKEN`; otherwise the IDE spawns `darkpyonix manager --ephemeral` as its child
 * and reads `{url, token, pid}` from the first stdout line. Kernels are found by every manager
 * (FR-M5), so any manager reaches the same kernel.
 */
class ManagerDiscovery(
    val home: Path = defaultHome(),
    private val env: Map<String, String> = System.getenv(),
) {
    companion object {
        const val ENV_URL = "DARKPYONIX_MANAGER_URL"
        const val ENV_TOKEN = "DARKPYONIX_TOKEN"
        const val SPAWN_TIMEOUT_MS = 15_000L

        /** `DARKPYONIX_HOME`, else `~/.darkpyonix`. */
        fun defaultHome(): Path {
            System.getenv("DARKPYONIX_HOME")?.takeIf { it.isNotEmpty() }?.let { return Paths.get(it) }
            return Paths.get(System.getProperty("user.home"), ".darkpyonix")
        }
    }

    /** `DARKPYONIX_HOME/manager.log`: the spawned manager's stderr, as the CLI keeps it (FR-C1). */
    val logFile: Path get() = home.resolve("manager.log")

    /**
     * The given manager: the settings pair first, else the environment pair, else null (spawn).
     * Only one half of a pair is an error, like the CLI (FR-C1).
     */
    fun configured(settingsUrl: String?, settingsToken: String?): ManagerEndpoint? {
        pair(settingsUrl, settingsToken, "the DarkPyonix settings (manager URL and token)")?.let { return it }
        return pair(env[ENV_URL], env[ENV_TOKEN], "$ENV_URL and $ENV_TOKEN")
    }

    private fun pair(url: String?, token: String?, what: String): ManagerEndpoint? {
        val u = url?.trim().orEmpty()
        val t = token?.trim().orEmpty()
        if (u.isEmpty() && t.isEmpty()) return null
        if (u.isEmpty() || t.isEmpty()) throw IOException("Set both or neither of $what: only one is set.")
        return ManagerEndpoint(u, t)
    }

    /**
     * Starts [command] (`darkpyonix manager --ephemeral`) and reads its first stdout line. The
     * rest of stdout is drained on a daemon thread so the manager never blocks on a full pipe;
     * stderr is appended to [logFile]. The token only travels through the stdout pipe.
     */
    fun spawnEphemeral(command: List<String>, identity: ClientIdentity, timeoutMs: Long = SPAWN_TIMEOUT_MS): SpawnedManager {
        Files.createDirectories(home)
        val log = logFile.toFile()
        if (!log.exists()) log.createNewFile()
        restrict(logFile)
        val pb = ProcessBuilder(command)
            .redirectInput(ProcessBuilder.Redirect.from(nullFile()))
            .redirectOutput(ProcessBuilder.Redirect.PIPE)
            .redirectError(ProcessBuilder.Redirect.appendTo(log))
        pb.environment()["DARKPYONIX_HOME"] = home.toString()
        // The child is the manager; it must not hand itself a manager to reuse.
        pb.environment().remove(ENV_URL)
        pb.environment().remove(ENV_TOKEN)
        val child = try {
            pb.start()
        } catch (e: IOException) {
            throw IOException(
                "Cannot start `${command.joinToString(" ")}`: ${e.message}. " +
                    "Install the darkpyonix CLI or set its path in Settings | Tools | DarkPyonix.",
                e,
            )
        }
        val first = CompletableFuture<String?>()
        Thread({
            try {
                val reader = BufferedReader(InputStreamReader(child.inputStream, StandardCharsets.UTF_8))
                first.complete(reader.readLine())
                val buf = CharArray(8192)
                while (reader.read(buf) >= 0) {
                    // Drained and dropped: the manager writes nothing else we need.
                }
            } catch (e: IOException) {
                first.complete(null)
            }
        }, "DarkPyonix manager stdout").apply { isDaemon = true }.start()

        val line = try {
            first.get(timeoutMs, TimeUnit.MILLISECONDS)
        } catch (e: TimeoutException) {
            child.destroy()
            throw IOException("`${command.joinToString(" ")}` printed no address within ${timeoutMs / 1000} s${logTail(log)}")
        } catch (e: ExecutionException) {
            null
        }
        val ann = line?.let { ManagerAnnouncement.parse(it) }
        if (ann == null) {
            val exited = child.waitFor(2, TimeUnit.SECONDS)
            if (!exited) child.destroy()
            val why = if (exited) "exited with ${child.exitValue()}" else "printed no {url, token, pid} line"
            throw IOException("`${command.joinToString(" ")}` $why${logTail(log)}")
        }
        return SpawnedManager(ann, child, ManagerClient(ann.url, ann.token, identity))
    }

    private fun restrict(path: Path) {
        try {
            Files.setPosixFilePermissions(path, PosixFilePermissions.fromString("rw-------"))
        } catch (_: UnsupportedOperationException) {
        } catch (_: IOException) {
        }
    }

    private fun nullFile() = File(if (System.getProperty("os.name").startsWith("Windows")) "NUL" else "/dev/null")

    private fun logTail(log: File): String = try {
        val lines = log.readLines().takeLast(5)
        if (lines.isEmpty()) "" else "\n" + lines.joinToString("\n")
    } catch (_: IOException) {
        ""
    }
}
