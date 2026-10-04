package dev.darkpyonix.intellij.protocol

/** One Server-Sent Events message. [id] is null when the message carried no `id` field. */
data class SseMessage(val id: String?, val event: String, val data: String)

/**
 * Line-driven Server-Sent Events parser (WHATWG HTML §9.2.6), fed one line at a time without
 * its line terminator. [lastEventId] follows the spec: a message without `id` keeps the previous
 * value, which is what the manager relies on for `replay_truncated` (manager.openapi.yaml,
 * `streamEvents`).
 */
class SseParser(private val onMessage: (SseMessage) -> Unit) {
    private val data = StringBuilder()
    private var event = ""
    private var id: String? = null
    private var hasData = false

    var lastEventId: String? = null
        private set

    var retryMillis: Long? = null
        private set

    fun feed(line: String) {
        if (line.isEmpty()) {
            dispatch()
            return
        }
        if (line.startsWith(":")) return // comment / keep-alive
        val colon = line.indexOf(':')
        val field: String
        var value: String
        if (colon < 0) {
            field = line
            value = ""
        } else {
            field = line.substring(0, colon)
            value = line.substring(colon + 1)
            if (value.startsWith(" ")) value = value.substring(1)
        }
        when (field) {
            "data" -> {
                if (hasData) data.append('\n')
                data.append(value)
                hasData = true
            }
            "event" -> event = value
            "id" -> if (!value.contains('\u0000')) {
                id = value
                lastEventId = value
            }
            "retry" -> value.toLongOrNull()?.let { retryMillis = it }
        }
    }

    /** Ends the stream: an unterminated message is discarded, as the spec requires. */
    fun reset() {
        data.setLength(0)
        event = ""
        id = null
        hasData = false
    }

    private fun dispatch() {
        if (!hasData) {
            event = ""
            id = null
            return
        }
        val msg = SseMessage(id, event.ifEmpty { "message" }, data.toString())
        reset()
        onMessage(msg)
    }
}
