package dev.darkpyonix.intellij.json

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class JsonTest {
    @Test
    fun `parses the JSON value kinds`() {
        val v = Json.parse("""{"a":1,"b":-2.5,"c":"x\né","d":[true,false,null],"e":{},"f":1e3}""").jsonObject()!!
        assertEquals(1L, v["a"])
        assertEquals(-2.5, v["b"])
        assertEquals("x\né", v["c"])
        assertEquals(listOf(true, false, null), v["d"])
        assertEquals(emptyMap<String, Any?>(), v["e"])
        assertEquals(1000.0, v["f"])
        assertEquals(listOf("a", "b", "c", "d", "e", "f"), v.keys.toList())
    }

    @Test
    fun `rejects invalid text`() {
        for (bad in listOf("", "1fr", "{", "[1,]", "\"a", "01", "{\"a\" 1}", "true false")) {
            assertNull(bad, Json.parseOrNull(bad))
        }
    }

    @Test
    fun `round-trips through stringify`() {
        val value = linkedMapOf<String, Any?>(
            "s" to "q\"\\\t\u0001", "n" to 3L, "d" to 0.5, "l" to listOf(1L, "two", null), "m" to mapOf("k" to false),
        )
        assertEquals(value, Json.parse(Json.stringify(value)))
        assertEquals("""{"base_version":3,"source":"x\n"}""", Json.stringify(linkedMapOf("base_version" to 3L, "source" to "x\n")))
    }
}
