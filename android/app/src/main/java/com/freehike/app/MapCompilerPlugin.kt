package com.freehike.app

import android.util.Log
import com.getcapacitor.JSObject
import com.getcapacitor.Plugin
import com.getcapacitor.PluginCall
import com.getcapacitor.PluginMethod
import com.getcapacitor.annotation.CapacitorPlugin
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

    /** Cancellation flag for the active job, checked between slices. */
    private val cancelRequested = AtomicBoolean(false)

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

        cancelRequested.set(false)
        executor.execute {
            try {
                var slices = 0
                while (true) {
                    if (cancelRequested.get()) {
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
                        is CompilationStatus.Failed -> {
                            Log.e(TAG, "job $jobId failed after $slices slices: ${status.reason}")
                            emitStatus("failed", jobId, slices)
                            call.resolve(
                                JSObject()
                                    .put("status", "failed")
                                    .put("jobId", jobId)
                                    .put("slices", slices)
                                    .put("reason", status.reason)
                            )
                            return@execute
                        }
                    }
                }
            } catch (t: Throwable) {
                call.reject("FFI compileChunk failed: ${t.message}", t as? Exception)
            }
        }
    }

    /** Requests cancellation of the active job (honored between slices). */
    @PluginMethod
    fun cancelJob(call: PluginCall) {
        cancelRequested.set(true)
        call.resolve(JSObject().put("requested", true))
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
    }
}
