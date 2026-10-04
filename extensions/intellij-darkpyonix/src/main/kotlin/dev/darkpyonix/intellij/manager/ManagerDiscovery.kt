package dev.darkpyonix.intellij.manager

import dev.darkpyonix.intellij.json.Json
import dev.darkpyonix.intellij.json.jsonObject
import dev.darkpyonix.intellij.json.long
import dev.darkpyonix.intellij.json.str
import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths

/** `<DARKPYONIX_HOME>/managers/<pid>.json` (PROTOCOL §1): `url`, `token`, `mode`, `pid`, `started_at`. */
data class ManagerRecord(val url: String, val token: String, val mode: String, val pid: Long, val startedAt: String) {
    companion object {
        fun parse(text: String): ManagerRecord? {
            val m = Json.parseOrNull(text).jsonObject() ?: return null
            return ManagerRecord(
                url = m.str("url") ?: return null,
                token = m.str("token") ?: return null,
                mode = m.str("mode") ?: "ephemeral",
                pid = m.long("pid") ?: return null,
                startedAt = m.str("started_at") ?: "",
            )
        }
    }
}

/**
 * Finding a manager the way the `darkpyonix` CLI does (SPEC FR-C1, mirrored from the CLI's
 * `discovery.rs`): live registrations first (dedicated before ephemeral, then newest), else
 * spawn `darkpyonix manager --ephemeral` and wait for its registration and `/health`.
 */
class ManagerDiscovery(
    val home: Path = defaultHome(),
    private val pidAlive: (Long) -> Boolean = ::processAlive,
) {
    companion object {
        /** `DARKPYONIX_HOME`, else `~/.darkpyonix`. */
        fun defaultHome(): Path {
            System.getenv("DARKPYONIX_HOME")?.takeIf { it.isNotEmpty() }?.let { return Paths.get(it) }
            return Paths.get(System.getProperty("user.home"), ".darkpyonix")
        }

        fun processAlive(pid: Long): Boolean =
            ProcessHandle.of(pid).map { it.isAlive }.orElse(false)

        const val SPAWN_TIMEOUT_MS = 10_000L
    }

    private val managersDir: Path get() = home.resolve("managers")

    /** Registered managers whose process is alive, best first. Stale files are left alone. */
    fun liveRecords(): List<ManagerRecord> {
        val dir = managersDir.toFile()
        val files = dir.listFiles { f -> f.isFile && f.name.endsWith(".json") } ?: return emptyList()
        return files.mapNotNull { f ->
            try {
                ManagerRecord.parse(f.readText())
            } catch (_: IOException) {
                null
            }
        }
            .filter { pidAlive(it.pid) }
            .sortedWith(compareByDescending<ManagerRecord> { it.mode == "dedicated" }.thenByDescending { it.startedAt })
    }

    /** The first registered manager that answers `/health`, or null. Never spawns. */
    fun findLive(identity: ClientIdentity): ManagerClient? =
        liveRecords().asSequence().map { ManagerClient(it.url, it.token, identity) }.firstOrNull { it.health() }

    /**
     * Starts `<command> manager --ephemeral` detached from the IDE (its own process; stdout
     * discarded, stderr to `managers/spawn.log`), then waits for `managers/<pid>.json`.
     */
    fun spawnEphemeral(command: List<String>, identity: ClientIdentity): ManagerClient {
        Files.createDirectories(managersDir)
        val log = managersDir.resolve("spawn.log").toFile()
        val pb = ProcessBuilder(command)
            .redirectInput(ProcessBuilder.Redirect.from(nullFile()))
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
            .redirectError(ProcessBuilder.Redirect.to(log))
        pb.environment()["DARKPYONIX_HOME"] = home.toString()
        val child = try {
            pb.start()
        } catch (e: IOException) {
            throw IOException(
                "Cannot start `${command.joinToString(" ")}`: ${e.message}. " +
                    "Install the darkpyonix CLI or set its path in Settings | Tools | DarkPyonix.",
                e,
            )
        }
        val reg = managersDir.resolve("${child.pid()}.json").toFile()
        val deadline = System.currentTimeMillis() + SPAWN_TIMEOUT_MS
        while (true) {
            if (!child.isAlive) {
                throw IOException("`${command.joinToString(" ")}` exited with ${child.exitValue()}${logTail(log)}")
            }
            if (reg.isFile) {
                val rec = try {
                    ManagerRecord.parse(reg.readText())
                } catch (_: IOException) {
                    null
                }
                if (rec != null) {
                    val client = ManagerClient(rec.url, rec.token, identity)
                    if (client.health()) return client
                }
            }
            if (System.currentTimeMillis() > deadline) {
                throw IOException("No healthy registration at $reg within ${SPAWN_TIMEOUT_MS / 1000} s${logTail(log)}")
            }
            Thread.sleep(50)
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
