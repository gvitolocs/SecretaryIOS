import Foundation
import os

/// The debug event log (docs/debug-events.md): what happened on this iPhone (taps, intents, notifications, Live
/// Activities, Face ID, approvals, failed requests), readable on the server without USB.
///
/// `DebugEvents.record("ui.tap", ["element": "approve", "screen": "approval_sheet"])` is cheap and never blocks: the
/// fields are cleaned on the caller's thread and the line is appended on a utility queue. Each process (app, widgets,
/// share, photoupload, …) keeps its own ring buffer `DebugEvents/<process>.jsonl` in the App Group container (or its
/// own Application Support without one): at most `maxBytes` / `maxEvents`, oldest dropped. Each process uploads its
/// own buffer (`flush`) to `POST /v1/debug/events` with the device token from the shared keychain, in batches of
/// `batchSize`, and remembers the last uploaded `seq`.
///
/// Never recorded (the server refuses them too): secrets, tokens, keys, signatures, message / note / command text,
/// plan details, URL queries, bodies. Field names with a forbidden word are dropped (`dropped_fields` counts them);
/// secret-looking strings become `[redacted]`; URLs lose their query; ids in `path` / `route` become `{id}`.
enum DebugEvents {
    static let maxBytes = 2_000_000
    static let maxEvents = 5_000
    /// Kept when the buffer is compacted.
    static let keepEvents = 4_000
    static let batchSize = 200
    static let maxValueChars = 200
    static let maxFields = 40
    static let redacted = "[redacted]"
    /// Shared keychain item: `off` stops uploads in every process (Settings → About → Send debug events).
    static let uploadSwitch = KeychainStore(service: "tech.getsecretary.shared", account: "debug-events-upload",
                                            accessGroup: BridgeCredentials.accessGroup)
    static let lastUploadKey = "secretary.debugEvents.lastUpload"

    /// Same list as secretary-server's `debug_events::FORBIDDEN_WORDS`.
    static let forbiddenWords: Set<String> = [
        // secrets
        "token", "tokens", "secret", "secrets", "password", "passwd", "passphrase", "key", "keys",
        "signature", "sig", "cookie", "cookies", "authorization", "credential", "credentials",
        "otp", "pin", "nonce", "jwt", "bearer", "private",
        // content
        "body", "text", "message", "messages", "msg", "content", "note", "notes", "command",
        "commands", "cmd", "plan", "prompt", "transcript", "chat", "reply", "title", "subtitle",
        "summary", "description", "detail", "details", "payload", "email", "address",
        // URLs and their queries
        "url", "urls", "uri", "query", "querystring",
    ]

    // MARK: - Recording

    /// Records one event. `type` is dotted lowercase (`ui.tap`, `notification.action`); fields are flat: strings,
    /// numbers and booleans (an `Error` becomes its domain and code, anything else its description, cleaned).
    static func record(_ type: String, _ fields: [String: Any] = [:]) {
        let ts = Date()
        let clean = sanitize(fields)
        queue.async { buffer.append(type: type, ts: ts, fields: clean) }
    }

    /// `net.request`: method, the path with ids as `{id}`, status or error, duration.
    static func request(_ method: String, _ path: String, status: Int? = nil, error: Error? = nil, since start: Date) {
        var fields: [String: Any] = ["method": method, "path": path,
                                     "duration_ms": Int(Date().timeIntervalSince(start) * 1000)]
        if let status { fields["status"] = status }
        if let error { fields["error"] = error }
        record("net.request", fields)
    }

    /// A card, widget, Live Activity or Shortcuts intent: `card.intent` phase `start` now, phase `end` (with
    /// `duration_ms`, and `ok` / `error` when given) from `end`, then an upload. Use as
    /// `let trace = DebugEvents.intent("deny_approval", ["id": id]); defer { trace.end() }`.
    static func intent(_ name: String, _ fields: [String: Any] = [:]) -> IntentTrace {
        record("card.intent", fields.merging(["intent": name, "phase": "start"]) { $1 })
        return IntentTrace(name: name, fields: fields)
    }

    struct IntentTrace {
        let name: String
        let fields: [String: Any]
        let started = Date()

        func end(error: Error? = nil) {
            var f = fields
            f["intent"] = name
            f["phase"] = "end"
            f["duration_ms"] = Int(Date().timeIntervalSince(started) * 1000)
            if let error { f["ok"] = false; f["error"] = error }
            DebugEvents.record("card.intent", f)
            DebugEvents.flushSoon()
        }
    }

    // MARK: - Sanitizer

    /// The words of a field name: `authToken` → auth, token; `unlock_key.id` → unlock, key, id.
    static func words(_ key: String) -> [String] {
        var out: [String] = []
        var current = ""
        var previousLower = false
        for c in key {
            if c == "_" || c == "." || c == "-" {
                if !current.isEmpty { out.append(current); current = "" }
                previousLower = false
                continue
            }
            if c.isUppercase, previousLower, !current.isEmpty { out.append(current); current = "" }
            previousLower = c.isLowercase || c.isNumber
            current += c.lowercased()
        }
        if !current.isEmpty { out.append(current) }
        return out
    }

    static func isForbidden(_ key: String) -> Bool {
        words(key).contains { forbiddenWords.contains($0) }
    }

    private static func validKey(_ key: String) -> Bool {
        !key.isEmpty && key.count <= 48 && key.unicodeScalars.allSatisfy {
            ($0.isASCII && (CharacterSet.alphanumerics.contains($0))) || $0 == "_" || $0 == "."
        }
    }

    static func isValidType(_ type: String) -> Bool {
        !type.isEmpty && type.count <= 64 && type.split(separator: ".", omittingEmptySubsequences: false).allSatisfy { part in
            !part.isEmpty && part.unicodeScalars.allSatisfy { ("a"..."z").contains($0) || ("0"..."9").contains($0) || $0 == "_" }
        }
    }

    /// Auth schemes, our token prefixes, JWTs, or a long unbroken run of base64 / hex.
    static func looksSecret(_ s: String) -> Bool {
        let lower = s.lowercased()
        if ["bearer ", "basic ", "digest "].contains(where: { lower.contains($0) }) { return true }
        let words = s.split(whereSeparator: { $0.isWhitespace || $0 == "/" || $0 == "," })
        if words.contains(where: { w in ["sec_", "svc_", "mcp_", "eyJ"].contains { w.hasPrefix($0) } }) { return true }
        let runs = s.split(whereSeparator: { !($0.isASCII && ($0.isLetter || $0.isNumber || "+=_-".contains($0))) })
        return runs.contains { $0.count >= 40 }
    }

    /// `/v1/approvals/ap_8f3a19c2d4/decide?x=1` → `/v1/approvals/{id}/decide`. Note slugs are content: `{id}` too.
    static func templatePath(_ path: String) -> String {
        let bare = path.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? ""
        let segments = bare.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        var out: [String] = []
        for (i, seg) in segments.enumerated() {
            let afterContent = i > 0 && ["notes", "files"].contains(segments[i - 1])
            let idLike = !seg.isEmpty && !(seg.hasPrefix("{") && seg.hasSuffix("}")) && (
                afterContent
                || seg.allSatisfy(\.isNumber)
                || (seg.count >= 8 && seg.contains(where: \.isNumber)
                    && seg.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "_" || $0 == "-") })
                || seg.contains("@") || seg.contains("%") || seg.count > 40)
            out.append(idLike ? "{id}" : seg)
        }
        return out.joined(separator: "/")
    }

    static func cleanString(key: String, _ value: String) -> String {
        var s = String(String.UnicodeScalarView(value.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) }))
        if s.contains("://") {
            if let cut = s.firstIndex(where: { $0 == "?" || $0 == "#" }) { s = String(s[..<cut]) }
        }
        if let scheme = s.range(of: "://") {
            let rest = s[scheme.upperBound...]
            if let at = rest.firstIndex(of: "@"), !rest[..<at].contains("/") {
                s = String(s[..<scheme.upperBound]) + rest[rest.index(after: at)...]
            }
        }
        if key == "path" || key == "route" || key.hasSuffix("_path") || key.hasSuffix("_route") {
            s = templatePath(s)
        }
        if looksSecret(s) { return redacted }
        return String(s.prefix(maxValueChars))
    }

    /// An error as `domain:code` (`NSURLErrorDomain:-1009`, `APIError:timeout`): never its message.
    static func errorCode(_ error: Error) -> String {
        if let api = error as? APIErrorCodeProviding { return api.debugCode }
        let ns = error as NSError
        return "\(ns.domain):\(ns.code)"
    }

    enum Value: Equatable, Encodable {
        case string(String), int(Int), double(Double), bool(Bool)

        func encode(to encoder: Encoder) throws {
            var c = encoder.singleValueContainer()
            switch self {
            case .string(let s): try c.encode(s)
            case .int(let i): try c.encode(i)
            case .double(let d): try c.encode(d)
            case .bool(let b): try c.encode(b)
            }
        }
    }

    /// The fields as recorded: forbidden names dropped (and counted), values cleaned, at most `maxFields`.
    static func sanitize(_ fields: [String: Any]) -> [String: Value] {
        var out: [String: Value] = [:]
        var dropped = 0
        for key in fields.keys.sorted() {
            guard let raw = fields[key] else { continue }
            if isForbidden(key) || !validKey(key) { dropped += 1; continue }
            guard out.count < maxFields else { break }
            let value: Value
            switch raw {
            case let b as Bool: value = .bool(b)
            case let i as Int: value = .int(i)
            case let d as Double: value = d.isFinite ? .double(d) : .string(String(d))
            case let s as String: value = .string(cleanString(key: key, s))
            case let e as Error: value = .string(cleanString(key: key, errorCode(e)))
            default: value = .string(cleanString(key: key, String(describing: raw)))
            }
            out[key] = value
        }
        if dropped > 0 { out["dropped_fields"] = .int(dropped) }
        return out
    }

    // MARK: - Identity of this process and build

    /// `app`, `widgets`, `share`, `photoupload`, … from the bundle id.
    static let process: String = {
        let id = Bundle.main.bundleIdentifier ?? ""
        let base = "tech.getsecretary.app"
        guard id.hasPrefix(base + ".") else { return id == base ? "app" : "unknown" }
        let suffix = id.dropFirst(base.count + 1).replacingOccurrences(of: ".", with: "_")
        return suffix.filter { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "_" || $0 == "-") }.lowercased()
    }()

    /// The git commit the build was made from (`SecretaryBuildCommit`, stamped by the app target's build phase).
    /// Extensions read it from the app bundle they sit in.
    static let build: String = {
        func stamped(_ bundle: Bundle?) -> String? {
            (bundle?.object(forInfoDictionaryKey: "SecretaryBuildCommit") as? String)
                .flatMap { $0.isEmpty || $0 == "unknown" || $0.hasPrefix("$(") ? nil : $0 }
        }
        let app = Bundle(url: Bundle.main.bundleURL.deletingLastPathComponent().deletingLastPathComponent())
        let raw = stamped(.main) ?? stamped(app) ?? "unknown"
        return String(raw.filter { $0.isASCII && ($0.isLetter || $0.isNumber || "._+-".contains($0)) }.prefix(40))
    }()

    // MARK: - Buffer

    private static let queue = DispatchQueue(label: "tech.getsecretary.debugEvents", qos: .utility)
    private static let buffer = Buffer(directory: defaultDirectory)
    static let log = Logger(subsystem: "tech.getsecretary.app", category: "DebugEvents")

    static var defaultDirectory: URL {
        let root = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: "group.tech.getsecretary.shared")
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return root.appendingPathComponent("DebugEvents", isDirectory: true)
    }

    /// One process's ring buffer and upload mark. Used only on `queue` (tests make their own).
    final class Buffer {
        struct State: Codable { var nextSeq: Int = 1; var uploadedSeq: Int = 0 }

        let file: URL
        let stateFile: URL
        private var state: State?
        private var count: Int?
        private let process: String
        private let build: String

        init(directory: URL, process: String = DebugEvents.process, build: String = DebugEvents.build) {
            self.process = process
            self.build = build
            file = directory.appendingPathComponent("\(process).jsonl")
            stateFile = directory.appendingPathComponent("\(process).state.json")
        }

        /// `nextSeq` is saved every `saveEvery` events; after a crash it continues from the buffer's last line.
        private static let saveEvery = 50

        private func loadState() -> State {
            if let state { return state }
            var loaded = (try? Data(contentsOf: stateFile)).flatMap { try? JSONDecoder().decode(State.self, from: $0) } ?? State()
            if let last = lines().last, let seq = Self.seq(of: last) { loaded.nextSeq = max(loaded.nextSeq, seq + 1) }
            state = loaded
            return loaded
        }

        private func save(_ new: State) {
            state = new
            try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            if let data = try? JSONEncoder().encode(new) { try? data.write(to: stateFile, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication]) }
        }

        private static let formatter: ISO8601DateFormatter = {
            let f = ISO8601DateFormatter()
            f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            return f
        }()

        private static let encoder: JSONEncoder = {
            let e = JSONEncoder()
            e.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
            return e
        }()

        private struct Line: Encodable {
            let ts: String
            let seq: Int
            let build: String
            let process: String
            let type: String
            let fields: [String: Value]
        }

        @discardableResult
        func append(type: String, ts: Date, fields: [String: Value]) -> Int? {
            guard DebugEvents.isValidType(type) else {
                DebugEvents.log.error("bad debug event type \(type, privacy: .public)")
                return nil
            }
            var st = loadState()
            let seq = st.nextSeq
            let line = Line(ts: Self.formatter.string(from: ts), seq: seq, build: build, process: process, type: type, fields: fields)
            guard var data = try? Self.encoder.encode(line) else { return nil }
            data.append(0x0A)
            let fm = FileManager.default
            try? fm.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
            if !fm.fileExists(atPath: file.path) {
                fm.createFile(atPath: file.path, contents: nil, attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
                count = 0
            }
            guard let handle = try? FileHandle(forWritingTo: file) else { return nil }
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: data)
            st.nextSeq += 1
            if seq % Self.saveEvery == 0 { save(st) } else { state = st }
            let n = (count ?? lines().count) + (count == nil ? 0 : 1)
            count = n
            let size = (try? fm.attributesOfItem(atPath: file.path)[.size] as? Int) ?? 0
            if n > DebugEvents.maxEvents || size > DebugEvents.maxBytes { compact() }
            return seq
        }

        /// The raw lines, oldest first.
        func lines() -> [Data] {
            guard let data = try? Data(contentsOf: file) else { return [] }
            return data.split(separator: 0x0A).map { Data($0) }
        }

        /// Drops the oldest lines down to `keepEvents` (and under `maxBytes`).
        func compact() {
            var kept = Array(lines().suffix(DebugEvents.keepEvents))
            while kept.reduce(0, { $0 + $1.count + 1 }) > DebugEvents.maxBytes * 3 / 4, !kept.isEmpty { kept.removeFirst(kept.count / 4 + 1) }
            var out = Data()
            for l in kept { out.append(l); out.append(0x0A) }
            try? out.write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
            count = kept.count
        }

        private static func seq(of line: Data) -> Int? {
            (try? JSONSerialization.jsonObject(with: line) as? [String: Any])?["seq"] as? Int
        }

        /// The next lines to upload (seq above the mark), at most `limit`, and the highest seq among them.
        func pending(limit: Int) -> (lines: [Data], maxSeq: Int)? {
            let mark = loadState().uploadedSeq
            var out: [Data] = []
            var maxSeq = mark
            for line in lines() {
                guard let s = Self.seq(of: line), s > mark else { continue }
                out.append(line)
                maxSeq = max(maxSeq, s)
                if out.count == limit { break }
            }
            return out.isEmpty ? nil : (out, maxSeq)
        }

        func markUploaded(through seq: Int) {
            var st = loadState()
            st.uploadedSeq = max(st.uploadedSeq, seq)
            save(st)
        }
    }

    // MARK: - Upload

    enum UploadOutcome: Equatable { case nothing, uploaded(Int), disabled, noCredentials, retryLater, dropped }

    /// The token and server addresses (the app sets its own client's; extensions use the shared keychain item).
    nonisolated(unsafe) static var credentials: () -> (token: String, endpoints: [URL])? = {
        guard let c = BridgeCredentials.read(), !c.token.isEmpty else { return nil }
        let urls = c.endpoints.compactMap(URL.init(string:))
        return urls.isEmpty ? nil : (c.token, urls)
    }

    static var uploadsEnabled: Bool {
        get { (try? uploadSwitch.read()) != "off" }
        set { try? uploadSwitch.save(newValue ? "on" : "off") }
    }

    static var lastUpload: Date? { UserDefaults.standard.object(forKey: lastUploadKey) as? Date }

    private static let uploading = OSAllocatedUnfairLock(initialState: false)
    private static let session: URLSession = {
        let c = URLSessionConfiguration.ephemeral
        c.timeoutIntervalForRequest = 20
        c.httpCookieAcceptPolicy = .never
        c.httpShouldSetCookies = false
        c.waitsForConnectivity = false
        return URLSession(configuration: c)
    }()

    /// Uploads this process's pending events (a few batches at most). One flush at a time per process.
    @discardableResult
    static func flush(maxBatches: Int = 5) async -> UploadOutcome {
        guard uploadsEnabled else { return .disabled }
        guard uploading.withLock({ busy in
            if busy { return false }
            busy = true
            return true
        }) else { return .nothing }
        defer { uploading.withLock { $0 = false } }
        guard let creds = credentials() else { return .noCredentials }
        var total = 0
        for _ in 0..<maxBatches {
            guard let batch = queue.sync(execute: { buffer.pending(limit: batchSize) }) else { break }
            var body = Data(#"{"events":["#.utf8)
            for (i, line) in batch.lines.enumerated() {
                if i > 0 { body.append(0x2C) }
                body.append(line)
            }
            body.append(contentsOf: Array("]}".utf8))
            switch await post(body, creds) {
            case .some(200..<300):
                queue.sync { buffer.markUploaded(through: batch.maxSeq) }
                total += batch.lines.count
                UserDefaults.standard.set(Date(), forKey: lastUploadKey)
            case .some(429), .none, .some(500...):
                return total > 0 ? .uploaded(total) : .retryLater
            case .some:
                // A 4xx other than 429 would refuse the batch again: drop it.
                queue.sync { buffer.markUploaded(through: batch.maxSeq) }
                log.error("debug events dropped: \(batch.lines.count) events refused")
                return .dropped
            }
        }
        return total > 0 ? .uploaded(total) : .nothing
    }

    /// Fire and forget (after an intent or a notification action finished).
    static func flushSoon() {
        Task.detached(priority: .utility) { await flush() }
    }

    /// The HTTP status, or nil when no endpoint answered.
    private static func post(_ body: Data, _ creds: (token: String, endpoints: [URL])) async -> Int? {
        for base in creds.endpoints {
            var request = URLRequest(url: base.appendingPathComponent("v1/debug/events"))
            request.httpMethod = "POST"
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue("Bearer \(creds.token)", forHTTPHeaderField: "Authorization")
            if let answer = try? await session.data(for: request), let http = answer.1 as? HTTPURLResponse {
                return http.statusCode
            }
        }
        return nil
    }
}

/// Errors that name themselves in the debug log by a code, never by their message.
protocol APIErrorCodeProviding { var debugCode: String { get } }
