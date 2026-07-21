import BackgroundTasks
import Foundation
import Capacitor
// DEBT(D004): iOS build/link and device smokes need an Xcode machine, latest FFI enum-split changes not compile-verified — platforms: ios,android
// DEBT(D009): mem gate needs a harness-free CLI driver plus the in-process allocator peak counter, an Austria-scale on-device run, and the iOS increased-memory entitlement — platforms: ios,core

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
        CAPPluginMethod(name: "queryJob", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "getEngineVersion", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "emitTestProgress", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "enqueueBackgroundJob", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "queryBackgroundJob", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "acknowledgeBackgroundJob", returnType: CAPPluginReturnPromise),
        CAPPluginMethod(name: "cancelBackgroundJob", returnType: CAPPluginReturnPromise),
    ]

    /// The live plugin instance, if the WebView is up. The background
    /// scheduler uses it to surface terminal events to the UI when (and only
    /// when) there is a UI; a BGProcessingTask relaunch has no WebView, and
    /// the JS layer discovers results via `queryBackgroundJob` on resume.
    private static weak var active: MapCompilerPlugin?

    public override func load() {
        MapCompilerPlugin.active = self
    }

    /// jobId names on-disk files under the sandbox (`{jobId}.pmtiles`,
    /// `.checkpoint`, `.index.redb`). A `/`, `..`, or absolute path would
    /// traverse out of it. The Rust FFI (`to_job_spec`) enforces the same
    /// invariant as the authoritative choke point; this pre-flight fails fast
    /// with a clear reject instead of surfacing as a compile `.failed`, and
    /// covers queryJob (which bypasses `to_job_spec`).
    static let unsafeJobIdMessage = "Invalid jobId: only [A-Za-z0-9_-] allowed, max 128 chars"

    static func isSafeJobId(_ jobId: String) -> Bool {
        !jobId.isEmpty
            && jobId.count <= 128
            && jobId.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") }
    }

    /// Forwards a background-compile terminal event to the WebView if one
    /// exists right now; silently a no-op in a headless BGTask relaunch.
    static func emitBackgroundEvent(_ data: [String: Any]) {
        active?.notifyListeners("backgroundCompile", data: data)
    }

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
        guard MapCompilerPlugin.isSafeJobId(jobId) else {
            call.reject(MapCompilerPlugin.unsafeJobIdMessage)
            return
        }
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
                case .failedFatal(let reason):
                    self.emitStatus("failed", jobId: jobId, slices: slices)
                    call.resolve([
                        "status": "failed",
                        "jobId": jobId,
                        "slices": slices,
                        "reason": reason,
                        "transient": false,
                    ])
                    return
                case .failedTransient(let reason):
                    // Another runner holds the job's slice lock; durable
                    // state untouched — surface as retryable, don't loop.
                    CAPLog.print("⚡️ MapCompiler job \(jobId) transient refusal: \(reason)")
                    self.emitStatus("failed", jobId: jobId, slices: slices)
                    call.resolve([
                        "status": "failed",
                        "jobId": jobId,
                        "slices": slices,
                        "reason": reason,
                        "transient": true,
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

    /// Cold-start resume detection: returns the engine's durable checkpoint
    /// for a job if one survives on disk (e.g. after iOS killed the process).
    @objc func queryJob(_ call: CAPPluginCall) {
        guard let jobId = call.getString("jobId"), !jobId.isEmpty else {
            call.reject("Missing required parameter: jobId")
            return
        }
        guard MapCompilerPlugin.isSafeJobId(jobId) else {
            call.reject(MapCompilerPlugin.unsafeJobIdMessage)
            return
        }
        let jobsDir = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("map_jobs").path

        ffiQueue.async {
            guard let cp = queryCheckpoint(jobId: jobId, outputDir: jobsDir) else {
                call.resolve(["found": false])
                return
            }
            call.resolve([
                "found": true,
                "phase": String(describing: cp.phase),
                "nextBlock": Int(cp.nextBlock),
                "pbfByteOffset": Int(cp.pbfByteOffset),
                "bytesWritten": Int(cp.bytesWritten),
            ])
        }
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

    // -----------------------------------------------------------------------
    // Background compilation (P8.C2)
    // -----------------------------------------------------------------------

    /// Queues a compile job for BGProcessingTask execution: persists the job
    /// spec durably (the task may fire in a fresh process with no WebView),
    /// then submits the scheduler request. iOS decides when the window opens
    /// (our request: external power, no network needed).
    @objc func enqueueBackgroundJob(_ call: CAPPluginCall) {
        guard let bbox = call.getString("bbox"), !bbox.isEmpty else {
            call.reject("Missing required parameter: bbox (\"west,south,east,north\")")
            return
        }
        let jobId = call.getString("jobId") ?? UUID().uuidString
        guard MapCompilerPlugin.isSafeJobId(jobId) else {
            call.reject(MapCompilerPlugin.unsafeJobIdMessage)
            return
        }
        let minZoom = UInt8(max(0, min(call.getInt("minZoom") ?? 5, 22)))
        let maxZoom = UInt8(max(0, min(call.getInt("maxZoom") ?? 14, 22)))
        let jobsDir = defaultJobsDir()

        // Enforced single-slot invariant (D005 parity): saving over an
        // existing record would orphan a finished job's archive or yank a
        // pending job out from under its scheduler window. The check lives
        // HERE, not in JS — the JS guard only covers 'pending' and can be
        // stale.
        if let existing = PendingJobStore.loadAny() {
            let remedy = existing.state == .pending
                ? "cancel it (cancelBackgroundJob) first"
                : "acknowledge it (acknowledgeBackgroundJob) first"
            call.reject(
                "The single-job store already holds job '\(existing.jobId)' (\(existing.state.rawValue)) — \(remedy)."
            )
            return
        }

        let record = PendingJobStore.Record(
            state: .pending,
            jobId: jobId,
            bbox: bbox,
            minZoom: minZoom,
            maxZoom: maxZoom,
            pbfPath: "\(jobsDir)/raw/\(jobId).osm.pbf",
            demPath: "\(jobsDir)/raw/\(jobId).dem.tif",
            outputDir: jobsDir,
            reason: nil,
            blocksTotal: nil,
            bytesWritten: nil,
            dirtyAttempts: 0,
            cleanStop: false
        )

        do {
            try PendingJobStore.save(record)
        } catch {
            call.reject("Could not persist background job: \(error.localizedDescription)")
            return
        }
        BackgroundCompileScheduler.shared.scheduleIfPending()
        call.resolve(["scheduled": true, "jobId": jobId])
    }

    /// Resume-time discovery for the JS layer: reports the durable
    /// background-job record (pending / finished / failed). On `finished`,
    /// `archivePath` names the .pmtiles in the app sandbox — the WebView
    /// stream-copies it into OPFS (the P7 seam: natively compiled archives
    /// land in the sandbox, which is NOT OPFS; the JS side owns the import)
    /// and then calls `acknowledgeBackgroundJob`.
    @objc func queryBackgroundJob(_ call: CAPPluginCall) {
        guard let record = PendingJobStore.loadAny() else {
            call.resolve(["state": "idle"])
            return
        }
        var data: [String: Any] = [
            "state": record.state.rawValue,
            "jobId": record.jobId,
        ]
        if record.state == .finished {
            data["archivePath"] = "\(record.outputDir)/\(record.jobId).pmtiles"
            data["blocksTotal"] = record.blocksTotal.map(Int.init) ?? 0
            data["bytesWritten"] = record.bytesWritten.map(Int.init) ?? 0
        }
        if let reason = record.reason {
            data["reason"] = reason
        }
        call.resolve(data)
    }

    /// Clears a terminal (finished/failed) record once the JS layer has
    /// imported the archive into OPFS (or shown the failure), and releases
    /// the job's disk claim: the sandbox archive (now redundant with the
    /// OPFS copy) plus leftover checkpoint/index/scratch state.
    ///
    /// Targeted (D005 parity): requires `jobId` and rejects on mismatch, so
    /// a stale acknowledge from a slow in-flight ingest can never clear a
    /// record it doesn't own. A pending record is NOT clearable here — that
    /// is `cancelBackgroundJob` territory.
    @objc func acknowledgeBackgroundJob(_ call: CAPPluginCall) {
        guard let jobId = call.getString("jobId"), !jobId.isEmpty else {
            call.reject("Missing required parameter: jobId")
            return
        }
        ffiQueue.async {
            guard let record = PendingJobStore.loadAny() else {
                // Idempotent: a retried ack finds the slot already empty.
                call.resolve(["cleared": false])
                return
            }
            guard record.jobId == jobId else {
                call.reject("Stale acknowledge: store holds job '\(record.jobId)', not '\(jobId)'")
                return
            }
            guard record.state != .pending else {
                call.reject("Job \(record.jobId) is still pending; cancel it instead")
                return
            }
            try? FileManager.default.removeItem(atPath: record.archivePath)
            _ = purgeJob(jobId: record.jobId, outputDir: record.outputDir)
            PendingJobStore.clear()
            call.resolve(["cleared": true])
        }
    }

    /// Hard cancellation of the background job (D005 parity): cancels the
    /// queued BGProcessingTask request, clears the durable record (the
    /// store's CAS terminal transitions make any still-running slice's late
    /// write a no-op), and wipes the job's disk footprint — with a second
    /// straggler sweep after the in-flight slice window has closed.
    @objc func cancelBackgroundJob(_ call: CAPPluginCall) {
        ffiQueue.async {
            let record = PendingJobStore.loadAny()
            if let record, record.state != .pending {
                call.reject("Job \(record.jobId) is \(record.state.rawValue); acknowledge it instead of cancelling")
                return
            }

            BGTaskScheduler.shared.cancel(
                taskRequestWithIdentifier: BackgroundCompileScheduler.taskIdentifier
            )
            PendingJobStore.clear()

            var result: [String: Any] = ["cancelled": true]
            if let record {
                _ = purgeJob(jobId: record.jobId, outputDir: record.outputDir)
                try? FileManager.default.removeItem(atPath: record.archivePath)
                result["jobId"] = record.jobId

                let jobId = record.jobId
                let outputDir = record.outputDir
                let archivePath = record.archivePath
                DispatchQueue.global(qos: .utility).asyncAfter(
                    deadline: .now() + BackgroundCompileScheduler.stragglerWindowSeconds
                ) {
                    _ = purgeJob(jobId: jobId, outputDir: outputDir)
                    try? FileManager.default.removeItem(atPath: archivePath)
                }
                CAPLog.print("⚡️ background job \(jobId) cancelled and purged")
            }
            call.resolve(result)
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

/// The one directory owning job checkpoints, raw inputs, and finished
/// archives (same derivation `startJob`/`queryJob` use inline).
func defaultJobsDir() -> String {
    FileManager.default
        .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("map_jobs").path
}

// ---------------------------------------------------------------------------
// Thermal monitoring (P8.C2 §1)
// ---------------------------------------------------------------------------

/// Mirrors `ProcessInfo.thermalState` into the Rust core's global thermal
/// flag. The Rust side is a single lock-free atomic store, safe from any
/// thread at any time — including while `compileChunk` runs on another
/// queue; running slices pick the change up at their next block boundary.
final class ThermalStateBridge {
    static let shared = ThermalStateBridge()
    private var observer: NSObjectProtocol?

    private init() {}

    /// Called once from `didFinishLaunching`. Pushes the CURRENT state
    /// immediately: the change notification does not fire for a state that
    /// was already elevated when this process started.
    func start() {
        ThermalStateBridge.pushCurrentState()
        observer = NotificationCenter.default.addObserver(
            forName: ProcessInfo.thermalStateDidChangeNotification,
            object: nil,
            queue: nil  // deliver on the posting thread; the FFI store is thread-safe
        ) { _ in
            ThermalStateBridge.pushCurrentState()
        }
    }

    /// Reads the live OS thermal level and publishes it to the Rust core.
    /// Also the explicit "poll once at task start" hook (P8.C2 §1): a
    /// BGProcessingTask can wake a process on an already-hot device.
    static func pushCurrentState() {
        let state = map(ProcessInfo.processInfo.thermalState)
        setThermalState(state: state)
        CAPLog.print("⚡️ ThermalStateBridge → \(state)")
    }

    /// 1:1 mapping per the FFI contract (ffi/src/lib.rs doc comment).
    static func map(_ state: ProcessInfo.ThermalState) -> ThermalState {
        switch state {
        case .nominal: return .nominal
        case .fair: return .fair
        case .serious: return .serious
        case .critical: return .critical
        // Fail COOL, mirroring the Rust core's unknown-byte rule: a future
        // hotter-than-critical level must throttle, never run full-tilt.
        @unknown default: return .critical
        }
    }
}

// ---------------------------------------------------------------------------
// Background scheduling (P8.C2 §2–3)
// ---------------------------------------------------------------------------

/// Owns the BGProcessingTask lifecycle: registration (launch), submission
/// (whenever a job is pending), and the in-window execution loop. The loop
/// is the same budget-yield contract `startJob` drives in the foreground —
/// the engine's durable checkpoint is what makes the ~295s guillotine and
/// process death between windows survivable.
final class BackgroundCompileScheduler {
    static let shared = BackgroundCompileScheduler()

    /// Must match Info.plist `BGTaskSchedulerPermittedIdentifiers`.
    static let taskIdentifier = "com.freehike.compiler.sync"

    /// Per-slice budget inside a background window. 2s is small enough that
    /// the expiration flag is honored promptly (iOS grants a few seconds of
    /// grace after the expiration handler fires) and large enough that
    /// checkpoint fsync overhead stays negligible.
    static let sliceBudgetMs: UInt32 = 2_000

    /// Circuit breaker (D005 parity with Android's P0-2 fix): consecutive
    /// windows allowed to die without completing a single slice before the
    /// job is declared poisoned. "Die" = process death mid-FFI (SIGBUS,
    /// Jetsam kill, Rust abort) — the only witness is the absent cleanStop
    /// marker at the next window's start.
    static let maxDirtyAttempts = 5

    /// Straggler-sweep delay after a hard cancel: the in-flight slice may
    /// fsync one more checkpoint after the first purge (stop is honored at
    /// slice granularity, ≤ ~2× the slice budget).
    static let stragglerWindowSeconds: TimeInterval = 5

    /// FFI work happens off the scheduler's callback thread, one lane, no
    /// overlap with itself.
    private let queue = DispatchQueue(label: "app.freehike.mapcompiler.bgtask", qos: .utility)

    private init() {}

    /// MUST run before `didFinishLaunching` returns — iOS rejects handler
    /// registration after launch completes.
    func register() {
        BGTaskScheduler.shared.register(
            forTaskWithIdentifier: BackgroundCompileScheduler.taskIdentifier,
            using: nil
        ) { task in
            guard let processing = task as? BGProcessingTask else {
                task.setTaskCompleted(success: false)
                return
            }
            self.handle(processing)
        }
    }

    /// Submits (or re-submits — same-identifier submission replaces the
    /// queued request, so this is idempotent) a processing request when a
    /// pending job exists. Called on enqueue, on entering background, and
    /// after every window that ends with work remaining.
    func scheduleIfPending() {
        guard PendingJobStore.loadPending() != nil else { return }
        let request = BGProcessingTaskRequest(
            identifier: BackgroundCompileScheduler.taskIdentifier
        )
        // Compiles are heavy, deferrable work: wait for the charger (the
        // honest "will compile while charging" UX), and the raw PBF/DEM are
        // already on disk, so no network is needed.
        request.requiresExternalPower = true
        request.requiresNetworkConnectivity = false
        do {
            try BGTaskScheduler.shared.submit(request)
            CAPLog.print("⚡️ BackgroundCompileScheduler: window requested")
        } catch {
            // Expected on Simulator (unsupported) and when Background App
            // Refresh is off. The job stays pending; a foreground startJob
            // can always finish it — same checkpoint, same engine.
            CAPLog.print("⚡️ BackgroundCompileScheduler: submit failed: \(error)")
        }
    }

    /// One background window. Drives budget-yield slices until the job
    /// reaches a terminal state, the window expires, or thermal Critical
    /// tells us to hand the window back and cool down.
    private func handle(_ task: BGProcessingTask) {
        // §1 requirement: report the PRE-EXISTING thermal level before the
        // first slice — the notification observer only covers changes.
        ThermalStateBridge.pushCurrentState()

        guard let loaded = PendingJobStore.loadPending() else {
            task.setTaskCompleted(success: true)
            return
        }

        // ── Circuit breaker (D005 parity) ───────────────────────────────
        // A window whose predecessor set cleanStop (deliberate expiration /
        // thermal / transient handback) is healthy; one whose predecessor
        // died mid-FFI counts as dirty. Consume the marker, then check.
        let wasCleanStop = loaded.cleanStop ?? false
        PendingJobStore.updatePending(jobId: loaded.jobId) { r in
            r.cleanStop = false
            if !wasCleanStop { r.dirtyAttempts = (r.dirtyAttempts ?? 0) + 1 }
        }
        guard let pending = PendingJobStore.loadPending() else {
            task.setTaskCompleted(success: true)
            return
        }
        if (pending.dirtyAttempts ?? 0) > BackgroundCompileScheduler.maxDirtyAttempts {
            let reason = "Background compile aborted: \(BackgroundCompileScheduler.maxDirtyAttempts) " +
                "consecutive windows died without completing a slice (likely corrupt input or memory pressure)."
            CAPLog.print("⚡️ BG compile circuit breaker tripped for \(pending.jobId)")
            PendingJobStore.markFailed(pending, reason: reason)
            _ = purgeJob(jobId: pending.jobId, outputDir: pending.outputDir)
            MapCompilerPlugin.emitBackgroundEvent([
                "state": "failed",
                "jobId": pending.jobId,
                "reason": reason,
            ])
            task.setTaskCompleted(success: false)
            return
        }

        let expiration = ExpirationFlag()
        task.expirationHandler = {
            // Runs ~295s in, with a few seconds of grace. Raising the flag
            // is the whole graceful stop: the loop observes it at the next
            // slice boundary, and the slice now in flight ends in the
            // engine's own checkpoint path (fsync + atomic rename) — there
            // is no state on this side to save.
            expiration.raise()
        }

        queue.async {
            let job = pending.toCompileJob()
            var slices = 0
            while true {
                if expiration.isRaised {
                    // Deliberate handback — never counts toward the breaker.
                    PendingJobStore.updatePending(jobId: pending.jobId) { $0.cleanStop = true }
                    CAPLog.print("⚡️ BG window expired after \(slices) slices; checkpoint durable, rescheduling")
                    self.scheduleIfPending()
                    task.setTaskCompleted(success: false)
                    return
                }

                let status = compileChunk(
                    job: job,
                    budgetMs: BackgroundCompileScheduler.sliceBudgetMs,
                    callback: BackgroundProgressSink()
                )
                slices += 1

                if slices == 1 {
                    // The FFI survived a full slice, so this window is not
                    // part of a crash loop: consecutive-death accounting
                    // starts over. A job poisoned at a fixed input byte
                    // resumes right at the poison and never reaches this
                    // line on later windows — the breaker trips after
                    // maxDirtyAttempts.
                    PendingJobStore.updatePending(jobId: pending.jobId) { $0.dirtyAttempts = 0 }
                }

                switch status {
                case .yielded:
                    // Thermal governance: under Critical the engine yields
                    // after its one-block minimum on every call. Re-invoking
                    // in a tight loop would defeat the throttle — hand the
                    // window back and let a later window resume cold.
                    if thermalState() == .critical {
                        PendingJobStore.updatePending(jobId: pending.jobId) { $0.cleanStop = true }
                        CAPLog.print("⚡️ BG compile paused at thermal Critical after \(slices) slices")
                        self.scheduleIfPending()
                        task.setTaskCompleted(success: false)
                        return
                    }
                    continue
                case .finished(let summary):
                    // The engine has already written `{jobId}.pmtiles` to its
                    // final sandbox path. OPFS is WebKit-private storage, so
                    // the copy into OPFS belongs to the JS layer (P7 seam):
                    // mark the record finished for resume-time discovery and
                    // notify the UI if one is alive right now.
                    let stillOurs = PendingJobStore.markFinished(pending, summary: summary)
                    guard stillOurs else {
                        // Hard-cancelled while this slice ran: the record is
                        // gone and the canceller owns cleanup — do NOT
                        // resurrect it or announce the result.
                        CAPLog.print("⚡️ BG job \(pending.jobId) finished but was cancelled mid-slice; dropping result")
                        task.setTaskCompleted(success: true)
                        return
                    }
                    MapCompilerPlugin.emitBackgroundEvent([
                        "state": "finished",
                        "jobId": pending.jobId,
                        "archivePath": pending.archivePath,
                        "blocksTotal": Int(summary.blocksTotal),
                        "bytesWritten": Int(summary.bytesWritten),
                    ])
                    task.setTaskCompleted(success: true)
                    return
                case .failedFatal(let reason):
                    // Fatal per the Surface v1 contract (bad input, corrupt
                    // state). Do NOT reschedule — retrying a fatal failure
                    // on a charger overnight is a battery/flash burn — and
                    // release the temporary disk state immediately.
                    let stillOurs = PendingJobStore.markFailed(pending, reason: reason)
                    _ = purgeJob(jobId: pending.jobId, outputDir: pending.outputDir)
                    if stillOurs {
                        MapCompilerPlugin.emitBackgroundEvent([
                            "state": "failed",
                            "jobId": pending.jobId,
                            "reason": reason,
                        ])
                    }
                    task.setTaskCompleted(success: false)
                    return
                case .failedTransient(let reason):
                    // Another runner holds the job's slice lock. Durable
                    // state is untouched: keep the record pending, mark the
                    // handback clean, and let a later window retry.
                    PendingJobStore.updatePending(jobId: pending.jobId) { $0.cleanStop = true }
                    CAPLog.print("⚡️ BG compile transient refusal after \(slices) slices: \(reason)")
                    self.scheduleIfPending()
                    task.setTaskCompleted(success: false)
                    return
                }
            }
        }
    }
}

/// Durable record of the one queued/terminal background job. Survives
/// process death (BGProcessingTask fires in a fresh process) as JSON beside
/// the engine's own state, atomic-rename on write. Single-job by design:
/// Surface v1 compiles one region at a time; a queue is product-layer
/// territory (Phase 9).
///
/// Hardening parity with the Android store (P-NATIVE.C1, closes D005):
/// every mutation is lock-serialized, and every terminal transition is a
/// compare-and-set on `(jobId, state == .pending)` via [updatePending] — a
/// slice that was in flight when a hard cancel cleared the record cannot
/// resurrect it with a late finished/failed write.
enum PendingJobStore {
    enum State: String, Codable {
        case pending, finished, failed
    }

    struct Record: Codable {
        var state: State
        let jobId: String
        let bbox: String
        let minZoom: UInt8
        let maxZoom: UInt8
        let pbfPath: String
        let demPath: String?
        let outputDir: String
        var reason: String?
        var blocksTotal: UInt32?
        var bytesWritten: UInt64?
        /// Circuit-breaker counter: background windows whose PREDECESSOR
        /// died without a deliberate exit (expiration / thermal / transient
        /// handback all set `cleanStop`). Optional so pre-parity JSON
        /// records keep decoding.
        var dirtyAttempts: Int?
        /// Set just before every deliberate window handback; consumed (and
        /// cleared) at the next window's start.
        var cleanStop: Bool?

        var archivePath: String { "\(outputDir)/\(jobId).pmtiles" }

        func toCompileJob() -> CompileJob {
            CompileJob(
                jobId: jobId,
                bbox: bbox,
                minZoom: minZoom,
                maxZoom: maxZoom,
                pbfPath: pbfPath,
                demPath: demPath,
                outputDir: outputDir
            )
        }
    }

    /// Serializes plugin-queue vs. scheduler-queue mutations. Recursive:
    /// the CAS helper calls load/save while holding it.
    private static let lock = NSRecursiveLock()

    private static var url: URL {
        URL(fileURLWithPath: defaultJobsDir()).appendingPathComponent("background_job.json")
    }

    static func save(_ record: Record) throws {
        lock.lock()
        defer { lock.unlock() }
        let dir = URL(fileURLWithPath: defaultJobsDir())
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let data = try JSONEncoder().encode(record)
        try data.write(to: url, options: .atomic)
    }

    /// The record in any state (resume-time discovery).
    static func loadAny() -> Record? {
        lock.lock()
        defer { lock.unlock() }
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(Record.self, from: data)
    }

    /// The record only if work remains for a background window.
    static func loadPending() -> Record? {
        guard let record = loadAny(), record.state == .pending else { return nil }
        return record
    }

    /// Compare-and-set: applies `transform` only while the store still
    /// holds the PENDING record for `jobId`. Returns false — writing
    /// nothing — if the record was cleared (hard cancel), overwritten, or
    /// already terminal.
    @discardableResult
    static func updatePending(jobId: String, _ transform: (inout Record) -> Void) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard var r = loadAny(), r.jobId == jobId, r.state == .pending else { return false }
        transform(&r)
        try? save(r)
        return true
    }

    /// Terminal success transition; false if the record is gone (cancelled).
    @discardableResult
    static func markFinished(_ record: Record, summary: CompileSummary) -> Bool {
        updatePending(jobId: record.jobId) { r in
            r.state = .finished
            r.blocksTotal = summary.blocksTotal
            r.bytesWritten = summary.bytesWritten
        }
    }

    /// Terminal failure transition; false if the record is gone (cancelled).
    @discardableResult
    static func markFailed(_ record: Record, reason: String) -> Bool {
        updatePending(jobId: record.jobId) { r in
            r.state = .failed
            r.reason = reason
        }
    }

    static func clear() {
        lock.lock()
        defer { lock.unlock() }
        try? FileManager.default.removeItem(at: url)
    }
}

/// Set once by the expiration handler (scheduler thread), read by the slice
/// loop (ffi queue). Same NSLock idiom as the plugin's cancellation flag.
final class ExpirationFlag {
    private let lock = NSLock()
    private var raised = false

    func raise() {
        lock.lock()
        raised = true
        lock.unlock()
    }

    var isRaised: Bool {
        lock.lock()
        defer { lock.unlock() }
        return raised
    }
}

/// Progress sink for headless windows: no WebView to forward to, but the
/// per-slice log line keeps Console.app debugging honest.
private final class BackgroundProgressSink: ProgressCallback, @unchecked Sendable {
    func onProgress(percentage: Float, status: String) {
        CAPLog.print("⚡️ BG compile \(Int(percentage))% — \(status)")
    }
}
