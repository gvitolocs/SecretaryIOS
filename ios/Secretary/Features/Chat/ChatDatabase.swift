import Foundation
import SQLite3

/// Serial, off-main storage. Connections are confined to one operation, including each transaction.
actor ChatDatabase {
    enum Scope: Sendable {
        case mine(String?)
        case conversation(String)

        var stateKey: String {
            switch self {
            case .mine: return "mine"
            case let .conversation(key): return "conversation:" + key
            }
        }
        var conversations: [String] {
            switch self {
            case let .mine(device): return ["legacy:ios", "lucina"] + (device.map { ["device:" + $0] } ?? [])
            case let .conversation(key): return [key]
            }
        }
    }

    struct Failure: Error { let message: String }
    nonisolated static var defaultURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Secretary/chat.sqlite")
    }
    let url: URL
    private let cap: Int

    init(url: URL = ChatDatabase.defaultURL, cap: Int = 5000) {
        self.url = url
        self.cap = cap
    }

    private func connection<T>(_ body: (OpaquePointer) throws -> T) throws -> T {
        let directory = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
            attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
        try FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], atPath: directory.path)
        var pointer: OpaquePointer?
        guard sqlite3_open(url.path, &pointer) == SQLITE_OK, let db = pointer else {
            if let pointer { sqlite3_close(pointer) }
            throw Failure(message: "Cannot open chat database")
        }
        defer { sqlite3_close(db) }
        try FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], atPath: url.path)
        sqlite3_busy_timeout(db, 3000)
        try execute(db, "PRAGMA journal_mode=WAL")
        try execute(db, "CREATE TABLE IF NOT EXISTS items(id TEXT PRIMARY KEY, conv TEXT NOT NULL, json BLOB NOT NULL)")
        try execute(db, "CREATE INDEX IF NOT EXISTS items_conv_id ON items(conv,id)")
        try execute(db, "CREATE TABLE IF NOT EXISTS state(key TEXT PRIMARY KEY,value TEXT)")
        try FileManager.default.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication], atPath: url.path)
        return try body(db)
    }

    private func execute(_ db: OpaquePointer, _ sql: String, _ values: [String] = []) throws {
        try statement(db, sql, values) { stmt in
            let result = sqlite3_step(stmt)
            guard result == SQLITE_DONE || result == SQLITE_ROW else {
                throw Failure(message: String(cString: sqlite3_errmsg(db)))
            }
        }
    }

    private func statement<T>(_ db: OpaquePointer, _ sql: String, _ values: [String],
                              _ body: (OpaquePointer) throws -> T) throws -> T {
        var pointer: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &pointer, nil) == SQLITE_OK, let stmt = pointer else {
            throw Failure(message: String(cString: sqlite3_errmsg(db)))
        }
        defer { sqlite3_finalize(stmt) }
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        for (index, value) in values.enumerated() {
            _ = value.withCString { sqlite3_bind_text(stmt, Int32(index + 1), $0, -1, transient) }
        }
        return try body(stmt)
    }

    func read(_ scope: Scope, before: String? = nil, limit: Int = 50) throws -> [TimelineItem] {
        try connection { db in
            let keys = scope.conversations
            let placeholders = Array(repeating: "?", count: keys.count).joined(separator: ",")
            let sql = "SELECT json FROM items WHERE conv IN (\(placeholders))"
                + (before == nil ? "" : " AND id < ?") + " ORDER BY id DESC LIMIT ?"
            return try statement(db, sql, keys + (before.map { [$0] } ?? []) + [String(limit)]) { stmt in
                var items: [TimelineItem] = []
                let decoder = JSONDecoder()
                while sqlite3_step(stmt) == SQLITE_ROW {
                    guard let bytes = sqlite3_column_blob(stmt, 0) else { continue }
                    let data = Data(bytes: bytes, count: Int(sqlite3_column_bytes(stmt, 0)))
                    if let item = try? decoder.decode(TimelineItem.self, from: data) { items.append(item) }
                }
                return TimelineMerge.sorted(items)
            }
        }
    }

    func value(_ key: String) throws -> String? {
        try connection { db in
            try statement(db, "SELECT value FROM state WHERE key=?", [key]) { stmt in
                guard sqlite3_step(stmt) == SQLITE_ROW, let value = sqlite3_column_text(stmt, 0) else { return nil }
                return String(cString: value)
            }
        }
    }

    func write(_ items: [TimelineItem], state: [String: String] = [:], reset: Scope? = nil,
               removeIDs: [String] = []) throws {
        try connection { db in
            try execute(db, "BEGIN IMMEDIATE")
            do {
                if let reset {
                    for key in reset.conversations { try execute(db, "DELETE FROM items WHERE conv=? AND id NOT LIKE 'local-%'", [key]) }
                    try execute(db, "DELETE FROM state WHERE key=?", [reset.stateKey])
                }
                for id in removeIDs { try execute(db, "DELETE FROM items WHERE id=?", [id]) }
                let encoder = JSONEncoder()
                for item in items {
                    let data = try encoder.encode(item)
                    try statement(db, "INSERT OR REPLACE INTO items(id,conv,json) VALUES(?,?,?)", [item.id, conversationKey(item)]) { stmt in
                        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
                        _ = data.withUnsafeBytes { sqlite3_bind_blob(stmt, 3, $0.baseAddress, Int32(data.count), transient) }
                        guard sqlite3_step(stmt) == SQLITE_DONE else { throw Failure(message: String(cString: sqlite3_errmsg(db))) }
                    }
                }
                for (key, value) in state { try execute(db, "INSERT OR REPLACE INTO state(key,value) VALUES(?,?)", [key, value]) }
                try execute(db, "DELETE FROM items WHERE id IN (SELECT id FROM items ORDER BY id DESC LIMIT -1 OFFSET ?)", [String(cap)])
                try execute(db, "COMMIT")
            } catch {
                try? execute(db, "ROLLBACK")
                throw error
            }
        }
    }

    func clear() throws {
        try connection { db in
            try execute(db, "BEGIN IMMEDIATE")
            do {
                try execute(db, "DELETE FROM items")
                try execute(db, "DELETE FROM state")
                try execute(db, "COMMIT")
            } catch { try? execute(db, "ROLLBACK"); throw error }
        }
    }
}

/// The server's conversation routing rule, shared by storage and the live mine filter.
func conversationKey(_ item: TimelineItem) -> String {
    if item.meta?.via == "mcp" {
        let caller = (item.meta?.caller ?? "").lowercased().filter { ("a"..."z").contains($0) || ("0"..."9").contains($0) || $0 == "_" || $0 == "-" }
        return "agent:" + (caller.isEmpty ? "mcp" : caller)
    }
    switch item.channel {
    case .web, .ios:
        if let device = item.meta?.device { return "device:" + device }
        return "legacy:" + item.channel.rawValue
    case .telegram, .discord, .alexa: return item.channel.rawValue
    default: return "lucina"
    }
}

func isMine(_ item: TimelineItem, deviceId: String?, sentClientIDs: Set<String> = []) -> Bool {
    if item.isLocal { return true }
    if deviceId == nil, item.meta?.device != nil,
       item.clientMessageId.map({ sentClientIDs.contains($0) }) != true { return false }
    let key = conversationKey(item)
    if key == "legacy:ios" || key == "lucina" { return true }
    if let deviceId, key == "device:" + deviceId { return true }
    return deviceId == nil && key.hasPrefix("device:") && item.clientMessageId.map { sentClientIDs.contains($0) } == true
}
