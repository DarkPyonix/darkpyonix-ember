package dev.darkpyonix.intellij.protocol

import org.junit.Assert.assertEquals
import org.junit.Test

class SseParserTest {
    private fun parse(vararg lines: String): Pair<List<SseMessage>, SseParser> {
        val out = ArrayList<SseMessage>()
        val p = SseParser { out += it }
        lines.forEach(p::feed)
        return out to p
    }

    @Test
    fun `parses id, event and data`() {
        val (msgs, p) = parse(
            "id: 1043", "event: output",
            "data: {\"run_id\":\"20261003-142233-a1f0\",\"index\":3}", "",
        )
        assertEquals(listOf(SseMessage("1043", "output", "{\"run_id\":\"20261003-142233-a1f0\",\"index\":3}")), msgs)
        assertEquals("1043", p.lastEventId)
    }

    @Test
    fun `joins multi-line data and ignores comments`() {
        val (msgs, _) = parse(": keep-alive", "data: a", "data:b", "", "")
        assertEquals(listOf(SseMessage(null, "message", "a\nb")), msgs)
    }

    @Test
    fun `a message without id keeps the last event id`() {
        val (msgs, p) = parse("id: 7", "event: output", "data: {}", "", "event: replay_truncated", "data: {\"oldest_seq\":9}", "")
        assertEquals(2, msgs.size)
        assertEquals(null, msgs[1].id)
        assertEquals("replay_truncated", msgs[1].event)
        assertEquals("7", p.lastEventId)
    }

    @Test
    fun `a block without data dispatches nothing and resets the event name`() {
        val (msgs, _) = parse("event: x", "", "data: y", "")
        assertEquals(listOf(SseMessage(null, "message", "y")), msgs)
    }

    @Test
    fun `retry is recorded`() {
        val (_, p) = parse("retry: 2500", "")
        assertEquals(2500L, p.retryMillis)
    }
}
