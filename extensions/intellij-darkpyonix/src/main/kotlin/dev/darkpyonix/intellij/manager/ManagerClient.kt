package dev.darkpyonix.intellij.manager

import dev.darkpyonix.intellij.json.Json
import dev.darkpyonix.intellij.json.jsonObject
import dev.darkpyonix.intellij.json.obj
import dev.darkpyonix.intellij.json.str
import java.io.IOException
import java.io.InputStream
import java.net.URI
import java.net.URLEncoder
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import java.nio.charset.StandardCharsets
import java.time.Duration

/** A non-2xx answer from the manager: `{"error": {code, message, data}}`. */
class ManagerException(
    val status: Int,
    val code: String,
    message: String,
    val data: Map<String, Any?>? = null,
) : IOException("$status $code: $message") {
    val isConflict get() = status == 409 && code == "conflict"
    val isLocked get() = status == 409 && code == "locked"
    val isBusy get() = status == 409 && code == "busy"
}

/** Who this IDE is, for `X-DarkPyonix-Client` / `X-DarkPyonix-Nickname` (SPEC FR-S4). */
data class ClientIdentity(val clientId: String, val nickname: String)

/**
 * Client for the Kernel Manager API (`docs/api/manager.openapi.yaml`, 1.0.0-draft.3). Every
 * call is blocking; call it off the EDT.
 */
class ManagerClient(
    val baseUrl: String,
    private val token: String,
    val identity: ClientIdentity,
    val http: HttpClient = defaultHttp,
) {
    companion object {
        val defaultHttp: HttpClient = HttpClient.newBuilder()
            .connectTimeout(Duration.ofSeconds(5))
            .followRedirects(HttpClient.Redirect.NORMAL)
            .build()

        private fun enc(s: String) = URLEncoder.encode(s, StandardCharsets.UTF_8)
    }

    private val api = baseUrl.trimEnd('/') + "/api/v1"

    // System

    fun health(): Boolean = try {
        val req = HttpRequest.newBuilder(URI.create(baseUrl.trimEnd('/') + "/health"))
            .timeout(Duration.ofSeconds(2)).GET().build()
        http.send(req, HttpResponse.BodyHandlers.discarding()).statusCode() in 200..299
    } catch (_: Exception) {
        false
    }

    fun managerInfo(): Map<String, Any?> = obj(call("GET", "/manager"))

    // Kernels

    fun listKernels(): List<Map<String, Any?>> =
        (obj(call("GET", "/kernels"))["kernels"] as? List<*>)?.mapNotNull { it.jsonObject() } ?: emptyList()

    /** `GET /kernels/{id}`, or null when no such kernel runs. */
    fun getKernel(kernelId: String): Map<String, Any?>? = try {
        obj(call("GET", "/kernels/$kernelId"))
    } catch (e: ManagerException) {
        if (e.status == 404) null else throw e
    }

    /** `POST /kernels {path}`: idempotent per file (FR-M2); may wait up to 10 s. */
    fun startKernel(path: String, python: String? = null): Map<String, Any?> {
        val body = linkedMapOf<String, Any?>("path" to path)
        if (!python.isNullOrBlank()) body["python"] = python
        return obj(call("POST", "/kernels", body, timeout = Duration.ofSeconds(30)))
    }

    fun interrupt(kernelId: String): Map<String, Any?> = obj(call("POST", "/kernels/$kernelId/interrupt", emptyMap<String, Any?>()))

    fun restart(kernelId: String, hard: Boolean): Map<String, Any?> =
        obj(call("POST", "/kernels/$kernelId/restart", mapOf("hard" to hard), timeout = Duration.ofSeconds(30)))

    // Documents (FR-R4, FR-S1)

    fun document(kernelId: String): Map<String, Any?> = obj(call("GET", "/kernels/$kernelId/document"))

    // Runs

    /**
     * `POST /kernels/{id}/runs`. Pass [cellIds] (FR-S6) or [cells] (indexes); neither runs the
     * whole file. [source] runs an unsaved buffer instead of the file.
     */
    fun startRun(
        kernelId: String,
        cellIds: List<String>? = null,
        cells: List<Int>? = null,
        source: String? = null,
        onBusy: String = "reject",
    ): Map<String, Any?> {
        val body = linkedMapOf<String, Any?>()
        if (cellIds == null && cells == null) {
            body["mode"] = "all"
        } else {
            body["mode"] = "cells"
            if (cellIds != null) body["cell_ids"] = cellIds
            if (cells != null) body["cells"] = cells
        }
        if (source != null) body["source"] = source
        body["on_busy"] = onBusy
        return obj(call("POST", "/kernels/$kernelId/runs", body))
    }

    // Collaboration (FR-S2..S4)

    fun createCell(
        kernelId: String,
        type: String,
        source: String,
        metadata: Map<String, Any?>?,
        after: String? = null,
        before: String? = null,
    ): Map<String, Any?> {
        val body = linkedMapOf<String, Any?>("type" to type, "source" to source)
        if (!metadata.isNullOrEmpty()) body["metadata"] = metadata
        if (after != null) body["after"] = after
        if (before != null) body["before"] = before
        return obj(call("POST", "/kernels/$kernelId/cells", body))
    }

    fun updateCell(
        kernelId: String,
        cellId: String,
        baseVersion: Long,
        source: String? = null,
        type: String? = null,
        metadata: Map<String, Any?>? = null,
    ): Map<String, Any?> {
        val body = linkedMapOf<String, Any?>("base_version" to baseVersion)
        if (source != null) body["source"] = source
        if (type != null) body["type"] = type
        if (metadata != null) body["metadata"] = metadata
        return obj(call("PATCH", "/kernels/$kernelId/cells/${enc(cellId)}", body))
    }

    fun deleteCell(kernelId: String, cellId: String, baseVersion: Long) {
        call("DELETE", "/kernels/$kernelId/cells/${enc(cellId)}?base_version=$baseVersion")
    }

    fun moveCell(kernelId: String, cellId: String, toIndex: Int): Map<String, Any?> =
        obj(call("POST", "/kernels/$kernelId/cells/${enc(cellId)}/move", mapOf("to_index" to toIndex)))

    /** `PUT .../lock`: takes or renews the lock (FR-S3). */
    fun lockCell(kernelId: String, cellId: String): Map<String, Any?> =
        obj(call("PUT", "/kernels/$kernelId/cells/${enc(cellId)}/lock", emptyMap<String, Any?>()))

    /** `DELETE .../lock`, optionally saving [source] first. */
    fun unlockCell(kernelId: String, cellId: String, source: String? = null, baseVersion: Long? = null) {
        val body = if (source != null) linkedMapOf<String, Any?>("source" to source, "base_version" to baseVersion) else null
        call("DELETE", "/kernels/$kernelId/cells/${enc(cellId)}/lock", body)
    }

    fun updatePresence(kernelId: String, focusedCellId: String?, cursor: Map<String, Any?>?) {
        call("PUT", "/kernels/$kernelId/presence", mapOf("focused_cell_id" to focusedCellId, "cursor" to cursor))
    }

    fun leavePresence(kernelId: String) {
        call("DELETE", "/kernels/$kernelId/presence")
    }

    // Events

    /** The `GET /kernels/{id}/events` request; [EventStream] owns the connection. */
    fun eventsRequest(kernelId: String, lastEventId: String?, since: Long?): HttpRequest {
        val q = StringBuilder("?client_id=").append(enc(identity.clientId))
            .append("&nickname=").append(enc(identity.nickname))
        if (lastEventId == null && since != null) q.append("&since=").append(since)
        val b = HttpRequest.newBuilder(URI.create("$api/kernels/$kernelId/events$q"))
            .header("Authorization", "Bearer $token")
            .header("Accept", "text/event-stream")
            .header("X-DarkPyonix-Client", identity.clientId)
            .GET()
        if (lastEventId != null) b.header("Last-Event-ID", lastEventId)
        return b.build()
    }

    // Plumbing

    private fun obj(v: Any?): Map<String, Any?> = v.jsonObject() ?: emptyMap()

    private fun call(method: String, path: String, body: Any? = null, timeout: Duration = Duration.ofSeconds(15)): Any? {
        val b = HttpRequest.newBuilder(URI.create(api + path))
            .timeout(timeout)
            .header("Authorization", "Bearer $token")
            .header("Accept", "application/json")
            .header("X-DarkPyonix-Client", identity.clientId)
            .header("X-DarkPyonix-Nickname", identity.nickname)
        if (body != null) {
            b.header("Content-Type", "application/json")
            b.method(method, HttpRequest.BodyPublishers.ofString(Json.stringify(body), StandardCharsets.UTF_8))
        } else {
            b.method(method, HttpRequest.BodyPublishers.noBody())
        }
        val resp = http.send(b.build(), HttpResponse.BodyHandlers.ofString(StandardCharsets.UTF_8))
        val text = resp.body() ?: ""
        val parsed = if (text.isBlank()) null else Json.parseOrNull(text)
        if (resp.statusCode() !in 200..299) {
            val err = parsed.jsonObject()?.obj("error")
            throw ManagerException(
                resp.statusCode(),
                err?.str("code") ?: "http_${resp.statusCode()}",
                err?.str("message") ?: text.take(200),
                err?.obj("data"),
            )
        }
        return parsed
    }

    /** Opens the event stream; the caller closes the returned stream to disconnect. */
    fun openEvents(request: HttpRequest): Pair<Int, InputStream> {
        val resp = http.send(request, HttpResponse.BodyHandlers.ofInputStream())
        return resp.statusCode() to resp.body()
    }
}
