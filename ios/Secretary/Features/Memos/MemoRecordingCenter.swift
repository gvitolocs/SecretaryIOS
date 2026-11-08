import Darwin
import Foundation
import QuartzCore

/// The memo recorder, outside AppModel. When the Lock Screen control or the Action button
/// starts a memo, iOS launches the app in the background just for `StartMemoRecordingIntent`;
/// that launch sets up only the audio session, the recorder and the Live Activity. AppModel
/// (chat, the avatar stage, sync, the Rust core, photo backup, the upload queue) is built when a
/// window opens, or when the recording stops and has to be queued.
@MainActor
enum MemoRecordingCenter {
    static let recorder = MemoRecorder()

    /// Installs the intents' handlers. Called from `SecretaryApp.init`, before any scene.
    static func install() {
        MemoRecordingCommands.handle = { command in
            switch command {
            case .pause: recorder.pause()
            case .resume: recorder.resume()
            case .stop: stop()
            }
        }
        MemoRecordingCommands.start = { try await start() }
    }

    static func start() async throws {
        let perform = CACurrentMediaTime()
        let processAge = ProcessCost.age()
        let launch = AppModel.isBuilt ? "app" : "minimal"
        guard (try? KeychainStore.accessKey.read())?.isEmpty == false else {
            MemoLog.write("start intent (\(launch)): no access key")
            throw IntentFailure.message(String(localized: "Open Secretary and sign in first."))
        }
        if recorder.isActive { return }
        if AppModel.isBuilt { AppModel.shared.memos.player.pause() }
        await recorder.start()
        if let error = recorder.error {
            MemoLog.write("start intent (\(launch)): failed: \(error)")
            throw IntentFailure.message(error)
        }
        // The first audio: the recorder's clock moves once buffers arrive.
        var first: Double?
        for _ in 0..<200 where first == nil {
            if recorder.recordedTime > 0 { first = (CACurrentMediaTime() - perform) * 1000 } else { try? await Task.sleep(for: .milliseconds(5)) }
        }
        cost = ProcessCost.now()
        MemoLog.write(String(format: "start intent (%@): process age %.0f ms at perform, first audio %@ after perform, footprint %.1f MB, live=%@",
                             launch, processAge * 1000, first.map { String(format: "%.0f ms", $0) } ?? "not within 1 s",
                             cost.map { Double($0.footprint) / 1_048_576 } ?? 0, recorder.activityReport))
    }

    /// The process's CPU and memory when the intent's recording started (the cost line at stop).
    private static var cost: ProcessCost?

    /// Stops and queues the memo; AppModel (upload, transcription) starts only now.
    static func stop() {
        logCost()
        guard let r = recorder.stop() else { return }
        AppModel.shared.memos.add(r)
    }

    /// CPU share and memory while recording, since the intent started it.
    static func logCost() {
        guard let start = cost, let end = ProcessCost.now() else { return }
        cost = nil
        let wall = max(0.001, end.wall - start.wall)
        MemoLog.write(String(format: "recording cost: %.1f s, CPU %.1f%%, footprint %.1f MB (app model %@)",
                             wall, (end.cpu - start.cpu) / wall * 100, Double(end.footprint) / 1_048_576,
                             AppModel.isBuilt ? "built" : "not built"))
    }
}

/// This process's CPU time, memory footprint and age (the recording cost in memo-log.txt).
struct ProcessCost {
    let wall: Double
    let cpu: Double
    let footprint: UInt64

    static func now() -> ProcessCost? {
        var usage = rusage()
        guard getrusage(RUSAGE_SELF, &usage) == 0 else { return nil }
        let cpu = Double(usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) + Double(usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) / 1e6
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count) }
        }
        guard kr == KERN_SUCCESS else { return nil }
        return ProcessCost(wall: CACurrentMediaTime(), cpu: cpu, footprint: info.phys_footprint)
    }

    /// Seconds since this process started (sysctl's start time).
    static func age() -> Double {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.size
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&mib, 4, &info, &size, nil, 0) == 0 else { return 0 }
        let start = info.kp_proc.p_un.__p_starttime
        let started = Double(start.tv_sec) + Double(start.tv_usec) / 1e6
        return Date().timeIntervalSince1970 - started
    }
}
