package dev.darkpyonix.intellij.notebook

import com.intellij.notification.NotificationType
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.application.ReadAction
import com.intellij.openapi.command.WriteCommandAction
import com.intellij.openapi.diagnostic.Logger
import com.intellij.openapi.editor.Document
import com.intellij.openapi.editor.Editor
import com.intellij.openapi.editor.RangeMarker
import com.intellij.openapi.editor.event.DocumentEvent
import com.intellij.openapi.editor.event.DocumentListener
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Disposer
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.util.concurrency.AppExecutorUtil
import dev.darkpyonix.intellij.format.NotebookCell
import dev.darkpyonix.intellij.format.NotebookFormat
import dev.darkpyonix.intellij.json.jsonObject
import dev.darkpyonix.intellij.json.obj
import dev.darkpyonix.intellij.json.str
import dev.darkpyonix.intellij.manager.EventStream
import dev.darkpyonix.intellij.manager.ManagerClient
import dev.darkpyonix.intellij.manager.ManagerException
import dev.darkpyonix.intellij.protocol.Actor
import dev.darkpyonix.intellij.protocol.ApplyResult
import dev.darkpyonix.intellij.protocol.CellCursor
import dev.darkpyonix.intellij.protocol.CellLock
import dev.darkpyonix.intellij.protocol.CellMatcher
import dev.darkpyonix.intellij.protocol.CellReconciler
import dev.darkpyonix.intellij.protocol.DocCell
import dev.darkpyonix.intellij.protocol.DocChange
import dev.darkpyonix.intellij.protocol.DocumentState
import dev.darkpyonix.intellij.protocol.Presence
import dev.darkpyonix.intellij.protocol.SyncOp
import dev.darkpyonix.intellij.settings.DarkPyonixSettings
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/**
 * One notebook file open in the IDE, bound to its kernel through a manager.
 *
 * - Outputs: the document snapshot (FR-R4) plus live events (PROTOCOL §3.4) in [state].
 * - Collaboration (FR-S1..S8): buffer edits are locked (FR-S3), pushed 300 ms after typing stops
 *   with `base_version` (FR-S2), and edits of other clients are mirrored into the buffer.
 *   Cells locked by others become guarded (read-only) blocks.
 * - Presence (FR-S4): the event stream carries `client_id`; focus and cursor go to `PUT /presence`.
 */
class NotebookSession(
    val project: Project,
    private val service: NotebookService,
    val file: VirtualFile,
    val document: Document,
) : Disposable {
    private val log = Logger.getInstance(NotebookSession::class.java)
    private val settings = DarkPyonixSettings.getInstance()
    val identity = settings.identity()

    val state = DocumentState()

    @Volatile
    var client: ManagerClient? = null
        private set

    @Volatile
    var kernelId: String? = null
        private set

    /** Permission of our token (FR-A3, FR-S8); null until connected. */
    @Volatile
    var permission: String? = null
        private set

    @Volatile
    var streamConnected = false
        private set

    @Volatile
    var statusText: String = "Not connected"
        private set

    private var stream: EventStream? = null
    private val listeners = CopyOnWriteArrayList<() -> Unit>()
    private val changeScheduled = AtomicBoolean(false)
    private val connecting = AtomicBoolean(false)

    /** Last parse of the buffer; updated on the EDT after every change. */
    @Volatile
    var local: NotebookFormat.Parsed = NotebookFormat.parse(document.text)
        private set

    // Edit sync state.
    private val dirty = AtomicBoolean(false)
    private val flushLock = Any()
    private var flushFuture: ScheduledFuture<*>? = null
    private val myLocks: MutableSet<String> = ConcurrentHashMap.newKeySet()
    private val lockRequests: MutableSet<String> = ConcurrentHashMap.newKeySet()
    private var applyingRemote = false
    private var guards = ArrayList<RangeMarker>()
    @Volatile
    private var lastEditAt = 0L
    private val idleUnlock: ScheduledFuture<*>

    // Presence throttling (20/s server side; we send at most 10/s).
    @Volatile
    private var pendingPresence: Pair<String?, Map<String, Any?>?>? = null
    private val presenceScheduled = AtomicBoolean(false)
    private var lastFocusedCellId: String? = null

    private var editors = 0

    init {
        document.addDocumentListener(object : DocumentListener {
            override fun beforeDocumentChange(event: DocumentEvent) = onBeforeChange(event)
            override fun documentChanged(event: DocumentEvent) = onChanged()
        }, this)
        idleUnlock = AppExecutorUtil.getAppScheduledExecutorService().scheduleWithFixedDelay(
            Runnable { releaseIdleLocks() }, 30, 30, TimeUnit.SECONDS,
        )
    }

    // ---------------------------------------------------------------- lifecycle

    fun editorOpened() {
        editors++
        if (editors == 1 && settings.state.attachOnOpen) connectAsync(startKernel = false, quiet = true)
    }

    fun editorClosed() {
        editors--
        if (editors <= 0) Disposer.dispose(this)
    }

    val isConnected: Boolean get() = client != null && kernelId != null
    val canEdit: Boolean get() = permission == "admin" || permission == "editor"
    val canRun: Boolean get() = canEdit || permission == "viewer3"

    fun addListener(parent: Disposable, listener: () -> Unit) {
        listeners += listener
        Disposer.register(parent) { listeners -= listener }
    }

    /** Coalesces change notifications onto the EDT (at most one pending at a time). */
    fun fireChanged() {
        if (!changeScheduled.compareAndSet(false, true)) return
        AppExecutorUtil.getAppScheduledExecutorService().schedule(Runnable {
            ApplicationManager.getApplication().invokeLater({
                changeScheduled.set(false)
                if (!project.isDisposed) listeners.forEach { it() }
            }, ModalityState.any())
        }, 50, TimeUnit.MILLISECONDS)
    }

    private class ConnectRequest(val startKernel: Boolean, val quiet: Boolean, val then: (() -> Unit)?)

    private val connectRequests = CopyOnWriteArrayList<ConnectRequest>()

    /**
     * Attaches to the file's kernel, then runs [then] on a pooled thread. With [startKernel],
     * spawns a manager and the kernel if needed. Requests made while connecting are served by
     * the same attempt (or a retry with `startKernel` if the attempt only looked for a kernel).
     */
    fun connectAsync(startKernel: Boolean, quiet: Boolean = false, then: (() -> Unit)? = null) {
        if (isConnected) {
            then?.let { pooled(it) }
            return
        }
        connectRequests += ConnectRequest(startKernel, quiet, then)
        drainConnectRequests()
    }

    private fun drainConnectRequests() {
        if (!connecting.compareAndSet(false, true)) return
        pooled {
            try {
                while (connectRequests.isNotEmpty()) {
                    val batch = connectRequests.toList()
                    connectRequests.removeAll(batch.toSet())
                    val ok = try {
                        isConnected || connect(batch.any { it.startKernel })
                    } catch (e: Exception) {
                        log.info("DarkPyonix connect failed", e)
                        setStatus("Connection failed: ${e.message}")
                        if (batch.any { !it.quiet }) service.notify("DarkPyonix: ${e.message}", NotificationType.ERROR)
                        false
                    }
                    if (ok) batch.forEach { it.then?.invoke() }
                }
            } finally {
                connecting.set(false)
            }
            if (connectRequests.isNotEmpty()) drainConnectRequests()
        }
    }

    private fun connect(startKernel: Boolean): Boolean {
        val c = service.managerClient(spawn = startKernel) ?: run {
            setStatus("No DarkPyonix manager running")
            return false
        }
        val canonical = File(file.path).canonicalPath
        val kid = if (startKernel) {
            setStatus("Starting kernel…")
            c.startKernel(canonical, settings.state.python?.takeIf { it.isNotBlank() }).str("kernel_id")
                ?: NotebookFormat.kernelIdFor(canonical)
        } else {
            val k = NotebookFormat.kernelIdFor(canonical)
            c.getKernel(k) ?: run {
                setStatus("No kernel for this file (run a cell to start one)")
                return false
            }
            k
        }
        permission = try {
            c.managerInfo().str("permission")
        } catch (e: ManagerException) {
            null
        }
        client = c
        kernelId = kid
        resync()
        startStream()
        setStatus("Connected to ${c.baseUrl}")
        return true
    }

    private fun startStream() {
        val c = client ?: return
        val kid = kernelId ?: return
        stream?.close()
        stream = EventStream(c, kid, state.subscribeSince, ::onEvent) { connected, error ->
            streamConnected = connected
            if (!connected && error != null) setStatus("Event stream: ${error.message}")
            if (connected) setStatus("Connected to ${c.baseUrl}")
        }.also { it.start() }
    }

    /** Loads a fresh `GET /kernels/{id}/document` snapshot (FR-S1). */
    fun resync() {
        val c = client ?: return
        val kid = kernelId ?: return
        state.loadSnapshot(c.document(kid))
        ApplicationManager.getApplication().invokeLater({ refreshGuards() }, ModalityState.any())
        fireChanged()
    }

    private fun setStatus(text: String) {
        statusText = text
        fireChanged()
    }

    // ---------------------------------------------------------------- events

    private fun onEvent(seq: Long?, type: String, data: Map<String, Any?>) {
        val before = state.cells
        val result = state.apply(seq, type, data)
        when (result.kind) {
            ApplyResult.Kind.RESYNC -> pooled { runCatching { resync() } }
            ApplyResult.Kind.IGNORED -> return
            ApplyResult.Kind.APPLIED -> {}
        }
        val change = result.docChange
        if (change != null && change.by?.clientId != identity.clientId) {
            val after = state.cells
            ApplicationManager.getApplication().invokeLater({ applyRemote(before, after, change) }, ModalityState.nonModal())
        }
        if (result.locksChanged || change != null) {
            ApplicationManager.getApplication().invokeLater({ refreshGuards() }, ModalityState.any())
        }
        if (type == "run.finished") {
            val status = data.str("status")
            val by = Actor.from(data["interrupted_by"])
            if (status == "error" || by != null) setStatus("Last run: $status${by?.let { " by ${it.displayName}" } ?: ""}")
        }
        fireChanged()
    }

    /** Mirrors another client's edit into the buffer (EDT). */
    private fun applyRemote(before: List<DocCell>, after: List<DocCell>, change: DocChange) {
        if (project.isDisposed) return
        if (change is DocChange.Reloaded) {
            // The kernel re-read the file after an outside edit (FR-S5); let the VFS reload it.
            file.refresh(true, false)
            return
        }
        val parsed = NotebookFormat.parse(document.text)
        val match = CellMatcher.match(parsed.cells, before)
        fun localOf(cellId: String?): NotebookCell? {
            if (cellId == null) return null
            val i = match.indexOf(cellId)
            return if (i >= 0) parsed.cells[i] else null
        }
        fun anchorFor(cell: DocCell): NotebookCell? {
            val prevId = after.getOrNull(after.indexOfFirst { it.cellId == cell.cellId } - 1)?.cellId
            return localOf(prevId) ?: parsed.cells.firstOrNull()
        }
        fun cellText(cell: DocCell): String {
            val text = NotebookFormat.formatHeader(cell.title, cell.type, cell.metadata) + cell.source
            return if (text.endsWith("\n")) text else text + "\n"
        }

        val edits = ArrayList<() -> Unit>()
        when (change) {
            is DocChange.Updated -> {
                val l = localOf(change.cell.cellId) ?: return
                val c = change.cell
                edits += {
                    if (l.source != c.source) document.replaceString(l.bodyStartOffset, l.endOffset, c.source)
                    val headerChanged = !l.isPreamble && (l.type != c.type || l.metadata != c.metadata)
                    if (headerChanged) {
                        document.replaceString(l.startOffset, l.bodyStartOffset, NotebookFormat.formatHeader(l.title, c.type, c.metadata))
                    }
                }
            }
            is DocChange.Created -> {
                val anchor = anchorFor(change.cell)
                val at = anchor?.endOffset ?: document.textLength
                edits += {
                    val needsNl = at > 0 && document.charsSequence[at - 1] != '\n'
                    document.insertString(at, (if (needsNl) "\n" else "") + cellText(change.cell))
                }
            }
            is DocChange.Deleted -> {
                val l = localOf(change.cellId) ?: return
                if (l.isPreamble) return
                edits += { document.deleteString(l.startOffset, l.endOffset) }
            }
            is DocChange.Moved -> {
                val l = localOf(change.cell.cellId) ?: return
                val anchor = anchorFor(change.cell)
                edits += {
                    var text = document.charsSequence.subSequence(l.startOffset, l.endOffset).toString()
                    if (!text.endsWith("\n")) text += "\n"
                    var at = anchor?.endOffset ?: document.textLength
                    if (at > l.startOffset) at -= (l.endOffset - l.startOffset)
                    document.deleteString(l.startOffset, l.endOffset)
                    at = at.coerceIn(0, document.textLength)
                    val needsNl = at > 0 && document.charsSequence[at - 1] != '\n'
                    document.insertString(at, (if (needsNl) "\n" else "") + text)
                }
            }
            is DocChange.Reloaded -> {}
        }
        if (edits.isEmpty()) return
        val who = change.by?.displayName ?: "another client"
        removeGuards()
        applyingRemote = true
        try {
            WriteCommandAction.runWriteCommandAction(project, "DarkPyonix: Edit by $who", null, {
                edits.forEach { it() }
            })
        } finally {
            applyingRemote = false
        }
        local = NotebookFormat.parse(document.text)
        refreshGuards()
        // The kernel saves the same bytes (FR-S5); keep the buffer unmodified so the IDE does
        // not report a conflict when the file changes on disk.
        if (!dirty.get()) FileDocumentManager.getInstance().saveDocument(document)
        fireChanged()
    }

    // ---------------------------------------------------------------- local edits

    private fun onBeforeChange(event: DocumentEvent) {
        if (applyingRemote || !isConnected || !canEdit) return
        val cells = local.cells
        val start = event.offset
        val end = event.offset + event.oldLength
        val cell = cells.firstOrNull { start >= it.bodyStartOffset && end <= it.endOffset && start < it.endOffset + 1 }
            ?: return
        if (event.newFragment.contains("# %%") || event.oldFragment.contains("# %%")) return
        val id = CellMatcher.match(cells, state.cells)[cell.index] ?: return
        if (id in myLocks || id in lockRequests) return
        lockRequests += id
        pooled { lock(id) }
    }

    private fun onChanged() {
        local = NotebookFormat.parse(document.text)
        if (applyingRemote) return
        fireChanged()
        if (!isConnected || !canEdit) return
        lastEditAt = System.currentTimeMillis()
        dirty.set(true)
        synchronized(flushLock) {
            flushFuture?.cancel(false)
            flushFuture = AppExecutorUtil.getAppScheduledExecutorService().schedule(Runnable { flush() }, 300, TimeUnit.MILLISECONDS)
        }
    }

    private fun lock(cellId: String): Boolean {
        val c = client ?: return false
        val kid = kernelId ?: return false
        return try {
            val resp = c.lockCell(kid, cellId)
            myLocks += cellId
            state.setLock(cellId, CellLock.from(resp, cellId))
            true
        } catch (e: ManagerException) {
            if (e.isLocked) {
                val by = e.data?.get("locked_by")
                val who = (by.jsonObject()?.str("user") ?: by as? String) ?: "another client"
                service.notify(
                    "This cell is being edited by $who; your change was not sent.",
                    NotificationType.WARNING,
                    "Take theirs" to { takeTheirs(cellId) },
                )
            } else {
                log.info("lock failed", e)
            }
            false
        } catch (e: Exception) {
            log.info("lock failed", e)
            false
        } finally {
            lockRequests -= cellId
        }
    }

    /** Pushes the buffer to the kernel's document (FR-S2). Runs on a pooled thread. */
    fun flush() {
        synchronized(flushLock) {
            if (!dirty.getAndSet(false)) return
            val c = client ?: return
            val kid = kernelId ?: return
            val text = ReadAction.compute<String, RuntimeException> { document.text }
            val parsed = NotebookFormat.parse(text)
            val server = state.cells
            val match = CellMatcher.match(parsed.cells, server)
            val ops = CellReconciler.reconcile(parsed.cells, server, match)
            val refs = HashMap<String, String>()
            var failed = false
            for (op in ops) {
                try {
                    when (op) {
                        is SyncOp.Update -> {
                            if (op.cellId !in myLocks) {
                                val lockedByOther = state.cell(op.cellId)?.lock?.let { it.lockedBy != identity.clientId } ?: false
                                if (lockedByOther || !lock(op.cellId)) {
                                    failed = true
                                    continue
                                }
                            }
                            state.upsertFromResponse(c.updateCell(kid, op.cellId, op.baseVersion, op.source, op.type, op.metadata))
                        }
                        is SyncOp.Create -> {
                            val after = op.afterRef?.let { refs[it] ?: it.takeUnless { r -> r.startsWith("new:") } }
                            val resp = c.createCell(kid, op.type, op.source, op.metadata, after = after)
                            state.upsertFromResponse(resp)
                            resp.str("cell_id")?.let { refs[op.ref] = it }
                        }
                        is SyncOp.Delete -> {
                            c.deleteCell(kid, op.cellId, op.baseVersion)
                            state.removeCell(op.cellId)
                            myLocks -= op.cellId
                        }
                        is SyncOp.Move -> {
                            val id = refs[op.ref] ?: op.ref.takeUnless { it.startsWith("new:") } ?: continue
                            state.upsertFromResponse(c.moveCell(kid, id, op.toIndex))
                        }
                    }
                } catch (e: ManagerException) {
                    failed = true
                    onEditRejected(op, e)
                } catch (e: Exception) {
                    failed = true
                    log.info("DarkPyonix edit failed", e)
                    setStatus("Edit not sent: ${e.message}")
                    break
                }
            }
            if (!failed && !dirty.get()) {
                ApplicationManager.getApplication().invokeLater({
                    if (!dirty.get() && !project.isDisposed) FileDocumentManager.getInstance().saveDocument(document)
                }, ModalityState.nonModal())
            }
            fireChanged()
        }
    }

    private fun onEditRejected(op: SyncOp, e: ManagerException) {
        val cellId = when (op) {
            is SyncOp.Update -> op.cellId
            is SyncOp.Delete -> op.cellId
            else -> null
        }
        when {
            e.isConflict -> {
                // data.cell is the current cell (manager.openapi.yaml, Conflict).
                e.data?.obj("cell")?.let { state.upsertFromResponse(it) }
                val cur = cellId?.let { state.cell(it) }
                service.notify(
                    "Cell ${cur?.index ?: ""} was changed by someone else while you were editing it.",
                    NotificationType.WARNING,
                    "Keep mine" to { dirty.set(true); pooled { flush() } },
                    "Take theirs" to { if (cellId != null) takeTheirs(cellId) },
                )
            }
            e.isLocked -> {
                val by = e.data?.get("locked_by")
                val who = (by.jsonObject()?.str("user") ?: by as? String) ?: "another client"
                service.notify(
                    "Cell is locked by $who; your change was not sent.",
                    NotificationType.WARNING,
                    "Take theirs" to { if (cellId != null) takeTheirs(cellId) },
                )
            }
            e.status == 403 -> {
                permission = permission?.takeIf { it != "editor" && it != "admin" }
                service.notify("Your token cannot edit this notebook (FR-S8).", NotificationType.WARNING)
            }
            else -> setStatus("Edit refused: ${e.message}")
        }
    }

    /** Replaces the buffer's copy of [cellId] with the kernel's (conflict resolution). */
    fun takeTheirs(cellId: String) {
        val cell = state.cell(cellId) ?: return
        ApplicationManager.getApplication().invokeLater({
            applyRemote(state.cells, state.cells, DocChange.Updated(null, cell, null))
        }, ModalityState.nonModal())
    }

    /** Releases our locks on cells other than [keep] after pushing pending edits (FR-S3). */
    private fun releaseLocks(keep: String?) {
        val toRelease = myLocks.filter { it != keep }
        if (toRelease.isEmpty()) return
        pooled {
            flush()
            val c = client ?: return@pooled
            val kid = kernelId ?: return@pooled
            for (id in toRelease) {
                try {
                    c.unlockCell(kid, id)
                } catch (e: Exception) {
                    log.debug("unlock failed", e)
                }
                myLocks -= id
                state.setLock(id, null)
            }
            ApplicationManager.getApplication().invokeLater({ refreshGuards() }, ModalityState.any())
            fireChanged()
        }
    }

    private fun releaseIdleLocks() {
        if (myLocks.isNotEmpty() && System.currentTimeMillis() - lastEditAt > 60_000) releaseLocks(null)
    }

    // ---------------------------------------------------------------- presence

    /** Caret moved in an editor of this document (EDT). */
    fun onCaret(editor: Editor) {
        if (!isConnected) return
        val offset = editor.caretModel.offset
        val cells = local.cells
        val pos = cells.indexOfFirst { it.containsOffset(offset) }.let { if (it < 0) cells.lastIndex else it }
        val cell = cells.getOrNull(pos) ?: return
        val cellId = CellMatcher.match(cells, state.cells)[pos]
        if (cellId != lastFocusedCellId) {
            lastFocusedCellId = cellId
            releaseLocks(cellId)
        }
        val caretLine = editor.document.getLineNumber(offset)
        val line = (caretLine - cell.bodyStartLine).coerceAtLeast(0)
        val column = if (caretLine < cell.bodyStartLine) 0 else offset - editor.document.getLineStartOffset(caretLine)
        val cursor = cellId?.let {
            val sel = editor.selectionModel
            val selection = if (sel.hasSelection()) listOf(lineCol(editor, cell, sel.selectionStart), lineCol(editor, cell, sel.selectionEnd)) else null
            CellCursor(it, line, column, selection).toJson()
        }
        pendingPresence = cellId to cursor
        if (presenceScheduled.compareAndSet(false, true)) {
            AppExecutorUtil.getAppScheduledExecutorService().schedule(Runnable {
                presenceScheduled.set(false)
                val p = pendingPresence ?: return@Runnable
                val c = client ?: return@Runnable
                val kid = kernelId ?: return@Runnable
                try {
                    c.updatePresence(kid, p.first, p.second)
                } catch (e: Exception) {
                    log.debug("presence failed", e)
                }
            }, 100, TimeUnit.MILLISECONDS)
        }
    }

    private fun lineCol(editor: Editor, cell: NotebookCell, offset: Int): List<Int> {
        val line = editor.document.getLineNumber(offset)
        return listOf((line - cell.bodyStartLine).coerceAtLeast(0), offset - editor.document.getLineStartOffset(line))
    }

    /** Other clients (not this IDE). */
    fun others(): List<Presence> = state.presence.filter { it.clientId != identity.clientId }

    // ---------------------------------------------------------------- guarded blocks

    /** Cells locked by other clients become read-only (FR-S3). EDT only. */
    fun refreshGuards() {
        if (project.isDisposed) return
        removeGuards()
        val cells = local.cells
        val server = state.cells
        val match = CellMatcher.match(cells, server)
        for ((i, l) in cells.withIndex()) {
            val cell = server.firstOrNull { it.cellId == match[i] } ?: continue
            val lock = cell.lock ?: continue
            if (lock.lockedBy == identity.clientId) continue
            if (l.endOffset > l.startOffset) guards += document.createGuardedBlock(l.startOffset, l.endOffset)
        }
    }

    private fun removeGuards() {
        for (g in guards) document.removeGuardedBlock(g)
        guards = ArrayList()
    }

    // ---------------------------------------------------------------- mapping for the UI

    /** Buffer cells paired with the kernel's cells (FR-R4 order). */
    fun mappedCells(): List<Pair<NotebookCell, DocCell?>> {
        val cells = local.cells
        val server = state.cells
        val byId = server.associateBy { it.cellId }
        val match = CellMatcher.match(cells, server)
        return cells.mapIndexed { i, l -> l to match[i]?.let { byId[it] } }
    }

    /** Outputs are stale when the buffer's source differs from the source that produced them. */
    fun isStale(local: NotebookCell, cell: DocCell): Boolean {
        if (cell.outputs.isEmpty()) return false
        if (cell.outputsSourceSha256 != null) return cell.outputsSourceSha256 != local.sourceSha256
        return cell.stale || cell.sourceSha256 != local.sourceSha256
    }

    // ---------------------------------------------------------------- runs

    /** Runs the cells at buffer positions [positions], or the whole file when null. */
    fun run(positions: List<Int>?) {
        connectAsync(startKernel = true) { runNow(positions, onBusy = if (settings.state.queueWhenBusy) "queue" else "reject") }
    }

    private fun runNow(positions: List<Int>?, onBusy: String) {
        val c = client ?: return
        val kid = kernelId ?: return
        if (!canRun) {
            service.notify("Your token cannot run cells (needs viewer3 or above).", NotificationType.WARNING)
            return
        }
        try {
            if (canEdit) {
                dirty.set(true)
                flush()
                if (positions == null) {
                    c.startRun(kid, onBusy = onBusy)
                } else {
                    val parsed = local
                    val match = CellMatcher.match(parsed.cells, state.cells)
                    val ids = positions.mapNotNull { match.getOrNull(it) }
                    if (ids.size == positions.size) {
                        c.startRun(kid, cellIds = ids, onBusy = onBusy)
                    } else {
                        c.startRun(kid, cells = positions.map { parsed.cells[it].index }, source = parsed.cells.joinToString("") { it.header + it.source }, onBusy = onBusy)
                    }
                }
            } else {
                // Without edit rights the kernel's copy may differ from the buffer: send the buffer.
                val text = ReadAction.compute<String, RuntimeException> { document.text }
                val indexes = positions?.map { local.cells[it].index }
                c.startRun(kid, cells = indexes, source = text, onBusy = onBusy)
            }
        } catch (e: ManagerException) {
            if (e.isBusy) {
                val current = e.data?.obj("current")?.str("run_id") ?: "another run"
                service.notify(
                    "The kernel is busy with $current.",
                    NotificationType.WARNING,
                    "Queue" to { pooled { runNow(positions, "queue") } },
                    "Interrupt" to { interrupt() },
                )
            } else {
                service.notify("DarkPyonix: ${e.message}", NotificationType.ERROR)
            }
        }
    }

    /** `POST /interrupt`: KeyboardInterrupt in the running cell; the namespace is kept. */
    fun interrupt() {
        pooled {
            val c = client ?: return@pooled
            val kid = kernelId ?: return@pooled
            try {
                c.interrupt(kid)
            } catch (e: Exception) {
                service.notify("DarkPyonix: ${e.message}", NotificationType.ERROR)
            }
        }
    }

    fun restart(hard: Boolean) {
        pooled {
            val c = client ?: return@pooled
            val kid = kernelId ?: return@pooled
            try {
                c.restart(kid, hard)
                resync()
            } catch (e: Exception) {
                service.notify("DarkPyonix: ${e.message}", NotificationType.ERROR)
            }
        }
    }

    /** Cell at a buffer offset, as a position in [local]. */
    fun positionAt(offset: Int): Int {
        val cells = local.cells
        val i = cells.indexOfFirst { it.containsOffset(offset) }
        return if (i < 0) cells.lastIndex else i
    }

    fun serverCellAt(position: Int): DocCell? {
        val cells = local.cells
        val id = CellMatcher.match(cells, state.cells).getOrNull(position) ?: return null
        return state.cell(id)
    }

    // ---------------------------------------------------------------- disposal

    override fun dispose() {
        idleUnlock.cancel(false)
        synchronized(flushLock) { flushFuture?.cancel(false) }
        service.sessionClosed(this)
        val c = client
        val kid = kernelId
        val locks = myLocks.toList()
        val pendingEdits = dirty.get()
        stream?.close()
        stream = null
        ApplicationManager.getApplication().invokeLater({ removeGuards() }, ModalityState.any())
        if (c != null && kid != null) {
            AppExecutorUtil.getAppExecutorService().execute {
                try {
                    if (pendingEdits) flush()
                    for (id in locks) runCatching { c.unlockCell(kid, id) }
                    runCatching { c.leavePresence(kid) }
                } catch (_: Exception) {
                }
            }
        }
    }

    private fun pooled(block: () -> Unit) {
        ApplicationManager.getApplication().executeOnPooledThread(Runnable { block() })
    }
}
