package dev.darkpyonix.intellij.manager

import dev.darkpyonix.intellij.json.Json
import dev.darkpyonix.intellij.json.jsonObject
import dev.darkpyonix.intellij.protocol.SseMessage
import dev.darkpyonix.intellij.protocol.SseParser
import java.io.BufferedReader
import java.io.InputStream
import java.io.InputStreamReader
import java.nio.charset.StandardCharsets

/**
 * Follows `GET /kernels/{id}/events` on a daemon thread and reconnects with `Last-Event-ID`
 * (SPEC FR-M1). Being connected with `client_id` is also what keeps this IDE in the presence
 * list (FR-S4).
 */
class EventStream(
    private val client: ManagerClient,
    private val kernelId: String,
    /** First connection: replay events after this seq (the document snapshot's `seq`). */
    private val since: Long,
    private val onEvent: (seq: Long?, type: String, data: Map<String, Any?>) -> Unit,
    private val onState: (connected: Boolean, error: Throwable?) -> Unit,
) {
    @Volatile
    private var closed = false

    @Volatile
    private var input: InputStream? = null

    private val thread = Thread(::loop, "DarkPyonix events $kernelId").apply { isDaemon = true }

    fun start() = thread.start()

    fun close() {
        closed = true
        try {
            input?.close() // cancels the HTTP exchange and unblocks readLine
        } catch (_: Exception) {
        }
        thread.interrupt()
    }

    private fun loop() {
        var lastEventId: String? = null
        var backoff = 500L
        while (!closed) {
            var parser: SseParser? = null
            try {
                val (status, body) = client.openEvents(client.eventsRequest(kernelId, lastEventId, since))
                input = body
                if (status !in 200..299) {
                    val text = body.use { String(it.readAllBytes(), StandardCharsets.UTF_8) }
                    throw ManagerException(status, "http_$status", text.take(200))
                }
                onState(true, null)
                backoff = 500L
                val p = SseParser(::dispatch)
                parser = p
                BufferedReader(InputStreamReader(body, StandardCharsets.UTF_8)).use { reader ->
                    var line = reader.readLine()
                    while (!closed && line != null) {
                        p.feed(line)
                        p.lastEventId?.let { lastEventId = it }
                        line = reader.readLine()
                    }
                }
                if (!closed) onState(false, null)
            } catch (e: Exception) {
                if (closed) break
                onState(false, e)
                if (e is ManagerException && (e.status == 401 || e.status == 403 || e.status == 404)) break
            } finally {
                parser?.lastEventId?.let { lastEventId = it }
            }
            if (closed) break
            try {
                Thread.sleep(backoff)
            } catch (_: InterruptedException) {
                break
            }
            backoff = (backoff * 2).coerceAtMost(10_000L)
        }
    }

    private fun dispatch(msg: SseMessage) {
        val data = Json.parseOrNull(msg.data).jsonObject() ?: emptyMap()
        onEvent(msg.id?.toLongOrNull(), msg.event, data)
    }
}
