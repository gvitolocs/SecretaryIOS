import Foundation

struct ChatConversation: Codable, Equatable, Identifiable, Sendable {
    let key: String
    let kind: String
    let title: String
    let channel: String
    let device: String?
    let deviceName: String?
    let caller: String?
    let count: Int
    let lastId: String
    let lastTs: Date
    let lastRole: String
    let lastText: String

    var id: String { key }
    var canContinue: Bool { kind == "device" || kind == "legacy" }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        key = try c.decode(String.self, forKey: .key)
        kind = c.lenient(String.self, .kind, "lucina")
        title = c.lenient(String.self, .title, key)
        channel = c.lenient(String.self, .channel, "other")
        device = c.lenientOptional(String.self, .device)
        deviceName = c.lenientOptional(String.self, .deviceName)
        caller = c.lenientOptional(String.self, .caller)
        count = c.lenient(Int.self, .count, 0)
        lastId = c.lenientOptional(FlexibleID.self, .lastId)?.value ?? ""
        lastRole = c.lenient(String.self, .lastRole, "lucina")
        lastText = c.lenient(String.self, .lastText, "")
        if let ms = c.lenientOptional(Double.self, .lastTs) {
            lastTs = Date(timeIntervalSince1970: ms / 1000)
        } else if let text = c.lenientOptional(String.self, .lastTs) {
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            lastTs = formatter.date(from: text) ?? ISO8601DateFormatter().date(from: text) ?? .distantPast
        } else { lastTs = .distantPast }
    }

    // Cache dates in the same millisecond representation accepted from the wire.
    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(key, forKey: .key)
        try c.encode(kind, forKey: .kind)
        try c.encode(title, forKey: .title)
        try c.encode(channel, forKey: .channel)
        try c.encodeIfPresent(device, forKey: .device)
        try c.encodeIfPresent(deviceName, forKey: .deviceName)
        try c.encodeIfPresent(caller, forKey: .caller)
        try c.encode(count, forKey: .count)
        try c.encode(lastId, forKey: .lastId)
        try c.encode(lastTs.timeIntervalSince1970 * 1000, forKey: .lastTs)
        try c.encode(lastRole, forKey: .lastRole)
        try c.encode(lastText, forKey: .lastText)
    }

    private enum CodingKeys: String, CodingKey {
        case key, kind, title, channel, device, deviceName, caller, count, lastId, lastTs, lastRole, lastText
    }
}

struct ConversationPage: Decodable, Sendable {
    let items: [ChatConversation]
}
