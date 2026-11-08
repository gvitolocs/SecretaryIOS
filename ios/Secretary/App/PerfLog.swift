import Darwin
import Foundation
import os
import UIKit

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

    private(set) static var firstMessageMs: Double?
    private(set) static var firstMessageSource: String?
    private(set) static var firstMessageCount = 0
    private(set) static var chatSyncedMs: Double?

    static var firstMessageSeen: Bool { firstMessageSource != nil }

    static func firstMessage(source: String, count: Int = 1) {
        guard firstMessageSource == nil else { return }
        firstMessageSource = source
        firstMessageCount = count
        let now = Date()
        firstMessageMs = processStart.map { ms(since: $0, to: now) }
        let app = appInitAt.map { format(ms(since: $0, to: now)) } ?? "?"
        let total = firstMessageMs.map(format) ?? "?"
        signposter.emitEvent("FirstMessage", "\(source, privacy: .public)")
        logger.notice("perf launch: first message \(total, privacy: .public) ms after process start (\(app, privacy: .public) ms after app init), \(source, privacy: .public), \(count) items")
    }

    static func chatSynced() {
        if let start = foregroundAt {
            foregroundAt = nil
            let total = ms(since: start)
            foregroundSyncedMs.append(total)
            logger.notice("perf foreground: chat synced \(format(total), privacy: .public) ms after the app became active")
        }
        guard chatSyncedMs == nil else { return }
        let now = Date()
        chatSyncedMs = ms(since: processStart ?? appInitAt ?? now, to: now)
        let app = appInitAt.map { format(ms(since: $0, to: now)) } ?? "?"
        signposter.emitEvent("ChatSynced")
        logger.notice("perf launch: chat synced \(format(chatSyncedMs ?? 0), privacy: .public) ms after process start (\(app, privacy: .public) ms after app init)")
    }

    /// CPU time (user + system) the process used in its first 10 and 30 s, in ms.
    private(set) static var cpuFirst10sMs: Double?
    private(set) static var cpuFirst30sMs: Double?
    /// The app came back from the background (nil once its chat has synced).
    private static var foregroundAt: Date?
    private(set) static var foregroundSyncedMs: [Double] = []
    private(set) static var foregroundCpu10sMs: [Double] = []

    /// CPU time (user + system) this process used so far, in ms.
    nonisolated static func cpuMs() -> Double {
        var usage = rusage()
        guard getrusage(RUSAGE_SELF, &usage) == 0 else { return 0 }
        func ms(_ t: timeval) -> Double { Double(t.tv_sec) * 1000 + Double(t.tv_usec) / 1000 }
        return ((ms(usage.ru_utime) + ms(usage.ru_stime)) * 10).rounded() / 10
    }

    private static var wasInBackground = false

    static func backgrounded() { wasInBackground = true }

    /// The scene became active; measured only when it comes back from the background.
    static func foregrounded() {
        guard wasInBackground else { return }
        wasInBackground = false
        foregroundAt = Date()
        ScrollTrace.begin("warm")
        let cpu = cpuMs()
        DispatchQueue.main.asyncAfter(deadline: .now() + 10) {
            MainActor.assumeIsolated {
                let used = cpuMs() - cpu
                foregroundCpu10sMs.append(used)
                logger.notice("perf foreground: \(format(used), privacy: .public) ms CPU in the 10 s after becoming active")
            }
        }
    }

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
        ScrollTrace.begin("cold")
        launchInterval = signposter.beginInterval("Launch")
        if let processStart {
            logger.notice("perf launch: process start → app init \(format(ms(since: processStart, to: now)), privacy: .public) ms")
        }
        // CPU used by the launch and the work deferred after it (the photo backup's first pass starts at ~15 s).
        let since = processStart ?? now
        for seconds in [10.0, 30.0] {
            let delay = max(0, seconds - now.timeIntervalSince(since))
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
                MainActor.assumeIsolated {
                    let used = cpuMs()
                    if seconds == 10 { cpuFirst10sMs = used } else { cpuFirst30sMs = used }
                    logger.notice("perf launch: \(format(used), privacy: .public) ms CPU in the first \(Int(seconds)) s")
                }
            }
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
        if let firstMessageMs, let firstMessageSource {
            lines.append("launch: first message \(format(firstMessageMs)) ms (\(firstMessageSource))")
            lines.append("launch: first message items \(firstMessageCount)")
        }
        if let chatSyncedMs { lines.append("launch: chat synced \(format(chatSyncedMs)) ms") }
        if let cpuFirst10sMs { lines.append("launch: CPU in the first 10 s \(format(cpuFirst10sMs)) ms") }
        if let cpuFirst30sMs { lines.append("launch: CPU in the first 30 s \(format(cpuFirst30sMs)) ms") }
        for value in foregroundSyncedMs { lines.append("foreground: chat synced \(format(value)) ms after becoming active") }
        for value in foregroundCpu10sMs { lines.append("foreground: CPU in the 10 s after becoming active \(format(value)) ms") }
        let setupTotal = setup.reduce(0) { $0 + $1.ms }
        lines.append("setup: AppModel \(format(setupTotal)) ms in \(setup.count) steps")
        for step in setup.sorted(by: { $0.ms > $1.ms }) { lines.append("  setup \(step.name): \(format(step.ms)) ms") }
        for s in switches { lines.append("tab: \(s.from.rawValue) → \(s.to.rawValue) \(format(s.ms)) ms") }
        if let lucinaReadyMs { lines.append("lucina: stage ready \(format(lucinaReadyMs)) ms after first open") }
        for (key, value) in counts.sorted(by: { $0.key < $1.key }) { lines.append("count \(key): \(value)") }
        return lines
    }
}

/// `-SecretaryPerfScroll`: a timeline of what the chat list does after a launch or a return to the
/// foreground (data changes, the app's own scrolls, the reader's scroll phases and offsets), and when a
/// reader's drag first moved it. Written to `Documents/perf-scroll.txt` 12 s after each start.
@MainActor
enum ScrollTrace {
    nonisolated static let enabled = ProcessInfo.processInfo.arguments.contains("-SecretaryPerfScroll")
    private static var lines: [String] = []
    private static var epoch: Date?
    private static var label = "cold"
    private static var readerActive = false
    private static var readerStart: CGFloat?
    private static var lastOffset: CGFloat?
    private static var lastHeight: CGFloat?
    /// Start → the first reader drag that moved the list more than 30 pt, in ms.
    private(set) static var firstReaderScrollMs: Double?
    private static var readerTouches = 0

    /// A cold launch (App init) or a return to the foreground.
    static func begin(_ kind: String) {
        guard enabled else { return }
        epoch = kind == "cold" ? (PerfLog.processStart ?? Date()) : Date()
        label = kind
        lines = []
        firstReaderScrollMs = nil
        readerTouches = 0
        let started = epoch
        // Where each touch lands (windows come and go: sheets, the keyboard, alerts).
        for seconds in [0.3, 1.0, 2.0, 4.0] {
            DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { MainActor.assumeIsolated { TouchProbe.install() } }
        }
        let delay = max(0, 12 - Date().timeIntervalSince(started ?? Date()))
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
            MainActor.assumeIsolated { if epoch == started { write() } }
        }
    }

    private static var now: String {
        guard let epoch else { return "?" }
        return String(format: "%7.1f", Date().timeIntervalSince(epoch) * 1000)
    }

    static func event(_ text: @autoclosure () -> String) {
        guard enabled, epoch != nil, lines.count < 4000 else { return }
        lines.append("\(now) \(text())")
    }

    static func phase(_ name: String, reader: Bool) {
        guard enabled else { return }
        if reader, !readerActive { readerStart = lastOffset; readerTouches += 1 }
        readerActive = reader
        event("phase \(name)")
    }

    static func geometry(offset: CGFloat, height: CGFloat) {
        guard enabled else { return }
        defer { lastOffset = offset; lastHeight = height }
        if let lastHeight, abs(lastHeight - height) > 0.5 {
            event(String(format: "content height %.0f → %.0f (offset %.0f)", lastHeight, height, offset))
        }
        guard readerActive else { return }
        if let start = readerStart, abs(offset - start) > 30, firstReaderScrollMs == nil, let epoch {
            firstReaderScrollMs = (Date().timeIntervalSince(epoch) * 10_000).rounded() / 10
            event(String(format: "READER MOVED THE LIST %.0f pt (touch %d)", offset - start, readerTouches))
        }
        if let last = lastOffset, abs(last - offset) > 40 {
            event(String(format: "offset %.0f → %.0f during a reader scroll", last, offset))
        }
    }

    /// Logs every touch that begins, with the view it hits (it never takes part in the touch).
    final class TouchProbe: NSObject, UIGestureRecognizerDelegate {
        static let shared = TouchProbe()
        private var probed = Set<ObjectIdentifier>()

        static func install() {
            let windows = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.flatMap(\.windows)
            ScrollTrace.event("windows: " + windows.map { "\(type(of: $0)) level \($0.windowLevel.rawValue) hidden \($0.isHidden) key \($0.isKeyWindow)" }.joined(separator: "; "))
            for window in windows where !shared.probed.contains(ObjectIdentifier(window)) {
                shared.probed.insert(ObjectIdentifier(window))
                let probe = UILongPressGestureRecognizer(target: shared, action: #selector(touched(_:)))
                probe.minimumPressDuration = 0
                probe.cancelsTouchesInView = false
                probe.delaysTouchesBegan = false
                probe.delaysTouchesEnded = false
                probe.delegate = shared
                window.addGestureRecognizer(probe)
            }
        }

        @objc private func touched(_ recognizer: UILongPressGestureRecognizer) {
            guard recognizer.state == .began, let window = recognizer.view as? UIWindow else { return }
            let point = recognizer.location(in: window)
            var chain: [String] = []
            var view = window.hitTest(point, with: nil)
            while let current = view, chain.count < 6 {
                chain.append(String(describing: type(of: current)))
                view = current.superview
            }
            MainActor.assumeIsolated {
                ScrollTrace.event(String(format: "touch at %.0f,%.0f in %@ hits ", point.x, point.y, String(describing: type(of: window))) + chain.joined(separator: " < "))
            }
        }

        func gestureRecognizer(_ g: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }
    }

    static var url: URL {
        let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return documents.appendingPathComponent("perf-scroll.txt")
    }

    private static func write() {
        let stamp = ISO8601DateFormatter().string(from: Date())
        var text = "== scroll trace \(label) \(stamp)\n"
        let first = firstReaderScrollMs.map { String(format: "%.1f ms", $0) } ?? "never"
        text += "first reader scroll that moved the list: \(first) after \(label == "cold" ? "process start" : "becoming active") (\(readerTouches) touches)\n"
        text += PerfLog.summary().prefix(6).joined(separator: "\n") + "\n"
        text += lines.joined(separator: "\n") + "\n== done\n"
        PerfLog.logger.notice("perf scroll (\(label, privacy: .public)): first reader scroll \(first, privacy: .public)")
        let url = url
        if !FileManager.default.fileExists(atPath: url.path) { FileManager.default.createFile(atPath: url.path, contents: nil) }
        guard let handle = try? FileHandle(forWritingTo: url) else { return }
        defer { try? handle.close() }
        _ = try? handle.seekToEnd()
        try? handle.write(contentsOf: Data(text.utf8))
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
