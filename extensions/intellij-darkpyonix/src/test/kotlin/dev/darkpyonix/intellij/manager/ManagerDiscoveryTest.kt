package dev.darkpyonix.intellij.manager

import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.util.concurrent.TimeUnit

class ManagerDiscoveryTest {
    private val identity = ClientIdentity("ij-test-client-1", "test")
    private val home: Path = Files.createTempDirectory("dpx-home")
    private val started = ArrayList<Process>()

    @After
    fun cleanUp() {
        started.forEach { it.destroyForcibly() }
        home.toFile().deleteRecursively()
    }

    private fun posix() = !System.getProperty("os.name").startsWith("Windows")

    private fun sh(script: String) = listOf("/bin/sh", "-c", script)

    @Test
    fun `FR-C1 settings URL and token are used before the environment`() {
        val d = ManagerDiscovery(home, mapOf(ManagerDiscovery.ENV_URL to "http://env", ManagerDiscovery.ENV_TOKEN to "e"))
        assertEquals(ManagerEndpoint("http://set", "s"), d.configured("http://set", "s"))
        assertEquals(ManagerEndpoint("http://env", "e"), d.configured("", null))
    }

    @Test
    fun `FR-C1 no settings and no environment means spawn`() {
        assertNull(ManagerDiscovery(home, emptyMap()).configured(null, " "))
    }

    @Test
    fun `FR-C1 only one of URL and token is an error`() {
        for ((d, url, token) in listOf(
            Triple(ManagerDiscovery(home, emptyMap()), "http://set", ""),
            Triple(ManagerDiscovery(home, emptyMap()), "", "s"),
            Triple(ManagerDiscovery(home, mapOf(ManagerDiscovery.ENV_URL to "http://env")), null, null),
            Triple(ManagerDiscovery(home, mapOf(ManagerDiscovery.ENV_TOKEN to "e")), null, null),
        )) {
            try {
                d.configured(url, token)
                fail("expected an error for url=$url token=$token")
            } catch (e: IOException) {
                assertTrue(e.message!!.contains("only one"))
            }
        }
    }

    @Test
    fun `FR-C1 the announcement line carries url, token and pid`() {
        assertEquals(
            ManagerAnnouncement("http://127.0.0.1:46881", "t0k", 42),
            ManagerAnnouncement.parse("""{"url":"http://127.0.0.1:46881","token":"t0k","pid":42}""" + "\n"),
        )
        assertNull(ManagerAnnouncement.parse("starting manager"))
        assertNull(ManagerAnnouncement.parse("""{"url":"http://x","pid":1}"""))
    }

    @Test
    fun `FR-C1 spawned manager is read from its first stdout line and kept running`() {
        assumeTrue(posix())
        val s = ManagerDiscovery(home, emptyMap()).spawnEphemeral(
            sh("""echo '{"url":"http://127.0.0.1:1","token":"tok","pid":'$$'}'; exec sleep 30"""),
            identity,
        )
        started += s.process
        assertEquals("http://127.0.0.1:1", s.announcement.url)
        assertEquals("tok", s.announcement.token)
        assertEquals(s.process.pid(), s.announcement.pid)
        assertEquals("http://127.0.0.1:1", s.client.baseUrl)
        assertTrue(s.process.isAlive)
        // No registry files (INTENT D4).
        assertFalse(Files.exists(home.resolve("managers")))
    }

    @Test
    fun `FR-C1 stdout after the first line is drained so the manager never blocks`() {
        assumeTrue(posix())
        // 2 MB after the announcement: far more than a pipe buffer.
        val s = ManagerDiscovery(home, emptyMap()).spawnEphemeral(
            sh("""echo '{"url":"http://127.0.0.1:1","token":"tok","pid":1}'; head -c 2000000 /dev/zero; exit 0"""),
            identity,
        )
        started += s.process
        assertTrue("the child blocked on a full stdout pipe", s.process.waitFor(20, TimeUnit.SECONDS))
    }

    @Test
    fun `FR-C1 stderr goes to manager log and the token stays out of it`() {
        assumeTrue(posix())
        val s = ManagerDiscovery(home, emptyMap()).spawnEphemeral(
            sh("""echo 'manager notice' >&2; echo '{"url":"http://127.0.0.1:1","token":"secret-token","pid":1}'; exec sleep 30"""),
            identity,
        )
        started += s.process
        val log = home.resolve("manager.log")
        val deadline = System.currentTimeMillis() + 5_000
        while (!Files.readString(log).contains("manager notice") && System.currentTimeMillis() < deadline) Thread.sleep(20)
        val text = Files.readString(log)
        assertTrue(text.contains("manager notice"))
        assertFalse(text.contains("secret-token"))
        assertEquals("rw-------", java.nio.file.attribute.PosixFilePermissions.toString(Files.getPosixFilePermissions(log)))
    }

    @Test
    fun `FR-C1 spawn failure is reported with the end of the manager log`() {
        assumeTrue(posix())
        try {
            ManagerDiscovery(home, emptyMap()).spawnEphemeral(sh("echo 'port in use' >&2; exit 3"), identity)
            fail("expected an error")
        } catch (e: IOException) {
            assertTrue(e.message, e.message!!.contains("exited with 3"))
            assertTrue(e.message, e.message!!.contains("port in use"))
        }
    }

    @Test
    fun `FR-C1 a manager that prints nothing times out and is stopped`() {
        assumeTrue(posix())
        try {
            ManagerDiscovery(home, emptyMap()).spawnEphemeral(sh("exec sleep 30"), identity, timeoutMs = 500)
            fail("expected an error")
        } catch (e: IOException) {
            assertTrue(e.message, e.message!!.contains("printed no address"))
        }
    }

    @Test
    fun `FR-C1 a missing CLI is reported with a hint`() {
        try {
            ManagerDiscovery(home, emptyMap()).spawnEphemeral(listOf(home.resolve("no-such-darkpyonix").toString(), "manager"), identity)
            fail("expected an error")
        } catch (e: IOException) {
            assertTrue(e.message, e.message!!.contains("Install the darkpyonix CLI"))
        }
    }
}
