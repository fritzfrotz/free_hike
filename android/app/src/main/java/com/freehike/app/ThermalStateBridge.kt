package com.freehike.app

import android.content.Context
import android.os.Build
import android.os.PowerManager
import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean
import uniffi.freehike.ThermalState
import uniffi.freehike.setThermalState

/**
 * Mirrors Android's thermal status into the Rust core's global thermal flag
 * (P8.C3 §1). The Rust side is a single lock-free atomic store, safe from
 * any thread at any time — including while compileChunk runs on another
 * thread; running slices pick the change up at their next block boundary.
 *
 * The thermal API is API 29+ (minSdk is 24): below Q there is no status to
 * read, so the core simply stays at its Nominal default — full-speed, the
 * same behavior those devices had before governance existed.
 */
object ThermalStateBridge {

    private const val TAG = "ThermalStateBridge"
    private val listenerRegistered = AtomicBoolean(false)

    /**
     * Pushes the CURRENT status immediately (the listener only covers
     * CHANGES — a worker can start in a fresh process on an already-hot
     * device), then registers the change listener once per process.
     * Idempotent; called from both plugin load (WebView path) and worker
     * start (headless path), whichever comes first.
     */
    fun start(context: Context) {
        pushCurrentStatus(context)
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
        if (!listenerRegistered.compareAndSet(false, true)) return
        val pm = context.applicationContext.getSystemService(Context.POWER_SERVICE) as PowerManager
        // Direct executor: the callback body is one atomic store + one log
        // line, so hopping to another thread would only add latency.
        pm.addThermalStatusListener({ it.run() }) { status -> push(status) }
    }

    /**
     * Reads the live thermal status and publishes it to the Rust core —
     * the explicit "poll once at task start" hook (P8.C3 §1).
     */
    fun pushCurrentStatus(context: Context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
        val pm = context.applicationContext.getSystemService(Context.POWER_SERVICE) as PowerManager
        push(pm.currentThermalStatus)
    }

    private fun push(status: Int) {
        val state = map(status)
        setThermalState(state)
        Log.i(TAG, "thermal status $status -> $state")
    }

    /**
     * Collapse mapping per the FFI contract (ffi/src/lib.rs doc comment):
     * NONE -> Nominal, LIGHT -> Fair, MODERATE -> Serious, SEVERE and every
     * hotter level (CRITICAL / EMERGENCY / SHUTDOWN, plus any future
     * addition) -> Critical. Unknown-hot fails COOL, mirroring the Rust
     * core's unknown-byte rule.
     */
    fun map(status: Int): ThermalState = when {
        status <= PowerManager.THERMAL_STATUS_NONE -> ThermalState.NOMINAL
        status == PowerManager.THERMAL_STATUS_LIGHT -> ThermalState.FAIR
        status == PowerManager.THERMAL_STATUS_MODERATE -> ThermalState.SERIOUS
        else -> ThermalState.CRITICAL
    }
}
