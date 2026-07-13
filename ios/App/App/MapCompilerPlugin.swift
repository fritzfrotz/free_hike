import Foundation
import Capacitor

/// Layer 2 of the tri-layer bridge (Frontend UI -> Capacitor Plugin -> UniFFI -> Rust Core).
///
/// Wraps the UniFFI-generated Swift bindings (`FreeHikeFFI/freehike.swift`, compiled into
/// this target via the `freehikeFFI.h` bridging header) and forwards Rust-side
/// `ProgressCallback` ticks to the WebView as Capacitor `compilationProgress` events.
/// Bulk data never crosses the JS bridge: a bbox string goes down, small progress/status
/// payloads come back up.
///
/// NOTE (operating manual, HITL gate): the wrapped FFI surface is the Phase 1 walking
/// skeleton. When the real chunked `compile_chunk(budget) -> Finished | Yielded` surface
/// lands, this plugin changes with it.
@objc(MapCompilerPlugin)
public class MapCompilerPlugin: CAPPlugin, CAPBridgedPlugin {
    public let identifier = "MapCompilerPlugin"
    public let jsName = "MapCompiler"
    public let pluginMethods: [CAPPluginMethod] = [
        CAPPluginMethod(name: "startJob", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "getEngineVersion", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "emitTestProgress", returnType: CAPPluginReturnPromise),
    ]

    /// Single background lane for FFI work — never block the WebView/main thread.
    private let ffiQueue = DispatchQueue(label: "app.freehike.mapcompiler.ffi", qos: .utility)

    /// Smoke test: proves the Rust core is linked and callable.
    @objc func getEngineVersion(_ call: CAPPluginCall) {
        ffiQueue.async {
            call.resolve(["version": engineVersion()])
        }
    }

    /// Walking-skeleton compile entry point. Expects `bbox` as
    /// "west,south,east,north" (WGS84). Returns the Rust core's JSON status
    /// envelope verbatim in `result`.
    @objc func startJob(_ call: CAPPluginCall) {
        guard let bbox = call.getString("bbox"), !bbox.isEmpty else {
            call.reject("Missing required parameter: bbox (\"west,south,east,north\")")
            return
        }
        ffiQueue.async {
            let result = compileChunk(bbox: bbox)
            call.resolve(["result": result])
        }
    }

    /// Debug method proving the Rust -> Swift -> WebView progress path: asks the
    /// core to emit `steps` synthetic ticks, each forwarded as a
    /// `compilationProgress` event.
    @objc func emitTestProgress(_ call: CAPPluginCall) {
        let steps = call.getInt("steps") ?? 5
        guard steps >= 0 else {
            call.reject("steps must be >= 0")
            return
        }
        let forwarder = BridgeForwardingProgress(plugin: self)
        ffiQueue.async {
            let sent = ffiEmitTestProgress(forwarder, UInt32(steps))
            call.resolve(["sent": Int(sent)])
        }
    }
}

/// Adapts the UniFFI callback interface onto Capacitor's event emitter.
/// `@unchecked Sendable`: the generated `ProgressCallback` protocol requires
/// `Sendable`; the only state is a weak plugin reference, and
/// `notifyListeners` is thread-safe on Capacitor's side.
private final class BridgeForwardingProgress: ProgressCallback, @unchecked Sendable {
    private weak var plugin: CAPPlugin?

    init(plugin: CAPPlugin) {
        self.plugin = plugin
    }

    func onProgress(percentage: Float, status: String) {
        plugin?.notifyListeners(
            "compilationProgress",
            data: ["percentage": percentage, "status": status]
        )
    }
}

/// Top-level trampoline to the UniFFI free function `emitTestProgress(callback:steps:)`.
/// Called from inside the plugin class, whose own `emitTestProgress(_:)` member would
/// otherwise shadow the free function during name lookup.
private func ffiEmitTestProgress(_ callback: ProgressCallback, _ steps: UInt32) -> UInt32 {
    emitTestProgress(callback: callback, steps: steps)
}
