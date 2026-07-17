package com.freehike.app

import android.os.Bundle
import android.util.Log
import com.getcapacitor.BridgeActivity

/**
 * Root activity (P8.C4: converted from Java to Kotlin to host the JNI
 * static-init block and the boot-time thermal hook).
 *
 * Initialization order is load-bearing:
 * 1. Class load → companion `init` eagerly links libfreehike_ffi.so, so
 *    every later FFI touch (thermal push, worker, plugin) finds the
 *    symbols already resolved.
 * 2. `registerPlugin` BEFORE `super.onCreate` — Capacitor binds app-local
 *    plugins during bridge initialization.
 * 3. `ThermalStateBridge.start` BEFORE `super.onCreate` — thermal
 *    governance is armed before the heavy part of boot (WebView + bridge
 *    spin-up), not after it. The bridge is idempotent, so the plugin's own
 *    `load()` call and the background worker's call remain harmless
 *    re-arms; they cover process entry points that never pass through this
 *    activity (a headless WorkManager start has no MainActivity at all).
 */
class MainActivity : BridgeActivity() {

    public override fun onCreate(savedInstanceState: Bundle?) {
        registerPlugin(MapCompilerPlugin::class.java)

        try {
            ThermalStateBridge.start(applicationContext)
        } catch (t: Throwable) {
            // Only reachable when the .so is missing (JNA links lazily, so
            // the first FFI call is where a bad packaging surfaces). Keep
            // the app booting — the map UI still works without the
            // compiler; every compile entry point logs its own failure.
            Log.e(TAG, "thermal bridge unavailable at boot", t)
        }

        super.onCreate(savedInstanceState)
    }

    companion object {
        private const val TAG = "MainActivity"

        // Eager JNI load at class initialization. The library name is the
        // Rust cdylib artifact libfreehike_ffi.so — NOT "freehike": the
        // UniFFI bindings bind the freehike_ffi crate via JNA, and a wrong
        // name here would abort class load (ExceptionInInitializerError)
        // before onCreate ever runs. Failure is logged, not thrown, so a
        // broken ABI split degrades to a map-only app instead of a
        // boot-loop crash.
        init {
            try {
                System.loadLibrary("freehike_ffi")
                Log.i(TAG, "libfreehike_ffi.so linked at class init")
            } catch (e: UnsatisfiedLinkError) {
                Log.e(TAG, "libfreehike_ffi.so missing from jniLibs — compiler disabled", e)
            }
        }
    }
}
