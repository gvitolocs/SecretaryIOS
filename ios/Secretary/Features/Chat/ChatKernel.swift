import Foundation
import os
import SecretaryCore

/// The chat engine's rules live in the app's Rust core (`crates/secretary-chat`, proved in
/// `verification/verus/chat_merge.rs`): the timeline merge and its order, the sync cursor loop and the older-page
/// cursor. Swift keeps the messages and sends small key records through `secretary_chat_request` (JSON in, JSON
/// out; the protocol is in docs/server-api.md, "Chat engine kernel").
enum ChatKernel {
    struct Failure: Error, CustomStringConvertible {
        let description: String
    }

    static let logger = Logger(subsystem: "tech.getsecretary.app", category: "ChatKernel")

    /// One request; the answer's object, or the kernel's `error`.
    static func call(_ request: [String: Any]) throws -> [String: Any] {
        let body = try JSONSerialization.data(withJSONObject: request)
        let json = String(decoding: body, as: UTF8.self)
        guard let raw = json.withCString({ secretary_chat_request($0) }) else { throw Failure(description: "no answer") }
        defer { secretary_string_free(raw) }
        let text = String(cString: raw)
        guard let answer = try JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any] else {
            throw Failure(description: "the answer is not an object")
        }
        if let error = answer["error"] as? String { throw Failure(description: error) }
        return answer
    }

    /// What the merge reads of an item. `share_id` only for a share and for a local bubble that carries one
    /// (Swift's old rule: an incoming share matched a local item's `meta.shareId`).
    static func key(_ item: TimelineItem) -> [String: Any] {
        var key: [String: Any] = [
            "id": item.id,
            "ts_us": Int64((item.ts.timeIntervalSince1970 * 1_000_000).rounded()),
            "role": item.role.rawValue,
        ]
        if let client = item.clientMessageId { key["client_message_id"] = client }
        if let reply = item.replyTo { key["reply_to"] = reply }
        if let share = item.meta?.shareId, item.meta?.isShare == true || item.isLocal { key["share_id"] = share }
        return key
    }

    /// `older`: the cursor for the next older page and whether there is one, for a page read from the cache.
    static func older(ids: [String], stored: String?, pageLength: Int, pageSize: Int) -> (cursor: String?, hasOlder: Bool) {
        var request: [String: Any] = ["op": "older", "ids": ids, "page_len": pageLength, "page_size": pageSize]
        request["stored"] = stored ?? NSNull()
        do {
            let answer = try call(request)
            return (answer["older_cursor"] as? String, answer["has_older"] as? Bool ?? false)
        } catch {
            logger.fault("older: \(String(describing: error), privacy: .public)")
            assertionFailure("chat kernel: \(error)")
            return (stored?.isEmpty == false ? stored : nil, pageLength == pageSize)
        }
    }
}

/// `sync_next`: one step of the sync cursor loop (the plan's state goes back and forth as JSON).
struct ChatSyncPlan {
    enum Action: Equatable {
        case fetch(after: String?, limit: Int)
        case apply(reset: Bool)
        case done
        case abort
    }

    enum Outcome {
        case started
        case page(cursor: String?, hasMore: Bool)
        case applied
        case gone410
        case failed
    }

    private let cursor: String?
    private var plan: Any = NSNull()

    init(cursor: String?) { self.cursor = cursor }

    mutating func next(_ outcome: Outcome) throws -> Action {
        let reported: [String: Any]
        switch outcome {
        case .started: reported = ["kind": "started"]
        case let .page(cursor, hasMore): reported = ["kind": "page", "cursor": cursor ?? NSNull(), "has_more": hasMore]
        case .applied: reported = ["kind": "applied"]
        case .gone410: reported = ["kind": "gone410"]
        case .failed: reported = ["kind": "failed"]
        }
        let answer = try ChatKernel.call(["op": "sync_next", "plan": plan, "cursor": cursor ?? NSNull(), "outcome": reported])
        plan = answer["plan"] ?? NSNull()
        guard let action = answer["action"] as? [String: Any], let kind = action["kind"] as? String else {
            throw ChatKernel.Failure(description: "sync_next: no action")
        }
        switch kind {
        case "fetch": return .fetch(after: action["after"] as? String, limit: action["limit"] as? Int ?? 50)
        case "apply": return .apply(reset: action["reset"] as? Bool ?? false)
        case "done": return .done
        default: return .abort
        }
    }
}
