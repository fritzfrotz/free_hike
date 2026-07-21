package com.freehike.app

import android.os.Handler
import android.os.Looper
import android.util.Log
import com.getcapacitor.JSObject
import com.getcapacitor.Plugin
import com.getcapacitor.PluginCall
import com.getcapacitor.PluginMethod
import com.getcapacitor.annotation.CapacitorPlugin
import java.io.File
import uniffi.freehike.CompilationStatus
import uniffi.freehike.CompileJob
import uniffi.freehike.ProgressCallback
import uniffi.freehike.compileChunk
import uniffi.freehike.emitTestProgress
import uniffi.freehike.engineVersion
import uniffi.freehike.purgeJob
import uniffi.freehike.queryCheckpoint
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Layer 2 of the tri-layer bridge — Surface v1 (suspendable state machine).
 *
 * startJob drives the budget-yield loop natively: compile_chunk is re-invoked
 * with the same CompileJob while the engine returns Yielded, honoring
 * cancellation between slices. The engine owns all resume state on disk
 * (fsync'd checkpoint keyed by job_id); this layer never round-trips state.
 *
 * In production the loop body is scheduled by WorkManager/Foreground Service
 * slices; here it runs continuously on the plugin's single background lane,
 * which is behaviorally identical from the engine's point of view.
 */
@CapacitorPlugin(name = "MapCompiler")
class MapCompilerPlugin : Plugin() {

    /** Single background lane for FFI work — never block the WebView/main thread. */
    private val executor = Executors.newSingleThreadExecutor()

    /**
     * Cancellation token of the job whose loop is RUNNING right now
     * (P-NATIVE.C1, closes B007): each startJob owns a fresh token, set as
     * active when its loop starts and cleared at its terminal — so a cancel
     * strikes exactly the running job, never a queued one whose flag the
     * old shared-AtomicBoolean design reset at enqueue time.
     */
    private val activeCancel = java.util.concurrent.atomic.AtomicReference<AtomicBoolean?>(null)

    /** Raised by handleOnDestroy (closes D007): the WebView is gone, so any
     *  running/queued foreground loop must stop WITHOUT purging — the
     *  durable checkpoint stays for the next session's resume. */
    @Volatile
    private var destroyed = false

    override fun load() {
        // The UniFFI bindings load libfreehike_ffi.so themselves via JNA on
        // first use; this early loadLibrary surfaces a missing/mismatched .so
        // at plugin load with a clear log line instead of at first user tap.
        try {
            System.loadLibrary("freehike_ffi")
            Log.i(TAG, "libfreehike_ffi.so loaded (" + engineVersion() + ")")
        } catch (e: UnsatisfiedLinkError) {
            Log.e(TAG, "libfreehike_ffi.so missing from jniLibs — FFI calls will fail", e)
        }

        // P8.C3: mirror OS thermal pressure into the Rust core from plugin
        // load (pushes the current status immediately, then listens), and
        // let the background worker reach the WebView while one exists.
        ThermalStateBridge.start(context)
        active = this
    }

    override fun handleOnDestroy() {
        // P-NATIVE.C1 (closes D007): stop the orphan instead of letting it
        // burn CPU headless. The in-flight FFI slice cannot be interrupted,
        // but the loop observes `destroyed` at its next boundary and exits
        // without purging (reload ≠ cancel: the checkpoint must survive for
        // resume). shutdownNow drops queued loops whose PluginCalls died
        // with the WebView.
        destroyed = true
        activeCancel.get()?.set(true)
        executor.shutdownNow()
        if (active === this) active = null
        super.handleOnDestroy()
    }

    /** Forwards a background-compile terminal event to the WebView. */
    internal fun emitBackground(data: JSObject) {
        notifyListeners(EVENT_BACKGROUND, data)
    }

    /** Smoke test: proves the Rust core is linked and callable. */
    @PluginMethod
    fun getEngineVersion(call: PluginCall) {
        executor.execute {
            try {
                call.resolve(JSObject().put("version", engineVersion()))
            } catch (t: Throwable) {
                call.reject("FFI engineVersion failed: ${t.message}", t as? Exception)
            }
        }
    }

    /**
     * Runs a compile job to completion (or failure/cancellation) via the
     * budget-yield loop. Progress streams as `compilationProgress` events;
     * each slice boundary emits a `compilationStatus` event; the call
     * resolves with the terminal status.
     *
     * Params: bbox (required, "west,south,east,north"), jobId?, minZoom?,
     * maxZoom?, budgetMs? (per-slice; default 250).
     */
    @PluginMethod
    fun startJob(call: PluginCall) {
        val bbox = call.getString("bbox")
        if (bbox.isNullOrBlank()) {
            call.reject("Missing required parameter: bbox (\"west,south,east,north\")")
            return
        }
        val jobId = call.getString("jobId") ?: UUID.randomUUID().toString()
        if (!isSafeJobId(jobId)) {
            call.reject(UNSAFE_JOB_ID_MESSAGE)
            return
        }
        val budgetMs = (call.getInt("budgetMs") ?: 250).coerceIn(0, 600_000)
        val minZoom = (call.getInt("minZoom") ?: 5).coerceIn(0, 22)
        val maxZoom = (call.getInt("maxZoom") ?: 14).coerceIn(0, 22)

        val jobsDir = context.filesDir.absolutePath + "/map_jobs"
        val job = CompileJob(
            jobId = jobId,
            bbox = bbox,
            minZoom = minZoom.toUByte(),
            maxZoom = maxZoom.toUByte(),
            // Placeholder inputs until the Phase 2 fetcher lands — the
            // simulated engine does not read them.
            pbfPath = "$jobsDir/raw/$jobId.osm.pbf",
            demPath = "$jobsDir/raw/$jobId.dem.tif",
            outputDir = jobsDir,
        )

        val cancelToken = AtomicBoolean(false)
        executor.execute {
            activeCancel.set(cancelToken)
            try {
                var slices = 0
                while (true) {
                    if (destroyed) {
                        // WebView torn down mid-loop: stop WITHOUT purging
                        // (checkpoint survives for resume); the PluginCall
                        // died with the bridge, so nothing to resolve.
                        Log.i(TAG, "job $jobId loop stopped by plugin destroy after $slices slices; checkpoint kept")
                        return@execute
                    }
                    if (cancelToken.get()) {
                        purgeJob(jobId, job.outputDir)
                        Log.i(TAG, "job $jobId cancelled after $slices slices; state purged")
                        emitStatus("cancelled", jobId, slices)
                        call.resolve(
                            JSObject().put("status", "cancelled").put("jobId", jobId).put("slices", slices)
                        )
                        return@execute
                    }

                    val status = compileChunk(job, budgetMs.toUInt(), bridgeForwardingCallback())
                    slices += 1

                    when (status) {
                        is CompilationStatus.Yielded -> {
                            val cp = status.checkpoint
                            Log.i(
                                TAG,
                                "slice $slices yielded: phase=${cp.phase} block=${cp.nextBlock} " +
                                    "pbfOffset=${cp.pbfByteOffset} bytesWritten=${cp.bytesWritten}"
                            )
                            emitStatus("yielded", jobId, slices)
                            // Loop continues: the engine resumes from its own
                            // durable checkpoint. In production this re-invoke
                            // is a WorkManager/BGTask reschedule instead.
                        }
                        is CompilationStatus.Finished -> {
                            val s = status.summary
                            Log.i(TAG, "job $jobId finished in $slices slices: ${s.blocksTotal} blocks, ${s.bytesWritten} bytes")
                            emitStatus("finished", jobId, slices)
                            call.resolve(
                                JSObject()
                                    .put("status", "finished")
                                    .put("jobId", jobId)
                                    .put("slices", slices)
                                    .put("blocksTotal", s.blocksTotal.toInt())
                                    .put("bytesWritten", s.bytesWritten.toLong())
                            )
                            return@execute
                        }
                        is CompilationStatus.FailedFatal -> {
                            Log.e(TAG, "job $jobId failed after $slices slices: ${status.reason}")
                            emitStatus("failed", jobId, slices)
                            call.resolve(
                                JSObject()
                                    .put("status", "failed")
                                    .put("jobId", jobId)
                                    .put("slices", slices)
                                    .put("reason", status.reason)
                                    .put("transient", false)
                            )
                            return@execute
                        }
                        is CompilationStatus.FailedTransient -> {
                            // Another runner holds the job's slice lock (e.g.
                            // a background window is mid-slice). Durable state
                            // is untouched; surface it as a retryable failure
                            // instead of looping against the lock.
                            Log.w(TAG, "job $jobId transient refusal after $slices slices: ${status.reason}")
                            emitStatus("failed", jobId, slices)
                            call.resolve(
                                JSObject()
                                    .put("status", "failed")
                                    .put("jobId", jobId)
                                    .put("slices", slices)
                                    .put("reason", status.reason)
                                    .put("transient", true)
                            )
                            return@execute
                        }
                    }
                }
            } catch (t: Throwable) {
                call.reject("FFI compileChunk failed: ${t.message}", t as? Exception)
            } finally {
                activeCancel.compareAndSet(cancelToken, null)
            }
        }
    }

    /** Requests cancellation of the RUNNING job (honored between slices).
     *  A queued job keeps its own untouched token (B007 fix). */
    @PluginMethod
    fun cancelJob(call: PluginCall) {
        val token = activeCancel.get()
        token?.set(true)
        call.resolve(JSObject().put("requested", token != null))
    }

    /**
     * Cold-start resume detection: returns the engine's durable checkpoint
     * for a job if one survives on disk (e.g. after the OS killed the
     * process mid-compilation).
     */
    @PluginMethod
    fun queryJob(call: PluginCall) {
        val jobId = call.getString("jobId")
        if (jobId.isNullOrBlank()) {
            call.reject("Missing required parameter: jobId")
            return
        }
        if (!isSafeJobId(jobId)) {
            call.reject(UNSAFE_JOB_ID_MESSAGE)
            return
        }
        val jobsDir = context.filesDir.absolutePath + "/map_jobs"
        executor.execute {
            try {
                val cp = queryCheckpoint(jobId, jobsDir)
                if (cp == null) {
                    Log.i(TAG, "checkpoint query for $jobId: none (fresh start)")
                    call.resolve(JSObject().put("found", false))
                } else {
                    Log.i(
                        TAG,
                        "checkpoint query for $jobId: FOUND phase=${cp.phase} block=${cp.nextBlock} " +
                            "pbfOffset=${cp.pbfByteOffset} bytesWritten=${cp.bytesWritten}"
                    )
                    call.resolve(
                        JSObject()
                            .put("found", true)
                            .put("phase", cp.phase.name)
                            .put("nextBlock", cp.nextBlock.toInt())
                            .put("pbfByteOffset", cp.pbfByteOffset.toLong())
                            .put("bytesWritten", cp.bytesWritten.toLong())
                    )
                }
            } catch (t: Throwable) {
                call.reject("FFI queryCheckpoint failed: ${t.message}", t as? Exception)
            }
        }
    }

    /** Debug: proves the Rust -> Kotlin -> WebView progress event path. */
    @PluginMethod
    fun emitTestProgress(call: PluginCall) {
        val steps = call.getInt("steps") ?: 5
        if (steps < 0) {
            call.reject("steps must be >= 0")
            return
        }
        executor.execute {
            try {
                val sent = emitTestProgress(bridgeForwardingCallback(), steps.toUInt())
                call.resolve(JSObject().put("sent", sent.toInt()))
            } catch (t: Throwable) {
                call.reject("FFI emitTestProgress failed: ${t.message}", t as? Exception)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Background compilation (P8.C3) — mirrors the iOS P8.C2 surface
    // -----------------------------------------------------------------------

    /**
     * Queues a compile job for WorkManager execution: persists the job spec
     * durably (the worker may run in a fresh process with no WebView), then
     * enqueues the charging-constrained unique work request.
     *
     * Enforced single-slot invariant (P1-4): the store holds ONE record, so
     * saving over an existing one would silently orphan a finished job's
     * archive (or yank a pending job out from under its running worker).
     * The authoritative check lives HERE, not in the JS layer — the JS
     * `isBackgroundCompiling` guard only covers 'pending' and can be stale.
     */
    @PluginMethod
    fun enqueueBackgroundJob(call: PluginCall) {
        val bbox = call.getString("bbox")
        if (bbox.isNullOrBlank()) {
            call.reject("Missing required parameter: bbox (\"west,south,east,north\")")
            return
        }
        val jobId = call.getString("jobId") ?: UUID.randomUUID().toString()
        if (!isSafeJobId(jobId)) {
            call.reject(UNSAFE_JOB_ID_MESSAGE)
            return
        }
        val minZoom = (call.getInt("minZoom") ?: 5).coerceIn(0, 22)
        val maxZoom = (call.getInt("maxZoom") ?: 14).coerceIn(0, 22)
        val jobsDir = context.filesDir.absolutePath + "/map_jobs"

        executor.execute {
            val existing = PendingJobStore.loadAny(context)
            if (existing != null) {
                val remedy = if (existing.state == PendingJobStore.STATE_PENDING) {
                    "cancel it (cancelBackgroundJob) first"
                } else {
                    "acknowledge it (acknowledgeBackgroundJob) first"
                }
                call.reject(
                    "The single-job store already holds job '${existing.jobId}' " +
                        "(${existing.state}) — $remedy."
                )
                return@execute
            }

            PendingJobStore.save(
                context,
                PendingJobStore.Record(
                    state = PendingJobStore.STATE_PENDING,
                    jobId = jobId,
                    bbox = bbox,
                    minZoom = minZoom,
                    maxZoom = maxZoom,
                    pbfPath = "$jobsDir/raw/$jobId.osm.pbf",
                    demPath = "$jobsDir/raw/$jobId.dem.tif",
                    outputDir = jobsDir,
                    reason = null,
                    blocksTotal = 0,
                    bytesWritten = 0,
                )
            )
            BackgroundCompileWorker.enqueue(context)
            call.resolve(JSObject().put("scheduled", true).put("jobId", jobId))
        }
    }

    /**
     * Resume-time discovery for the JS layer: reports the durable
     * background-job record. On `finished`, `archivePath` names the
     * .pmtiles in app storage — the WebView stream-copies it into OPFS
     * (the P7 seam) and then calls `acknowledgeBackgroundJob`.
     */
    @PluginMethod
    fun queryBackgroundJob(call: PluginCall) {
        val record = PendingJobStore.loadAny(context)
        if (record == null) {
            call.resolve(JSObject().put("state", "idle"))
            return
        }
        val data = JSObject().put("state", record.state).put("jobId", record.jobId)
        if (record.state == PendingJobStore.STATE_FINISHED) {
            data.put("archivePath", record.archivePath)
                .put("blocksTotal", record.blocksTotal)
                .put("bytesWritten", record.bytesWritten)
        }
        record.reason?.let { data.put("reason", it) }
        call.resolve(data)
    }

    /**
     * Clears a terminal (finished/failed) record once the JS layer has
     * imported the archive into OPFS (or shown the failure), and releases
     * the job's disk claim: the sandbox archive (now redundant with the
     * OPFS copy) plus any leftover checkpoint/index/scratch state.
     *
     * Targeted (P1-4): requires `jobId` and rejects on mismatch, so a stale
     * acknowledge from a slow in-flight ingest can never clear a record it
     * doesn't own. A pending record is NOT clearable here — that is
     * cancelBackgroundJob territory.
     */
    @PluginMethod
    fun acknowledgeBackgroundJob(call: PluginCall) {
        val jobId = call.getString("jobId")
        if (jobId.isNullOrBlank()) {
            call.reject("Missing required parameter: jobId")
            return
        }
        executor.execute {
            val record = PendingJobStore.loadAny(context)
            when {
                record == null -> {
                    // Idempotent: an ack retried after a crash finds the slot
                    // already empty. Not an error, but nothing was cleared.
                    call.resolve(JSObject().put("cleared", false))
                }
                record.jobId != jobId -> {
                    call.reject(
                        "Stale acknowledge: store holds job '${record.jobId}', not '$jobId'"
                    )
                }
                record.state == PendingJobStore.STATE_PENDING -> {
                    call.reject("Job ${record.jobId} is still pending; cancel it instead")
                }
                else -> {
                    // OPFS copy is verified-durable by the caller's contract
                    // (writable closed + byte count checked) before this call,
                    // so the sandbox archive is safe to release. purgeJob is a
                    // no-op after a clean finish but sweeps the temp state a
                    // failed job left behind.
                    File(record.archivePath).delete()
                    purgeJob(record.jobId, record.outputDir)
                    PendingJobStore.clear(context)
                    call.resolve(JSObject().put("cleared", true))
                }
            }
        }
    }

    /**
     * Hard cancellation of the background job (P1-6): stops the WorkManager
     * chain, clears the durable record, and wipes the job's disk footprint
     * (.checkpoint, .index.redb, .tiledata.tmp, .pmtiles.tmp via purgeJob,
     * plus any assembled archive).
     *
     * Stop is slice-granular: an in-flight compileChunk cannot be
     * interrupted and will fsync one final checkpoint when it yields. Two
     * defenses cover that window: PendingJobStore's terminal transitions
     * are compare-and-set on the pending record (a late markFinished/
     * markFailed after clear() is a no-op), and a second purge sweep runs
     * after STRAGGLER_WINDOW_MS to remove the late checkpoint file.
     */
    @PluginMethod
    fun cancelBackgroundJob(call: PluginCall) {
        executor.execute {
            val record = PendingJobStore.loadAny(context)
            if (record != null && record.state != PendingJobStore.STATE_PENDING) {
                call.reject(
                    "Job ${record.jobId} is ${record.state}; acknowledge it instead of cancelling"
                )
                return@execute
            }

            // Order matters: stop the chain, then clear the record (the CAS
            // guard in PendingJobStore makes any still-running slice's
            // terminal write a no-op from here on), then purge the files.
            BackgroundCompileWorker.cancel(context)
            PendingJobStore.clear(context)

            val result = JSObject().put("cancelled", true)
            if (record != null) {
                purgeJob(record.jobId, record.outputDir)
                File(record.archivePath).delete()
                scheduleStragglerSweep(record.jobId, record.outputDir, record.archivePath)
                result.put("jobId", record.jobId)
                Log.i(TAG, "background job ${record.jobId} cancelled and purged")
            } else {
                Log.i(TAG, "background cancel requested with empty store; work chain stopped")
            }
            call.resolve(result)
        }
    }

    /**
     * Second purge pass after the in-flight slice window has certainly
     * closed — the running slice may recreate `{jobId}.checkpoint` (its
     * final durable yield) after the first purge. The Handler only
     * dispatches; file I/O stays on the plugin's background lane.
     */
    private fun scheduleStragglerSweep(jobId: String, outputDir: String, archivePath: String) {
        Handler(Looper.getMainLooper()).postDelayed({
            if (executor.isShutdown) return@postDelayed // plugin destroyed (D007)
            executor.execute {
                val sweptState = purgeJob(jobId, outputDir)
                val sweptArchive = File(archivePath).delete()
                if (sweptState || sweptArchive) {
                    Log.i(TAG, "straggler sweep for $jobId removed late writes")
                }
            }
        }, BackgroundCompileWorker.STRAGGLER_WINDOW_MS)
    }

    /** Adapts the UniFFI callback interface onto Capacitor's event emitter. */
    private fun bridgeForwardingCallback(): ProgressCallback =
        object : ProgressCallback {
            override fun onProgress(percentage: Float, status: String) {
                notifyListeners(
                    EVENT_PROGRESS,
                    JSObject().put("percentage", percentage.toDouble()).put("status", status)
                )
            }
        }

    private fun emitStatus(state: String, jobId: String, slices: Int) {
        notifyListeners(
            EVENT_STATUS,
            JSObject().put("state", state).put("jobId", jobId).put("slices", slices)
        )
    }

    companion object {
        private const val TAG = "MapCompilerPlugin"
        private const val EVENT_PROGRESS = "compilationProgress"
        private const val EVENT_STATUS = "compilationStatus"
        private const val EVENT_BACKGROUND = "backgroundCompile"

        /**
         * jobId names on-disk files under the sandbox (`{jobId}.pmtiles`,
         * `.checkpoint`, `.index.redb`). A `/`, `..`, or absolute path would
         * traverse out of it. The Rust FFI (`to_job_spec`) enforces the same
         * invariant as the authoritative choke point; this pre-flight fails
         * fast with a clear reject instead of surfacing as a compile `Failed`,
         * and covers queryJob (which bypasses `to_job_spec`).
         */
        private val SAFE_JOB_ID = Regex("^[A-Za-z0-9_-]{1,128}$")
        private const val UNSAFE_JOB_ID_MESSAGE =
            "Invalid jobId: only [A-Za-z0-9_-] allowed, max 128 chars"

        private fun isSafeJobId(jobId: String): Boolean = SAFE_JOB_ID.matches(jobId)

        /**
         * The live plugin instance, if a WebView is up. The background
         * worker uses it to surface terminal events to the UI when (and
         * only when) there is a UI; a headless WorkManager run has no
         * WebView, and the JS layer discovers results via
         * `queryBackgroundJob` on resume.
         */
        @Volatile
        private var active: MapCompilerPlugin? = null

        /** No-op when no WebView exists (headless worker process). */
        fun emitBackgroundEvent(
            state: String,
            jobId: String,
            archivePath: String? = null,
            blocksTotal: Long? = null,
            bytesWritten: Long? = null,
            reason: String? = null,
        ) {
            val plugin = active ?: return
            val data = JSObject().put("state", state).put("jobId", jobId)
            archivePath?.let { data.put("archivePath", it) }
            blocksTotal?.let { data.put("blocksTotal", it) }
            bytesWritten?.let { data.put("bytesWritten", it) }
            reason?.let { data.put("reason", it) }
            plugin.emitBackground(data)
        }
    }
}
