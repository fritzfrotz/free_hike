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
import uniffi.freehike.ThermalState
import uniffi.freehike.compileChunk
import uniffi.freehike.purgeJob
import uniffi.freehike.thermalState

// DEBT(D004): iOS build/link and device smokes need an Xcode machine, latest FFI enum-split changes not compile-verified. The iOS background compile shows Queued and no percent while pending, its sink saves no progress (P9.C8) — platforms: ios,android
// DEBT(D018): notification permission is never requested, so on Android 13+ the compile's foreground-service notification is suppressed and none shows (RC2 log 12:37:54) — platforms: android
/**
 * The Android background window (P8.C3 §2–3) — the WorkManager mirror of
 * the iOS `BackgroundCompileScheduler`: drives 2000ms budget-yield slices
 * against the engine's durable checkpoint until the job reaches a terminal
 * state, WorkManager stops us, or thermal Critical tells us to hand the
 * window back and cool down.
 *
 * FGS binding: `setForeground` with `dataSync` asks for a Foreground
 * Service, which lifts JobScheduler's per-job limit to the platform's
 * dataSync allowance (6h per 24h on Android 15+ — the ceiling
 * ARCHITECTURE.md's checkpoint contract is built to survive). WorkManager
 * 2.10.2 hands the request to its own SystemForegroundService and returns:
 * a refusal there is caught and logged inside that service, never thrown
 * here, and WorkManager still treats the work as foreground — it ignores
 * JobScheduler's stop and keeps its wake lock. There is no 10-minute retry
 * lane in this library version: an unpromoted run goes on for as long as
 * the process stays alive (D013 in LOOPLOG). Only a refused service start
 * throws out of `setForeground`. Work is never lost either way — every
 * slice ends on a durable checkpoint.
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

        val loaded = PendingJobStore.loadPending(applicationContext)
        if (loaded == null) {
            Log.i(TAG, "no pending job; nothing to do")
            return@withContext Result.success()
        }

        // ── Circuit breaker ──────────────────────────────────────────────
        // A run whose predecessor set cleanStop (deliberate thermal retry /
        // constraint stop) is healthy; one whose predecessor died without
        // it — SIGBUS on the mmap, LMK kill, Rust abort — counts as dirty.
        // runAttemptCount alone can't tell these apart: it also increments
        // on our own deliberate Result.retry()s, so a cool-down loop on a
        // hot device would falsely trip the breaker.
        val record = loaded.copy(
            cleanStop = false,
            dirtyAttempts = if (loaded.cleanStop) loaded.dirtyAttempts else loaded.dirtyAttempts + 1,
        )
        PendingJobStore.save(applicationContext, record)

        if (record.dirtyAttempts > MAX_DIRTY_ATTEMPTS) {
            val reason = "Background compile aborted: $MAX_DIRTY_ATTEMPTS consecutive runs died " +
                "without completing a slice (likely corrupt input or memory pressure)."
            Log.e(TAG, "circuit breaker tripped for ${record.jobId} " +
                "(runAttemptCount=$runAttemptCount): $reason")
            PendingJobStore.markFailed(applicationContext, record, reason)
            // Release the dead job's disk claim: checkpoint, redb index,
            // tile-data scratch, half-written archive temp.
            purgeJob(record.jobId, record.outputDir)
            MapCompilerPlugin.emitBackgroundEvent(
                state = "failed",
                jobId = record.jobId,
                reason = reason,
            )
            return@withContext Result.failure()
        }

        // DEBT(D016): a boot start (WorkManager's boot receiver) gets no foreground service on Android 15+ — the dataSync start of a boot-started app is refused; inferred from usage stats, not observed (RC2 buffer for that process starts 12:43:23) — platforms: android
        try {
            setForeground(createForegroundInfo())
        } catch (t: Throwable) {
            // Only a refused service start lands here (API 31+ background-
            // start restriction); a refusal inside WorkManager's service
            // never does — see class docs. Not fatal: the run goes on and
            // every slice checkpoints.
            Log.w(TAG, "FGS promotion refused; continuing under the 10-minute cap", t)
        }

        val job = record.toCompileJob()
        // P10.C1: per-slice lines only in debuggable builds; a release
        // build logs progress once per integer percent.
        val verbose = DebugBuild.isDebuggable(applicationContext)
        // P9.C8: logcat line plus the saved progress point the WebView polls.
        val progressSink = CompileProgressSink(
            jobId = record.jobId,
            clock = System::currentTimeMillis,
            save = { PendingJobStore.saveProgress(applicationContext, it) },
            log = { Log.i(TAG, it) },
            verbose = verbose,
        )
        var slices = 0
        while (true) {
            // WorkManager's stop signal (constraint lost — e.g. charger
            // unplugged — or system shutdown of the worker). The engine's
            // last yield already fsync'd its checkpoint; simply not starting
            // another slice IS the graceful stop. WorkManager re-runs
            // constraint-stopped work by itself; the return value here is
            // ignored once stopped.
            if (isStopped) {
                PendingJobStore.updatePending(applicationContext, record.jobId) {
                    it.copy(cleanStop = true)
                }
                Log.i(TAG, "worker stopped after $slices slices; checkpoint durable")
                return@withContext Result.retry()
            }

            val status = try {
                compileChunk(job, SLICE_BUDGET_MS, progressSink)
            } catch (t: Throwable) {
                // UniFFI surfaces Rust panics as exceptions; treat like
                // CompilationStatus.FailedFatal (no retry) and release the
                // job's temporary disk state.
                val reason = "FFI panic: ${t.message}"
                val stillOurs = PendingJobStore.markFailed(applicationContext, record, reason)
                purgeJob(record.jobId, record.outputDir)
                Log.e(TAG, "compileChunk threw after $slices slices", t)
                if (stillOurs) {
                    MapCompilerPlugin.emitBackgroundEvent(
                        state = "failed",
                        jobId = record.jobId,
                        reason = reason,
                    )
                }
                return@withContext Result.failure()
            }
            slices += 1
            // P9.C11: one memory point per slice for the device capture's
            // curve against the 50 MB budget. Logging only; never throws.
            // Debuggable builds only (P10.C1).
            if (verbose) CompileMemoryLog.sample(slices)?.let { Log.i(TAG, it) }

            if (slices == 1) {
                // The FFI survived a full slice, so this run is not part of
                // a crash loop: consecutive-death accounting starts over. A
                // job poisoned at a fixed input byte never reaches this line
                // on later runs — the durable checkpoint resumes it right at
                // the poison, so it dies inside its FIRST slice every time
                // and the breaker trips after MAX_DIRTY_ATTEMPTS runs.
                PendingJobStore.updatePending(applicationContext, record.jobId) {
                    it.copy(dirtyAttempts = 0)
                }
            }

            when (status) {
                is CompilationStatus.Yielded -> {
                    // Thermal governance: under Critical the engine yields
                    // after its one-block minimum on every call. Re-invoking
                    // in a tight loop would defeat the throttle — return
                    // retry() and let WorkManager's exponential backoff be
                    // the cooldown. Marked clean so the breaker never counts
                    // cool-down cycles as failures.
                    if (thermalState() == ThermalState.CRITICAL) {
                        PendingJobStore.updatePending(applicationContext, record.jobId) {
                            it.copy(cleanStop = true)
                        }
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
                    val stillOurs = PendingJobStore.markFinished(applicationContext, record, s)
                    if (!stillOurs) {
                        // Hard-cancelled while this slice ran: the record is
                        // gone and the canceller owns cleanup. Do NOT
                        // resurrect the record or announce the result.
                        Log.i(TAG, "job ${record.jobId} finished but was cancelled mid-slice; dropping result")
                        return@withContext Result.success()
                    }
                    Log.i(TAG, "job ${record.jobId} finished in $slices slices: ${s.blocksTotal} blocks")
                    MapCompilerPlugin.emitBackgroundEvent(
                        state = "finished",
                        jobId = record.jobId,
                        archivePath = record.archivePath,
                        blocksTotal = s.blocksTotal.toLong(),
                        bytesWritten = s.bytesWritten.toLong(),
                    )
                    return@withContext Result.success()
                }
                is CompilationStatus.FailedFatal -> {
                    // Fatal per the Surface v1 contract (bad input, corrupt
                    // state). Do NOT retry — re-burning the failure on a
                    // charger overnight wastes battery and flash — and
                    // release the temporary disk state immediately.
                    val stillOurs = PendingJobStore.markFailed(applicationContext, record, status.reason)
                    purgeJob(record.jobId, record.outputDir)
                    Log.e(TAG, "job ${record.jobId} failed after $slices slices: ${status.reason}")
                    if (stillOurs) {
                        MapCompilerPlugin.emitBackgroundEvent(
                            state = "failed",
                            jobId = record.jobId,
                            reason = status.reason,
                        )
                    }
                    return@withContext Result.failure()
                }
                is CompilationStatus.FailedTransient -> {
                    // The environment refused the slice (another runner holds
                    // the job's slice lock). Durable state is untouched — do
                    // NOT purge, do NOT mark failed. Back off and let the
                    // retry chain re-enter once the contention clears; marked
                    // clean so the circuit breaker never counts it.
                    PendingJobStore.updatePending(applicationContext, record.jobId) {
                        it.copy(cleanStop = true)
                    }
                    Log.w(TAG, "job ${record.jobId} transient refusal after $slices slices: ${status.reason}")
                    return@withContext Result.retry()
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

    companion object {
        private const val TAG = "BackgroundCompileWorker"
        private const val WORK_NAME = "freehike-background-compile"
        private const val CHANNEL_ID = "freehike_compile"
        private const val NOTIFICATION_ID = 4157
        private val SLICE_BUDGET_MS: UInt = 2_000u

        /**
         * Circuit breaker (P0-2): consecutive runs allowed to die without
         * completing a single slice before the job is declared poisoned.
         * "Die" means process death mid-FFI — SIGBUS from the mmap'd PBF,
         * an LMK/OOM kill, a Rust abort — which no in-process catch can
         * see; the only witness is the absence of the cleanStop marker on
         * the next run. Without this cap, WorkManager reschedules the dead
         * RUNNING worker forever: a crash loop on every charging session.
         */
        internal const val MAX_DIRTY_ATTEMPTS = 5

        /**
         * Upper bound on how long an already-running slice can keep
         * touching job files after a stop/cancel request: the blocking FFI
         * call cannot be interrupted, so it runs out its budget (plus the
         * engine's one-block overrun) and then fsyncs one final checkpoint.
         * Cancellation sweeps the job directory a second time after this
         * window to catch that straggler write.
         */
        internal val STRAGGLER_WINDOW_MS: Long = SLICE_BUDGET_MS.toLong() * 2 + 1_000

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

        /**
         * Hard cancellation (P1-6): stops the unique work chain. The worker
         * honors the stop at its next slice boundary (≤ [STRAGGLER_WINDOW_MS]);
         * record clearing and disk purge are the caller's responsibility
         * (MapCompilerPlugin.cancelBackgroundJob owns that sequence).
         */
        fun cancel(context: Context) {
            WorkManager.getInstance(context.applicationContext).cancelUniqueWork(WORK_NAME)
            Log.i(TAG, "background compile work cancelled")
        }
    }
}
