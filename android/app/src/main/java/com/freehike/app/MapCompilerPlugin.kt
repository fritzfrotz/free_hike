package com.freehike.app

import android.util.Log
import com.getcapacitor.JSObject
import com.getcapacitor.Plugin
import com.getcapacitor.PluginCall
import com.getcapacitor.PluginMethod
import com.getcapacitor.annotation.CapacitorPlugin
import uniffi.freehike.ProgressCallback
import uniffi.freehike.compileChunk
import uniffi.freehike.emitTestProgress
import uniffi.freehike.engineVersion
import java.util.concurrent.Executors

/**
 * Layer 2 of the tri-layer bridge (Frontend UI -> Capacitor Plugin -> UniFFI -> Rust Core).
 *
 * Wraps the UniFFI-generated Kotlin bindings (package `uniffi.freehike`) and forwards
 * Rust-side [ProgressCallback] ticks to the WebView as Capacitor `compilationProgress`
 * events. The heavy lifting never crosses the JS bridge: JS sends a bbox string down,
 * and only small progress/status payloads flow back up.
 *
 * NOTE (operating manual, HITL gate): the wrapped FFI surface is the Phase 1 walking
 * skeleton. When the real chunked `compile_chunk(budget) -> Finished | Yielded` surface
 * lands, this plugin changes with it.
 */
@CapacitorPlugin(name = "MapCompiler")
class MapCompilerPlugin : Plugin() {

    /** Single background lane for FFI work — never block the WebView/main thread. */
    private val executor = Executors.newSingleThreadExecutor()

    override fun load() {
        // The UniFFI bindings load libfreehike_ffi.so themselves via JNA
        // (Native.load("freehike_ffi")) on first use. This explicit loadLibrary is a
        // belt-and-braces early failure: if the .so was not packaged into jniLibs for
        // this ABI we find out at plugin load, with a clear log line, instead of at
        // first FFI call inside a user gesture. dlopen is ref-counted, so loading
        // here and via JNA is safe.
        try {
            System.loadLibrary("freehike_ffi")
            Log.i(TAG, "libfreehike_ffi.so loaded (" + engineVersion() + ")")
        } catch (e: UnsatisfiedLinkError) {
            Log.e(TAG, "libfreehike_ffi.so missing from jniLibs — FFI calls will fail", e)
        }
    }

    /** Smoke test: proves the Rust core is loaded and callable. */
    @PluginMethod
    fun getEngineVersion(call: PluginCall) {
        executor.execute {
            try {
                val version = engineVersion()
                call.resolve(JSObject().put("version", version))
            } catch (t: Throwable) {
                call.reject("FFI engineVersion failed: ${t.message}", t as? Exception)
            }
        }
    }

    /**
     * Walking-skeleton compile entry point. Expects `bbox` as "west,south,east,north"
     * (WGS84). Returns the Rust core's JSON status envelope verbatim in `result`.
     */
    @PluginMethod
    fun startJob(call: PluginCall) {
        val bbox = call.getString("bbox")
        if (bbox.isNullOrBlank()) {
            call.reject("Missing required parameter: bbox (\"west,south,east,north\")")
            return
        }
        executor.execute {
            try {
                val result = compileChunk(bbox)
                call.resolve(JSObject().put("result", result))
            } catch (t: Throwable) {
                call.reject("FFI compileChunk failed: ${t.message}", t as? Exception)
            }
        }
    }

    /**
     * Debug method proving the Rust -> Kotlin -> WebView progress path: asks the core
     * to emit `steps` synthetic ticks, each forwarded as a `compilationProgress` event.
     */
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
                val payload = JSObject()
                    .put("percentage", percentage.toDouble())
                    .put("status", status)
                notifyListeners(EVENT_PROGRESS, payload)
            }
        }

    companion object {
        private const val TAG = "MapCompilerPlugin"
        private const val EVENT_PROGRESS = "compilationProgress"
    }
}
