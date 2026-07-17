package com.freehike.app

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.pm.ServiceInfo
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.ForegroundInfo
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkRequest
import androidx.work.WorkerParameters
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.freehike.CompilationStatus
import uniffi.freehike.ProgressCallback
import uniffi.freehike.ThermalState
import uniffi.freehike.compileChunk
import uniffi.freehike.thermalState

/**
 * The Android background window (P8.C3 §2–3) — the WorkManager mirror of
 * the iOS `BackgroundCompileScheduler`: drives 2000ms budget-yield slices
 * against the engine's durable checkpoint until the job reaches a terminal
 * state, WorkManager stops us, or thermal Critical tells us to hand the
 * window back and cool down.
 *
 * FGS binding: `setForeground` with `dataSync` promotes the worker to a
 * Foreground Service, lifting WorkManager's 10-minute cap to the
 * platform's dataSync allowance (6h per 24h on Android 14+ — the ceiling
 * ARCHITECTURE.md's checkpoint contract is built to survive). If promotion
 * is refused (API 31+ background-start restriction), the worker still runs
 * under the 10-minute cap: with 2s slices that is ~300 checkpointed slices
 * per window, and the retry chain continues from disk — degraded pace,
 * never lost work.
 */
class BackgroundCompileWorker(
    context: Context,
    params: WorkerParameters,
) : CoroutineWorker(context, params) {

    override suspend fun doWork(): Result = withContext(Dispatchers.IO) {
        // §1 requirement: report the PRE-EXISTING thermal level before the
        // first slice (a fresh worker process gets no change notification
        // for a device that was already hot), then keep listening.
        ThermalStateBridge.start(applicationContext)

        val pending = PendingJobStore.loadPending(applicationContext)
        if (pending == null) {
            Log.i(TAG, "no pending job; nothing to do")
            return@withContext Result.success()
        }

        try {
            setForeground(createForegroundInfo())
        } catch (t: Throwable) {
            // API 31+ refuses FGS promotion from some background states.
            // Not fatal: see class docs — the 10-minute lane still makes
            // durable progress.
            Log.w(TAG, "FGS promotion refused; continuing under the 10-minute cap", t)
        }

        val job = pending.toCompileJob()
        var slices = 0
        while (true) {
            // WorkManager's stop signal (constraint lost — e.g. charger
            // unplugged — or system shutdown of the worker). The engine's
            // last yield already fsync'd its checkpoint; simply not starting
            // another slice IS the graceful stop. WorkManager re-runs
            // constraint-stopped work by itself; the return value here is
            // ignored once stopped.
            if (isStopped) {
                Log.i(TAG, "worker stopped after $slices slices; checkpoint durable")
                return@withContext Result.retry()
            }

            val status = try {
                compileChunk(job, SLICE_BUDGET_MS, LoggingProgressSink)
            } catch (t: Throwable) {
                // UniFFI surfaces Rust panics as exceptions; treat like
                // CompilationStatus.Failed (fatal, no retry).
                PendingJobStore.markFailed(applicationContext, pending, "FFI panic: ${t.message}")
                Log.e(TAG, "compileChunk threw after $slices slices", t)
                return@withContext Result.failure()
            }
            slices += 1

            when (status) {
                is CompilationStatus.Yielded -> {
                    // Thermal governance: under Critical the engine yields
                    // after its one-block minimum on every call. Re-invoking
                    // in a tight loop would defeat the throttle — return
                    // retry() and let WorkManager's exponential backoff be
                    // the cooldown.
                    if (thermalState() == ThermalState.CRITICAL) {
                        Log.i(TAG, "thermal Critical after $slices slices; backing off to cool")
                        return@withContext Result.retry()
                    }
                }
                is CompilationStatus.Finished -> {
                    // The archive is already at its final sandbox path. OPFS
                    // is WebView-private storage (P7 seam), so the copy into
                    // OPFS belongs to the JS layer: flip the durable record
                    // and notify the UI if one is alive right now.
                    val s = status.summary
                    PendingJobStore.markFinished(applicationContext, pending, s)
                    Log.i(TAG, "job ${pending.jobId} finished in $slices slices: ${s.blocksTotal} blocks")
                    MapCompilerPlugin.emitBackgroundEvent(
                        state = "finished",
                        jobId = pending.jobId,
                        archivePath = pending.archivePath,
                        blocksTotal = s.blocksTotal.toLong(),
                        bytesWritten = s.bytesWritten.toLong(),
                    )
                    return@withContext Result.success()
                }
                is CompilationStatus.Failed -> {
                    // Fatal per the Surface v1 contract (bad input, corrupt
                    // state, disk). Do NOT retry — re-burning the failure on
                    // a charger overnight wastes battery and flash.
                    PendingJobStore.markFailed(applicationContext, pending, status.reason)
                    Log.e(TAG, "job ${pending.jobId} failed after $slices slices: ${status.reason}")
                    MapCompilerPlugin.emitBackgroundEvent(
                        state = "failed",
                        jobId = pending.jobId,
                        reason = status.reason,
                    )
                    return@withContext Result.failure()
                }
            }
        }
        @Suppress("UNREACHABLE_CODE")
        return@withContext Result.failure()
    }

    /**
     * The persistent "map is compiling" notification binding this worker to
     * a dataSync Foreground Service (Android 14+ requires the explicit
     * type, declared both here and on WorkManager's SystemForegroundService
     * in the manifest). Silent/low-importance: progress detail lives in the
     * app UI, the notification only satisfies the FGS visibility contract.
     */
    private fun createForegroundInfo(): ForegroundInfo {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val manager =
                applicationContext.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    "Offline map compilation",
                    NotificationManager.IMPORTANCE_LOW
                )
            )
        }
        val notification = NotificationCompat.Builder(applicationContext, CHANNEL_ID)
            .setContentTitle("Compiling offline map")
            .setContentText("Building hiking map tiles on this device")
            .setSmallIcon(applicationContext.applicationInfo.icon)
            .setOngoing(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            ForegroundInfo(
                NOTIFICATION_ID,
                notification,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
            )
        } else {
            ForegroundInfo(NOTIFICATION_ID, notification)
        }
    }

    /** Headless progress sink: logcat keeps field debugging honest. */
    private object LoggingProgressSink : ProgressCallback {
        override fun onProgress(percentage: Float, status: String) {
            Log.i(TAG, "BG compile ${percentage.toInt()}% — $status")
        }
    }

    companion object {
        private const val TAG = "BackgroundCompileWorker"
        private const val WORK_NAME = "freehike-background-compile"
        private const val CHANNEL_ID = "freehike_compile"
        private const val NOTIFICATION_ID = 4157
        private val SLICE_BUDGET_MS: UInt = 2_000u

        /**
         * Queues the (single) background compile. Charging-constrained —
         * the same honest "compiles while charging" posture as the iOS
         * request's requiresExternalPower; no network constraint (raw
         * PBF/DEM are already on disk). KEEP makes re-enqueue idempotent,
         * mirroring iOS's same-identifier submission.
         */
        fun enqueue(context: Context) {
            val request = OneTimeWorkRequestBuilder<BackgroundCompileWorker>()
                .setConstraints(Constraints.Builder().setRequiresCharging(true).build())
                .setBackoffCriteria(
                    BackoffPolicy.EXPONENTIAL,
                    WorkRequest.MIN_BACKOFF_MILLIS,
                    TimeUnit.MILLISECONDS
                )
                .build()
            WorkManager.getInstance(context.applicationContext)
                .enqueueUniqueWork(WORK_NAME, ExistingWorkPolicy.KEEP, request)
            Log.i(TAG, "background compile window requested")
        }
    }
}
