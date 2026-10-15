package com.freehike.app

import uniffi.freehike.ProgressCallback

/** One saved progress point of a background compile (P9.C8). */
data class CompileProgress(
    val jobId: String,
    /** Integer 0–100 across the whole job. */
    val percent: Int,
    /** The engine's status line at this point. */
    val status: String,
    /** Device wall-clock ms of the save. */
    val atMs: Long,
)

/**
 * The background worker's progress sink (P9.C8). Writes the logcat line —
 * every callback when [verbose] (debuggable builds), else once per integer
 * percent (P10.C1: a release build carries no per-slice line, but the
 * percent timeline stays in a capture) — and
 * hands a [CompileProgress] to [save] on integer change or once
 * [HEARTBEAT_MS] have passed since the last save — the WebView polls the
 * saved point and calls a job paused when it is more than 15 s old, so a
 * slow percent must still refresh the timestamp.
 *
 * Runs INSIDE Rust's FFI callback, mid-slice: anything thrown here would
 * come back out of compileChunk, and the worker treats that as an FFI panic
 * (job marked failed and purged). The whole body is therefore guarded; a
 * failed save is simply retried on the next callback.
 *
 * Free of Android types so the JVM unit test can drive it.
 */
class CompileProgressSink(
    private val jobId: String,
    private val clock: () -> Long,
    private val save: (CompileProgress) -> Unit,
    private val log: (String) -> Unit,
    private val verbose: Boolean,
) : ProgressCallback {

    private var lastPercent = -1
    private var lastLoggedPercent = -1
    private var lastSaveMs = 0L

    override fun onProgress(percentage: Float, status: String) {
        try {
            val percent = percentage.toInt().coerceIn(0, 100)
            if (verbose || percent != lastLoggedPercent) {
                lastLoggedPercent = percent
                runCatching { log("BG compile $percent% — $status") }
            }
            val now = clock()
            if (percent != lastPercent || now - lastSaveMs >= HEARTBEAT_MS) {
                save(CompileProgress(jobId, percent, status, now))
                lastPercent = percent
                lastSaveMs = now
            }
        } catch (_: Throwable) {
            // Display only: never let it reach the engine.
        }
    }

    companion object {
        const val HEARTBEAT_MS = 5_000L
    }
}
