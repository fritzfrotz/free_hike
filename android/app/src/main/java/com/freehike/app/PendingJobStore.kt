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
            .commit()
    }

    /** The record in any state (resume-time discovery), or null if idle. */
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
        )
    }

    /** The record only if compile work remains for a background window. */
    fun loadPending(context: Context): Record? =
        loadAny(context)?.takeIf { it.state == STATE_PENDING }

    fun markFinished(context: Context, record: Record, summary: CompileSummary) {
        save(
            context,
            record.copy(
                state = STATE_FINISHED,
                blocksTotal = summary.blocksTotal.toLong(),
                bytesWritten = summary.bytesWritten.toLong(),
            )
        )
    }

    fun markFailed(context: Context, record: Record, reason: String) {
        save(context, record.copy(state = STATE_FAILED, reason = reason))
    }

    @SuppressLint("ApplySharedPref")
    fun clear(context: Context) {
        prefs(context).edit().clear().commit()
    }
}
