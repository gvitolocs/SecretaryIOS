import Darwin
import Foundation
import os

// Launch and tab-switch timing, in every build (Release is what the iPhone measures):
// - os_signpost intervals in Points of Interest (`tech.getsecretary.app`): "Launch" from the
//   App's init to the first Chat frame, one "Setup" interval per store AppModel builds, and
//   "Tab" from a tab tap to that tab's first frame. Instruments → Points of Interest shows them.
// - Logger `tech.getsecretary.app` / `Perf`: one line per measurement, with the time since the
//   process started (kernel start time, so the dyld and static-init part is included).
// - `-SecretaryPerfSelfTest`: PerfSelfTest switches through every tab after launch and writes
//   the summary to `Documents/perf-selftest.txt` (ios/scripts/perf-selftest.sh fetches it).

@MainActor
enum PerfLog {
    nonisolated static let logger = Logger(subsystem: "tech.getsecretary.app", category: "Perf")
    nonisolated static let signposter = OSSignposter(subsystem: "tech.getsecretary.app", category: .pointsOfInterest)
    nonisolated static let selfTestRequested = ProcessInfo.processInfo.arguments.contains("-SecretaryPerfSelfTest")

    /// Wall-clock start of this process (sysctl), nil if the kernel would not say.
    nonisolated static let processStart: Date? = {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&mib, u_int(mib.count), &info, &size, nil, 0) == 0 else { return nil }
        let start = info.kp_proc.p_un.__p_starttime
        return Date(timeIntervalSince1970: Double(start.tv_sec) + Double(start.tv_usec) / 1_000_000)
    }()

    struct TabSwitch {
        let from: AppModel.Tab
        let to: AppModel.Tab
        let ms: Double
    }

    private(set) static var appInitAt: Date?
    /// Process start → first Chat frame, in ms (nil until then, or when launch did not open on Chat).
    private(set) static var firstChatFrameMs: Double?
    /// App init → first Chat frame, in ms.
    private(set) static var appToChatFrameMs: Double?
    /// AppModel's setup steps, in the order they ran (ms each).
    private(set) static var setup: [(name: String, ms: Double)] = []
    private(set) static var switches: [TabSwitch] = []
    /// How often something ran that should run once per launch (app setup, root views).
    private(set) static var counts: [String: Int] = [:]
    private(set) static var lucinaReadyMs: Double?
    private(set) static var lastAppeared: AppModel.Tab?

    private static var launchInterval: OSSignpostIntervalState?
    private static var launchDone = false
    private static var pending: (from: AppModel.Tab, to: AppModel.Tab, start: Date, state: OSSignpostIntervalState)?
    private static var lucinaLoadStart: Date?

    nonisolated private static func ms(since start: Date, to end: Date = Date()) -> Double {
        (end.timeIntervalSince(start) * 10_000).rounded() / 10
    }

    nonisolated private static func format(_ value: Double) -> String { String(format: "%.1f", value) }

    /// SecretaryApp.init: the first app code that runs.
    static func appInit() {
        guard appInitAt == nil else { return }
        let now = Date()
        appInitAt = now
        launchInterval = signposter.beginInterval("Launch")
        if let processStart {
            logger.notice("perf launch: process start → app init \(format(ms(since: processStart, to: now)), privacy: .public) ms")
        }
    }

    /// Runs one setup step inside a signpost interval and remembers how long it took.
    @discardableResult
    static func measure<T>(_ name: StaticString, _ body: () throws -> T) rethrows -> T {
        let label = String(describing: name)
        let state = signposter.beginInterval("Setup", id: signposter.makeSignpostID(), "\(label, privacy: .public)")
        let start = Date()
        defer {
            signposter.endInterval("Setup", state)
            setup.append((label, ms(since: start)))
        }
        return try body()
    }

    static func count(_ key: String) {
        counts[key, default: 0] += 1
    }

    /// A tab's root appeared. The first Chat appearance ends the launch; the one after a tap ends that switch.
    /// The measurement stops on the next main-queue turn, after SwiftUI committed the frame.
    static func tabAppeared(_ tab: AppModel.Tab) {
        lastAppeared = tab
        let appearedAt = Date()
        DispatchQueue.main.async { MainActor.assumeIsolated { frameCommitted(tab, appearedAt: appearedAt) } }
    }

    private static func frameCommitted(_ tab: AppModel.Tab, appearedAt: Date) {
        let framed = Date()
        if !launchDone {
            launchDone = true
            if let launchInterval { signposter.endInterval("Launch", launchInterval, "\(tab.rawValue, privacy: .public)") }
            if tab == .chat {
                if let appInitAt { appToChatFrameMs = ms(since: appInitAt, to: framed) }
                if let processStart { firstChatFrameMs = ms(since: processStart, to: framed) }
            }
            let total = firstChatFrameMs.map(format) ?? "?"
            let app = appToChatFrameMs.map(format) ?? "?"
            logger.notice("perf launch: first \(tab.rawValue, privacy: .public) frame \(total, privacy: .public) ms after process start (\(app, privacy: .public) ms after app init)")
        }
        guard let pending, pending.to == tab else { return }
        self.pending = nil
        signposter.endInterval("Tab", pending.state, "\(tab.rawValue, privacy: .public)")
        let total = ms(since: pending.start, to: framed)
        let appear = ms(since: pending.start, to: appearedAt)
        switches.append(TabSwitch(from: pending.from, to: tab, ms: total))
        logger.notice("perf tab: \(pending.from.rawValue, privacy: .public) → \(tab.rawValue, privacy: .public) \(format(total), privacy: .public) ms (onAppear after \(format(appear), privacy: .public) ms)")
    }

    /// The selected tab changed (a tap, a route, the self-test).
    static func tabSelected(from: AppModel.Tab, to: AppModel.Tab) {
        guard from != to else { return }
        if let pending { signposter.endInterval("Tab", pending.state, "superseded") }
        let label = "\(from.rawValue)→\(to.rawValue)"
        let state = signposter.beginInterval("Tab", id: signposter.makeSignpostID(), "\(label, privacy: .public)")
        pending = (from, to, Date(), state)
    }

    /// The Lucina tab created the web view and started loading the stage.
    static func lucinaLoadBegan() {
        lucinaLoadStart = Date()
        signposter.emitEvent("LucinaStageLoad")
    }

    /// The stage reported `ready` (model on screen).
    static func lucinaReady() {
        guard let start = lucinaLoadStart else { return }
        lucinaLoadStart = nil
        let total = ms(since: start)
        lucinaReadyMs = total
        signposter.emitEvent("LucinaStageReady")
        logger.notice("perf lucina: stage ready \(format(total), privacy: .public) ms after the tab opened it")
    }

    /// Everything measured so far, one line each.
    static func summary() -> [String] {
        var lines: [String] = []
        let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?"
        #if DEBUG
        let configuration = "Debug"
        #else
        let configuration = "Release"
        #endif
        lines.append("build \(build) \(configuration)")
        if let firstChatFrameMs {
            let verdict = firstChatFrameMs < 1000 ? "PASS" : "FAIL"
            lines.append("launch: first chat frame \(format(firstChatFrameMs)) ms after process start [\(verdict) < 1000 ms]")
        } else {
            lines.append("launch: no chat frame measured (signed out, or launched on another tab)")
        }
        if let appToChatFrameMs { lines.append("launch: app init → first chat frame \(format(appToChatFrameMs)) ms") }
        let setupTotal = setup.reduce(0) { $0 + $1.ms }
        lines.append("setup: AppModel \(format(setupTotal)) ms in \(setup.count) steps")
        for step in setup.sorted(by: { $0.ms > $1.ms }) { lines.append("  setup \(step.name): \(format(step.ms)) ms") }
        for s in switches { lines.append("tab: \(s.from.rawValue) → \(s.to.rawValue) \(format(s.ms)) ms") }
        if let lucinaReadyMs { lines.append("lucina: stage ready \(format(lucinaReadyMs)) ms after first open") }
        for (key, value) in counts.sorted(by: { $0.key < $1.key }) { lines.append("count \(key): \(value)") }
        return lines
    }
}

/// `-SecretaryPerfSelfTest`: after launch, opens every tab (some twice, to see the warm switch),
/// checks that none of them rebuilt the root or ran the app setup again, and writes the timings to
/// `Documents/perf-selftest.txt` and the Perf log.
@MainActor
enum PerfSelfTest {
    private static var started = false

    static var url: URL {
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return documents.appendingPathComponent("perf-selftest.txt")
    }

    static func runIfRequested(_ app: AppModel) async {
        guard PerfLog.selfTestRequested, !started else { return }
        started = true
        // The launch ends with the first frame; then let the deferred launch work run.
        guard await wait(seconds: 20, until: { PerfLog.lastAppeared != nil }) else { return write(["no tab appeared in 20 s"]) }
        try? await Task.sleep(for: .seconds(4))
        let before = PerfLog.counts
        let order: [AppModel.Tab] = [.shopping, .fitness, .plugins, .lucina, .chat, .shopping, .chat, .fitness, .lucina, .chat]
        for tab in order {
            let from = app.selectedTab
            app.selectedTab = tab
            let switched = await wait(seconds: 10, until: { PerfLog.switches.last.map { $0.to == tab && $0.from == from } ?? false })
            if !switched { PerfLog.logger.error("perf selftest: \(tab.rawValue, privacy: .public) did not appear in 10 s") }
            // The stage's first load is measured too (PerfLog.lucinaReady); later visits are just a switch.
            if tab == .lucina, PerfLog.lucinaReadyMs == nil { _ = await wait(seconds: 30, until: { PerfLog.lucinaReadyMs != nil }) }
            try? await Task.sleep(for: .seconds(1.5))
        }
        var lines = PerfLog.summary()
        // Tab switches must not rebuild the root or re-run the app setup.
        for key in ["app.start", "MainTabs.appear", "RootView.signedIn"] {
            let kept = before[key, default: 0] == PerfLog.counts[key, default: 0]
            lines.append("check \(key) unchanged by tab switches: \(kept ? "PASS" : "FAIL") (\(before[key, default: 0]) → \(PerfLog.counts[key, default: 0]))")
        }
        write(lines)
    }

    private static func wait(seconds: Double, until done: () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if done() { return true }
            try? await Task.sleep(for: .milliseconds(20))
        }
        return done()
    }

    private static func write(_ lines: [String]) {
        let stamp = ISO8601DateFormatter().string(from: Date())
        var text = "== perf selftest \(stamp)\n"
        for line in lines {
            PerfLog.logger.notice("perf selftest: \(line, privacy: .public)")
            print("perf selftest: \(line)")
            text += line + "\n"
        }
        text += "== done\n"
        let url = url
        if !FileManager.default.fileExists(atPath: url.path) { FileManager.default.createFile(atPath: url.path, contents: nil) }
        guard let handle = try? FileHandle(forWritingTo: url) else { return }
        defer { try? handle.close() }
        _ = try? handle.seekToEnd()
        try? handle.write(contentsOf: Data(text.utf8))
    }
}
