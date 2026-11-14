package dev.darkpyonix.intellij.manager

import dev.darkpyonix.intellij.json.long
import dev.darkpyonix.intellij.json.str
import dev.darkpyonix.intellij.protocol.DocumentState
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.net.URI
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.CopyOnWriteArrayList

/**
 * The plugin's manager client, event stream and document state against a REAL `darkpyonix`
 * manager (core issue #22). CI builds the manager from a pinned core commit and sets
 * `DARKPYONIX_BIN` to the binary; without it the test is skipped. Every process it starts
 * (manager and kernel) is killed at the end.
 */
class RealManagerIntegrationTest {
    private val bin: String? = System.getenv("DARKPYONIX_BIN")?.takeIf { it.isNotBlank() }
    private val identity = ClientIdentity("ij-it-client-01", "intellij-it")
    private lateinit var work: Path
    private var spawned: SpawnedManager? = null
    private var client: ManagerClient? = null
    private var kernelId: String? = null
    private var kernelPid: Long? = null
    private var stream: EventStream? = null

    @After
    fun cleanUp() {
        stream?.close()
        val c = client
        val kid = kernelId
        if (c != null && kid != null) {
            try {
                // Test cleanup only: the explicitly named kill operation.
                val req = HttpRequest.newBuilder(URI.create(c.baseUrl.trimEnd('/') + "/api/kernels/$kid?force=true"))
                    .header("Authorization", "Bearer ${spawned!!.announcement.token}")
                    .DELETE().build()
                c.http.send(req, HttpResponse.BodyHandlers.discarding())
            } catch (_: Exception) {
            }
        }
        kernelPid?.let { pid -> ProcessHandle.of(pid).ifPresent { it.destroyForcibly() } }
        spawned?.process?.let { p ->
            p.descendants().forEach { it.destroyForcibly() }
            p.destroyForcibly()
        }
        if (::work.isInitialized) work.toFile().deleteRecursively()
    }

    private fun waitFor(what: String, timeoutMs: Long = 30_000, cond: () -> Boolean) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (!cond()) {
            if (System.currentTimeMillis() > deadline) throw AssertionError("timed out waiting for $what")
            Thread.sleep(50)
        }
    }

    @Test
    fun `issue 22 run, live outputs, interrupt and stale latest outputs against the real manager`() {
        assumeTrue("DARKPYONIX_BIN is not set; skipping the real-manager test", bin != null)
        work = Files.createTempDirectory("dpx-it")
        val home = Files.createDirectories(work.resolve("home"))
        val nb = work.resolve("notebook.py")
        Files.writeString(
            nb,
            "import time\n\n# %%\nprint(\"hello from darkpyonix\")\n\n# %%\nfor i in range(600):\n    time.sleep(0.1)\n",
        )

        // FR-C1: spawned like the CLI does, address and token from the first stdout line.
        val s = ManagerDiscovery(home, emptyMap()).spawnEphemeral(listOf(bin!!, "manager", "--ephemeral"), identity)
        spawned = s
        val c = s.client
        client = c
        assertTrue(c.health())
        assertFalse("no registry files (INTENT D4)", Files.exists(home.resolve("managers")))
        val caps = dev.darkpyonix.intellij.protocol.Capabilities.fromManagerInfo(c.managerInfo())
        assertTrue("master token runs (FR-A3): $caps", caps.canRun)
        assertTrue("master token edits (FR-A3): $caps", caps.canEdit)

        // FR-M2: start the file's kernel.
        val k = c.startKernel(nb.toRealPath().toString())
        val kid = k.str("kernel_id")!!
        kernelId = kid
        kernelPid = k.long("pid")

        // FR-R4 snapshot, then live events from its seq.
        val state = DocumentState()
        state.loadSnapshot(c.document(kid))
        assertEquals(3, state.cells.size)
        val events = CopyOnWriteArrayList<Pair<String, Map<String, Any?>>>()
        stream = EventStream(c, kid, state.subscribeSince, { seq, type, data ->
            events += type to data
            state.apply(seq, type, data)
        }, { _, _ -> }).also { it.start() }

        // Run cell 1: its output arrives over SSE.
        c.startRun(kid, cells = listOf(1))
        waitFor("cell 1 output over SSE") {
            state.cellAt(1)?.outputs?.any { (it["text"] as? String)?.contains("hello from darkpyonix") == true } == true
        }
        waitFor("run 1 finished") { events.any { it.first == "run.finished" && it.second.str("status") == "ok" } }

        // Interrupt a slow cell: the run ends interrupted and the kernel survives.
        c.startRun(kid, cells = listOf(2))
        waitFor("cell 2 started") { events.any { it.first == "cell.started" && it.second.long("index") == 2L } }
        c.interrupt(kid)
        waitFor("run 2 interrupted") { events.any { it.first == "run.finished" && it.second.str("status") == "interrupted" } }
        val after = c.getKernel(kid)
        assertNotNull("the kernel survives an interrupt", after)
        assertEquals(kernelPid, after!!.long("pid"))
        val before = events.count { it.first == "run.finished" && it.second.str("status") == "ok" }
        c.startRun(kid, cells = listOf(1))
        waitFor("a run after the interrupt") { events.count { it.first == "run.finished" && it.second.str("status") == "ok" } > before }

        // Latest outputs per cell, then stale after an edit (FR-R4, FR-S2).
        val fresh = DocumentState().apply { loadSnapshot(c.document(kid)) }
        val cell1 = fresh.cellAt(1)!!
        assertTrue(cell1.outputs.any { (it["text"] as? String)?.contains("hello from darkpyonix") == true })
        assertFalse(cell1.stale)
        c.updateCell(kid, cell1.cellId, cell1.version, source = "print(\"changed\")\n\n")
        waitFor("cell 1 marked stale after the edit", 10_000) {
            DocumentState().apply { loadSnapshot(c.document(kid)) }.cellAt(1)?.stale == true
        }
        val edited = DocumentState().apply { loadSnapshot(c.document(kid)) }.cellAt(1)!!
        assertTrue("stale outputs are still shown", edited.outputs.isNotEmpty())
    }
}
