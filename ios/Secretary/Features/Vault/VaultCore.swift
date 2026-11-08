import Foundation
import Observation
import SecretaryCore

// An Obsidian vault in the app: plain Markdown files in Documents/Vault, read by the same Rust core as Juno and Android
// (`lucina-vault` through `secretary-ffi`, docs/lucina-vault.md). Swift holds no vault logic: it asks and draws.

/// The vault's wire types. Rust's protocol is the source: `crates/lucina-vault/src/{protocol,markdown}.rs`.
struct VaultSpan: Decodable, Hashable {
    var text: String
    var bold: Bool?
    var italic: Bool?
    var strike: Bool?
    var code: Bool?
    var mark: Bool?
    var link: String?
    var external: Bool?
    var tag: String?
}

struct VaultItem: Decodable, Hashable {
    var task: Bool?
    var blocks: [VaultBlock]
}

indirect enum VaultBlock: Decodable, Hashable {
    case heading(level: Int, spans: [VaultSpan])
    case paragraph(spans: [VaultSpan])
    case code(language: String, text: String)
    case math(text: String)
    case quote(blocks: [VaultBlock])
    case callout(kind: String, title: [VaultSpan], blocks: [VaultBlock])
    case list(ordered: Bool, start: Int, items: [VaultItem])
    case table(head: [[VaultSpan]], rows: [[[VaultSpan]]])
    case rule
    case embed(target: String, alt: String, external: Bool)
    case html(text: String)
    case footnote(label: String, blocks: [VaultBlock])

    private enum Keys: String, CodingKey {
        case type, level, spans, language, text, blocks, kind, title, ordered, start, items, head, rows, target, alt, external, label
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "heading": self = .heading(level: try c.decode(Int.self, forKey: .level), spans: try c.decode([VaultSpan].self, forKey: .spans))
        case "paragraph": self = .paragraph(spans: try c.decode([VaultSpan].self, forKey: .spans))
        case "code": self = .code(language: try c.decode(String.self, forKey: .language), text: try c.decode(String.self, forKey: .text))
        case "math": self = .math(text: try c.decode(String.self, forKey: .text))
        case "quote": self = .quote(blocks: try c.decode([VaultBlock].self, forKey: .blocks))
        case "callout":
            self = .callout(kind: try c.decode(String.self, forKey: .kind),
                            title: try c.decode([VaultSpan].self, forKey: .title),
                            blocks: try c.decode([VaultBlock].self, forKey: .blocks))
        case "list":
            self = .list(ordered: try c.decode(Bool.self, forKey: .ordered), start: try c.decode(Int.self, forKey: .start),
                         items: try c.decode([VaultItem].self, forKey: .items))
        case "table": self = .table(head: try c.decode([[VaultSpan]].self, forKey: .head), rows: try c.decode([[[VaultSpan]]].self, forKey: .rows))
        case "rule": self = .rule
        case "embed":
            self = .embed(target: try c.decode(String.self, forKey: .target), alt: try c.decode(String.self, forKey: .alt),
                          external: try c.decode(Bool.self, forKey: .external))
        case "html": self = .html(text: try c.decode(String.self, forKey: .text))
        case "footnoteDefinition": self = .footnote(label: try c.decode(String.self, forKey: .label), blocks: try c.decode([VaultBlock].self, forKey: .blocks))
        case let other: throw DecodingError.dataCorruptedError(forKey: .type, in: c, debugDescription: "unknown block \(other)")
        }
    }
}

struct VaultBacklink: Decodable, Hashable, Identifiable {
    var from: String
    var line: Int
    var link: String
    var id: String { "\(from):\(line):\(link)" }
    var name: String { VaultCore.name(of: from) }
}

struct VaultOutgoing: Decodable, Hashable {
    var link: String
    var display: String?
    var embed: Bool
    var dest: String?
}

struct VaultNote: Decodable, Hashable {
    var path: String
    var name: String
    var text: String
    var revision: String
    var blocks: [VaultBlock]
    var tags: [String]
    var aliases: [String]
    var outgoing: [VaultOutgoing]
    var backlinks: [VaultBacklink]
}

struct VaultListedNote: Decodable, Hashable, Identifiable {
    var path: String
    var name: String
    var id: String { path }
}

struct VaultListing: Decodable, Hashable {
    var folder: String
    var folders: [String]
    var notes: [VaultListedNote]
    var files: [String]
}

struct VaultCounts: Decodable, Hashable {
    var notes: Int
    var attachments: Int
    var links: Int
    var resolved: Int
    var unresolved: Int
    var tags: Int
    var skipped: Int
}

struct VaultStatus: Decodable, Hashable {
    var root: String
    var writable: Bool
    var counts: VaultCounts
}

struct VaultHit: Decodable, Hashable, Identifiable {
    var path: String
    var name: String
    var matches: Int
    var lines: [VaultHitLine]
    var id: String { path }
}

struct VaultHitLine: Decodable, Hashable {
    var line: Int
    var text: String
    init(from decoder: Decoder) throws {
        var c = try decoder.unkeyedContainer()
        line = try c.decode(Int.self)
        text = try c.decode(String.self)
    }
}

struct VaultTag: Decodable, Hashable, Identifiable {
    var tag: String
    var count: Int
    var id: String { tag }
}

struct VaultNode: Decodable, Hashable {
    var path: String
    var name: String
    var degree: Int
    var x: Double
    var y: Double
}

struct VaultGraph: Decodable, Hashable {
    var nodes: [VaultNode]
    var edges: [[Int]]
}

enum VaultError: LocalizedError, Equatable {
    case core(code: String, message: String)
    case conflict
    var errorDescription: String? {
        switch self {
        case let .core(_, message): message
        case .conflict: String(localized: "This note changed on disk after you opened it. Nothing was overwritten.")
        }
    }
}

private struct VaultEnvelope: Decodable {
    var ok: Bool
    var code: String?
    var error: String?
}

/// One open vault, on one serial queue: the core is not asked two things at once, and nothing blocks the main thread.
final class VaultCore: @unchecked Sendable {
    private let queue = DispatchQueue(label: "tech.getsecretary.vault")
    private var handle: OpaquePointer?

    deinit {
        if let handle { secretary_vault_close(handle) }
    }

    /// What Obsidian shows for a path: the file name without `.md`.
    static func name(of path: String) -> String {
        let base = path.split(separator: "/").last.map(String.init) ?? path
        return base.lowercased().hasSuffix(".md") ? String(base.dropLast(3)) : base
    }

    /// Opens the folder. Returns the core's reason when it cannot.
    func open(_ root: URL) async -> String? {
        await withCheckedContinuation { continuation in
            queue.async { [self] in
                if let handle { secretary_vault_close(handle) }
                handle = nil
                guard let h = root.path.withCString({ secretary_vault_open($0) }) else {
                    var message = "unknown error"
                    if let raw = secretary_open_error() {
                        message = String(cString: raw)
                        secretary_string_free(raw)
                    }
                    continuation.resume(returning: message)
                    return
                }
                handle = h
                continuation.resume(returning: nil)
            }
        }
    }

    /// One request of the protocol, decoded.
    func ask<T: Decodable>(_ request: [String: Any], as type: T.Type = T.self) async throws -> T {
        let data = try JSONSerialization.data(withJSONObject: request)
        let answer: Data = try await withCheckedThrowingContinuation { continuation in
            queue.async { [self] in
                guard let handle, let json = String(data: data, encoding: .utf8) else {
                    continuation.resume(throwing: VaultError.core(code: "closed", message: "The vault is not open."))
                    return
                }
                guard let raw = json.withCString({ secretary_vault_request(handle, $0) }) else {
                    continuation.resume(throwing: VaultError.core(code: "bridge", message: "No answer from the vault."))
                    return
                }
                let out = Data(bytes: raw, count: strlen(raw))
                secretary_string_free(raw)
                continuation.resume(returning: out)
            }
        }
        let e = try JSONDecoder().decode(VaultEnvelope.self, from: answer)
        guard e.ok else {
            if e.code == "conflict" { throw VaultError.conflict }
            throw VaultError.core(code: e.code ?? "unknown", message: e.error ?? "The vault refused.")
        }
        return try JSONDecoder().decode(T.self, from: answer)
    }
}

/// A place to push in the vault's navigation.
enum VaultRoute: Hashable {
    case folder(String)
    case note(String)
    case tags
    case graph(around: String?)
    case search(String)
}

@MainActor
@Observable
final class VaultStore {
    /// The app's own copy until the sync lands (docs/lucina-vault.md): plain files, nothing of ours among them.
    static var root: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("Vault", isDirectory: true)
    }

    let core = VaultCore()
    var status: VaultStatus?
    var openError: String?
    var opened = false

    func open() async {
        guard !opened else { return }
        let start = Date()
        let root = Self.root
        if !FileManager.default.fileExists(atPath: root.path) {
            opened = true
            openError = nil
            return
        }
        if let reason = await core.open(root) {
            openError = reason
            DebugEvents.record("vault.open_failed", ["elapsed_ms": Int(Date().timeIntervalSince(start) * 1000)])
            return
        }
        do {
            let s: VaultStatus = try await core.ask(["op": "status"])
            status = s
            opened = true
            // Counts only: names and text of notes stay on the phone.
            DebugEvents.record("vault.opened", [
                "elapsed_ms": Int(Date().timeIntervalSince(start) * 1000),
                "md_files": s.counts.notes, "links": s.counts.links, "unresolved": s.counts.unresolved, "tags": s.counts.tags,
            ])
        } catch {
            openError = error.localizedDescription
        }
    }

    /// Once per build, in the background: how the core does on this phone, as debug events with numbers only. It answers
    /// "does the vault open on the iPhone" without anyone having to look.
    nonisolated static func checkOnce() {
        let done = "vault.checked.\(Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "")"
        guard !UserDefaults.standard.bool(forKey: done) else { return }
        let root = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("Vault", isDirectory: true)
        guard FileManager.default.fileExists(atPath: root.path) else { return }
        Task.detached(priority: .utility) {
            let core = VaultCore()
            let t0 = Date()
            if let reason = await core.open(root) {
                DebugEvents.record("vault.check_failed", ["reason": reason])
                return
            }
            let opened = Date()
            guard let s: VaultStatus = try? await core.ask(["op": "status"]) else { return }
            struct Any_: Decodable {}
            let t1 = Date()
            let _: Any_? = try? await core.ask(["op": "search", "query": "the", "limit": 50])
            let t2 = Date()
            let graph: VaultGraph? = try? await core.ask(["op": "graph"])
            let t3 = Date()
            let _: Any_? = try? await core.ask(["op": "note", "path": "00 Home.md"])
            let t4 = Date()
            func ms(_ a: Date, _ b: Date) -> Int { Int(b.timeIntervalSince(a) * 1000) }
            DebugEvents.record("vault.check", [
                "open_ms": ms(t0, opened), "search_ms": ms(t1, t2), "graph_ms": ms(t2, t3), "page_ms": ms(t3, t4),
                "md_files": s.counts.notes, "links": s.counts.links, "unresolved": s.counts.unresolved,
                "tags": s.counts.tags, "nodes": graph?.nodes.count ?? -1,
            ])
            UserDefaults.standard.set(true, forKey: done)
        }
    }

    func allowEditing() async {
        struct Writable: Decodable { var writable: Bool }
        if let w: Writable = try? await core.ask(["op": "set_writable", "writable": true]) {
            status?.writable = w.writable
        }
    }
}
