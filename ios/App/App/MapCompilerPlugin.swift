import Foundation
import Capacitor

/// Layer 2 of the tri-layer bridge — Surface v1 (suspendable state machine).
///
/// `startJob` drives the budget-yield loop natively: `compileChunk` is
/// re-invoked with the same `CompileJob` while the engine returns `.yielded`,
/// honoring cancellation between slices. The engine owns all resume state on
/// disk (fsync'd checkpoint keyed by jobId); this layer never round-trips
/// state — which is exactly what makes the iOS 295-second BGProcessingTask
/// guillotine survivable: in production each loop iteration becomes one
/// BGTask slice, and a SIGKILL between slices loses nothing.
@objc(MapCompilerPlugin)
public class MapCompilerPlugin: CAPPlugin, CAPBridgedPlugin {
    public let identifier = "MapCompilerPlugin"
    public let jsName = "MapCompiler"
    public let pluginMethods: [CAPPluginMethod] = [
        CAPPluginMethod(name: "startJob", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "cancelJob", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "getEngineVersion", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "emitTestProgress", returnType: CAPPluginReturnPromise),
    ]

    /// Single background lane for FFI work — never block the WebView/main thread.
    private let ffiQueue = DispatchQueue(label: "app.freehike.mapcompiler.ffi", qos: .utility)

    /// Cancellation flag for the active job, checked between slices.
    /// Accessed from the main thread (cancelJob) and ffiQueue (loop); NSLock
    /// keeps it simple and audit-friendly.
    private let cancelLock = NSLock()
    private var cancelRequested = false

    private func setCancel(_ value: Bool) {
        cancelLock.lock()
        cancelRequested = value
        cancelLock.unlock()
    }

    private func isCancelRequested() -> Bool {
        cancelLock.lock()
        defer { cancelLock.unlock() }
        return cancelRequested
    }

    /// Smoke test: proves the Rust core is linked and callable.
    @objc func getEngineVersion(_ call: CAPPluginCall) {
        ffiQueue.async {
            call.resolve(["version": engineVersion()])
        }
    }

    /// Runs a compile job to completion (or failure/cancellation) via the
    /// budget-yield loop. Progress streams as `compilationProgress` events;
    /// each slice boundary emits a `compilationStatus` event; the call
    /// resolves with the terminal status.
    @objc func startJob(_ call: CAPPluginCall) {
        guard let bbox = call.getString("bbox"), !bbox.isEmpty else {
            call.reject("Missing required parameter: bbox (\"west,south,east,north\")")
            return
        }
        let jobId = call.getString("jobId") ?? UUID().uuidString
        let budgetMs = UInt32(max(0, min(call.getInt("budgetMs") ?? 250, 600_000)))
        let minZoom = UInt8(max(0, min(call.getInt("minZoom") ?? 5, 22)))
        let maxZoom = UInt8(max(0, min(call.getInt("maxZoom") ?? 14, 22)))

        let jobsDir = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("map_jobs").path

        let job = CompileJob(
            jobId: jobId,
            bbox: bbox,
            minZoom: minZoom,
            maxZoom: maxZoom,
            // Placeholder inputs until the Phase 2 fetcher lands — the
            // simulated engine does not read them.
            pbfPath: "\(jobsDir)/raw/\(jobId).osm.pbf",
            demPath: "\(jobsDir)/raw/\(jobId).dem.tif",
            outputDir: jobsDir
        )

        setCancel(false)
        let forwarder = BridgeForwardingProgress(plugin: self)

        ffiQueue.async { [weak self] in
            guard let self else { return }
            var slices = 0
            while true {
                if self.isCancelRequested() {
                    _ = purgeJob(jobId: jobId, outputDir: job.outputDir)
                    self.emitStatus("cancelled", jobId: jobId, slices: slices)
                    call.resolve(["status": "cancelled", "jobId": jobId, "slices": slices])
                    return
                }

                let status = compileChunk(job: job, budgetMs: budgetMs, callback: forwarder)
                slices += 1

                switch status {
                case .yielded(let checkpoint):
                    // Loop continues: the engine resumes from its own durable
                    // checkpoint. In production this re-invoke is the next
                    // BGProcessingTask submission instead of a tight loop.
                    CAPLog.print("⚡️ MapCompiler slice \(slices) yielded at \(checkpoint.phase) block \(checkpoint.nextBlock)")
                    self.emitStatus("yielded", jobId: jobId, slices: slices)
                case .finished(let summary):
                    self.emitStatus("finished", jobId: jobId, slices: slices)
                    call.resolve([
                        "status": "finished",
                        "jobId": jobId,
                        "slices": slices,
                        "blocksTotal": Int(summary.blocksTotal),
                        "bytesWritten": Int(summary.bytesWritten),
                    ])
                    return
                case .failed(let reason):
                    self.emitStatus("failed", jobId: jobId, slices: slices)
                    call.resolve([
                        "status": "failed",
                        "jobId": jobId,
                        "slices": slices,
                        "reason": reason,
                    ])
                    return
                }
            }
        }
    }

    /// Requests cancellation of the active job (honored between slices).
    @objc func cancelJob(_ call: CAPPluginCall) {
        setCancel(true)
        call.resolve(["requested": true])
    }

    /// Debug: proves the Rust -> Swift -> WebView progress event path.
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

    private func emitStatus(_ state: String, jobId: String, slices: Int) {
        notifyListeners(
            "compilationStatus",
            data: ["state": state, "jobId": jobId, "slices": slices]
        )
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
