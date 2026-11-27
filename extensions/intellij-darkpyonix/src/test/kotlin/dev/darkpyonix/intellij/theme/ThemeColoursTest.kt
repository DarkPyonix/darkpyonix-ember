package dev.darkpyonix.intellij.theme

import dev.darkpyonix.intellij.json.Json
import dev.darkpyonix.intellij.json.jsonList
import dev.darkpyonix.intellij.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import javax.xml.parsers.DocumentBuilderFactory

/**
 * FR-W6a: the IntelliJ DarkPyonix themes take every colour from the VS Code theme of the same
 * name, so there is one palette and no colour chosen for JetBrains alone.
 *
 * - A named colour in the `.theme.json` is called after the VS Code key it copies, and has that
 *   key's value.
 * - The `ui` section names only those named colours: no colour written out in it.
 * - Every colour of the `.icls` editor scheme is a colour the VS Code theme has.
 */
class ThemeColoursTest {

    private val variants = listOf("Dark", "Light")

    /** The VS Code theme beside this plugin in the repository; tests run in the plugin's directory. */
    private fun vscode(variant: String): Map<String, Any?> {
        val file = File("../vscode-darkpyonix-theme/themes/DarkPyonix $variant-color-theme.json")
        assertTrue("no VS Code theme at ${file.absolutePath}", file.isFile)
        return Json.parse(file.readText()).jsonObject()!!
    }

    private fun resource(path: String): String =
        checkNotNull(javaClass.getResource(path)) { "no $path on the plugin's classpath" }.readText()

    private fun intellij(variant: String): Map<String, Any?> =
        Json.parse(resource("/themes/DarkPyonix$variant.theme.json")).jsonObject()!!

    /** `#RRGGBB` or `#RRGGBBAA`, upper case, with a fully opaque alpha dropped. */
    private fun normal(colour: String): String {
        val hex = colour.removePrefix("#").uppercase()
        return if (hex.length == 8 && hex.endsWith("FF")) hex.dropLast(2) else hex
    }

    /** Every colour the VS Code theme has: its workbench colours and its token colours. */
    private fun palette(theme: Map<String, Any?>): Set<String> {
        val workbench = theme["colors"].jsonObject()!!.values.map { normal(it as String) }
        val tokens = theme["tokenColors"].jsonList()!!.mapNotNull {
            (it.jsonObject()!!["settings"].jsonObject()!!["foreground"] as? String)?.let(::normal)
        }
        return (workbench + tokens).toSet()
    }

    @Test
    fun `fr_w6a every named colour is the VS Code colour it is named after`() {
        for (variant in variants) {
            val source = vscode(variant)["colors"].jsonObject()!!
            val named = intellij(variant)["colors"].jsonObject()!!
            assertTrue("$variant names no colours", named.isNotEmpty())
            for ((key, value) in named) {
                val expected = source[key] as? String
                assertTrue("$variant: $key is not a colour of the VS Code theme", expected != null)
                assertEquals("$variant: $key", normal(expected!!), normal(value as String))
            }
        }
    }

    @Test
    fun `fr_w6a the ui section names only named colours`() {
        for (variant in variants) {
            val theme = intellij(variant)
            val named = theme["colors"].jsonObject()!!.keys
            fun walk(path: String, value: Any?) {
                when (value) {
                    is Map<*, *> -> value.forEach { (k, v) -> walk("$path.$k", v) }
                    is String -> {
                        assertTrue("$variant: $path is the colour $value written out", !value.startsWith("#"))
                        assertTrue("$variant: $path names $value, which is not a named colour", value in named)
                    }
                    else -> Unit
                }
            }
            walk("ui", theme["ui"])
            assertEquals("$variant: editor scheme", "/themes/DarkPyonix$variant.icls", theme["editorScheme"])
        }
    }

    @Test
    fun `fr_w6a every editor scheme colour is a colour of the VS Code theme`() {
        for (variant in variants) {
            val palette = palette(vscode(variant))
            val xml = DocumentBuilderFactory.newInstance().newDocumentBuilder()
                .parse(resource("/themes/DarkPyonix$variant.icls").byteInputStream())
            assertEquals("DarkPyonix $variant", xml.documentElement.getAttribute("name"))
            val options = xml.getElementsByTagName("option")
            var colours = 0
            for (i in 0 until options.length) {
                val option = options.item(i) as org.w3c.dom.Element
                val name = option.getAttribute("name")
                val value = option.getAttribute("value")
                if (value.isEmpty() || name == "FONT_TYPE") continue
                colours++
                assertTrue("$variant: $name is $value, which the VS Code theme does not have", normal(value) in palette)
            }
            assertTrue("$variant: the scheme has no colours", colours > 20)
        }
    }

    @Test
    fun `fr_w6a both themes are registered with the IDE`() {
        val plugin = resource("/META-INF/plugin.xml")
        for (variant in variants) {
            assertTrue("$variant is not registered", "path=\"/themes/DarkPyonix$variant.theme.json\"" in plugin)
            assertEquals("DarkPyonix $variant", intellij(variant)["name"])
            assertEquals(variant == "Dark", intellij(variant)["dark"])
        }
    }
}
