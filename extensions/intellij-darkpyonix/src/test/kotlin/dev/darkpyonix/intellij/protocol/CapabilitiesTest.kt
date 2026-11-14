package dev.darkpyonix.intellij.protocol

import dev.darkpyonix.intellij.json.Json
import dev.darkpyonix.intellij.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CapabilitiesTest {
    private fun info(json: String) = Capabilities.fromManagerInfo(Json.parse(json).jsonObject()!!)

    @Test
    fun `FR-A3 the master token's five capabilities allow running and editing`() {
        val c = info("""{"version":"0.3.0","mode":"ephemeral","pid":1,"started_at":"t","capabilities":["read","history","execute","edit","manage"]}""")
        assertTrue(c.canRun)
        assertTrue(c.canEdit)
    }

    @Test
    fun `FR-A3 execute and edit are separate capabilities, not grades`() {
        val runOnly = info("""{"capabilities":["read","history","execute"]}""")
        assertTrue(runOnly.canRun)
        assertFalse(runOnly.canEdit)
        val editOnly = info("""{"capabilities":["read","edit"]}""")
        assertFalse(editOnly.canRun)
        assertTrue(editOnly.canEdit)
        assertFalse(editOnly.without("edit").canEdit)
    }

    @Test
    fun `FR-A3 grade names and a missing set grant nothing`() {
        val grades = info("""{"permission":"admin","capabilities":["admin","editor","viewer3"]}""")
        assertFalse(grades.canRun)
        assertFalse(grades.canEdit)
        assertEquals(emptySet<String>(), info("{}").names)
    }
}
