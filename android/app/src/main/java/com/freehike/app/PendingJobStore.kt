package com.freehike.app

import android.annotation.SuppressLint
import android.content.Context
import uniffi.freehike.CompileJob
import uniffi.freehike.CompileSummary

/**
 * Durable record of the one queued/terminal background job — the Android
 * mirror of the iOS `PendingJobStore` (P8.C2), backed by SharedPreferences.
 * WorkManager can run the worker in a fresh process with no WebView, so the
 * job spec must survive process death. Single-job by design: Surface v1
 * compiles one region at a time; a queue is product-layer territory
 * (Phase 9).
 *
 * Writes use synchronous commit(), not apply(): the worker's process may be
 * killed right after a state change, and an unflushed async write would
 * lose the terminal marker the JS layer discovers on resume.
 *
 * Concurrency: the worker coroutine (Dispatchers.IO) and the plugin's
 * executor lane both mutate this store, so every mutation is
 * `@Synchronized` and every terminal transition goes through
 * [updatePending] — a compare-and-set on `(jobId, state == pending)`.
 * That guard is what makes hard cancellation safe: a worker slice that was
 * already in flight when `cancelBackgroundJob` cleared the record cannot
 * resurrect it by persisting a late finished/failed marker.
 */
object PendingJobStore {

    const val STATE_PENDING = "pending"
    const val STATE_FINISHED = "finished"
    const val STATE_FAILED = "failed"

    private const val PREFS = "freehike_background_job"

    data class Record(
        val state: String,
        val jobId: String,
        val bbox: String,
        val minZoom: Int,
        val maxZoom: Int,
        val pbfPath: String,
        val demPath: String?,
        val outputDir: String,
        val reason: String?,
        val blocksTotal: Long,
        val bytesWritten: Long,
        /**
         * Circuit-breaker counter (P0-2): runs of the worker whose
         * PREDECESSOR run ended without passing through a deliberate exit
         * (thermal retry / constraint stop / terminal state). A predecessor
         * that died mid-slice — SIGBUS on the mmap, LMK kill, Rust abort —
         * never got to set [cleanStop], so its successor counts as dirty.
         * Reset to 0 the first time a run completes a compile slice.
         */
        val dirtyAttempts: Int = 0,
        /**
         * Set just before the worker deliberately returns Result.retry()
         * (thermal Critical) or observes isStopped (constraint lost). The
         * next run consumes and clears it; if it is absent at run start,
         * the previous run died and [dirtyAttempts] is incremented.
         */
        val cleanStop: Boolean = false,
    ) {
        fun toCompileJob() = CompileJob(
            jobId = jobId,
            bbox = bbox,
            minZoom = minZoom.toUByte(),
            maxZoom = maxZoom.toUByte(),
            pbfPath = pbfPath,
            demPath = demPath,
            outputDir = outputDir,
        )

        val archivePath: String get() = "$outputDir/$jobId.pmtiles"
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    @SuppressLint("ApplySharedPref")
    @Synchronized
    fun save(context: Context, record: Record) {
        prefs(context).edit()
            .putString("state", record.state)
            .putString("jobId", record.jobId)
            .putString("bbox", record.bbox)
            .putInt("minZoom", record.minZoom)
            .putInt("maxZoom", record.maxZoom)
            .putString("pbfPath", record.pbfPath)
            .putString("demPath", record.demPath)
            .putString("outputDir", record.outputDir)
            .putString("reason", record.reason)
            .putLong("blocksTotal", record.blocksTotal)
            .putLong("bytesWritten", record.bytesWritten)
            .putInt("dirtyAttempts", record.dirtyAttempts)
            .putBoolean("cleanStop", record.cleanStop)
            .commit()
    }

    /** The record in any state (resume-time discovery), or null if idle. */
    @Synchronized
    fun loadAny(context: Context): Record? {
        val p = prefs(context)
        val state = p.getString("state", null) ?: return null
        return Record(
            state = state,
            jobId = p.getString("jobId", "") ?: "",
            bbox = p.getString("bbox", "") ?: "",
            minZoom = p.getInt("minZoom", 5),
            maxZoom = p.getInt("maxZoom", 14),
            pbfPath = p.getString("pbfPath", "") ?: "",
            demPath = p.getString("demPath", null),
            outputDir = p.getString("outputDir", "") ?: "",
            reason = p.getString("reason", null),
            blocksTotal = p.getLong("blocksTotal", 0),
            bytesWritten = p.getLong("bytesWritten", 0),
            dirtyAttempts = p.getInt("dirtyAttempts", 0),
            cleanStop = p.getBoolean("cleanStop", false),
        )
    }

    /** The record only if compile work remains for a background window. */
    fun loadPending(context: Context): Record? =
        loadAny(context)?.takeIf { it.state == STATE_PENDING }

    /**
     * Compare-and-set: applies `transform` only while the store still holds
     * the PENDING record for `jobId`. Returns false — and writes nothing —
     * if the record was cleared (hard cancel), overwritten, or already
     * terminal. Every worker-side mutation routes through here so a
     * cancelled job can never be resurrected by a late write.
     */
    @Synchronized
    fun updatePending(context: Context, jobId: String, transform: (Record) -> Record): Boolean {
        val current = loadAny(context) ?: return false
        if (current.jobId != jobId || current.state != STATE_PENDING) return false
        save(context, transform(current))
        return true
    }

    /** Terminal success transition; false if the record is gone (cancelled). */
    fun markFinished(context: Context, record: Record, summary: CompileSummary): Boolean =
        updatePending(context, record.jobId) {
            it.copy(
                state = STATE_FINISHED,
                blocksTotal = summary.blocksTotal.toLong(),
                bytesWritten = summary.bytesWritten.toLong(),
            )
        }

    /** Terminal failure transition; false if the record is gone (cancelled). */
    fun markFailed(context: Context, record: Record, reason: String): Boolean =
        updatePending(context, record.jobId) { it.copy(state = STATE_FAILED, reason = reason) }

    @SuppressLint("ApplySharedPref")
    @Synchronized
    fun clear(context: Context) {
        prefs(context).edit().clear().commit()
    }
}
