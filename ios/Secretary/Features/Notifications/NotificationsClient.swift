import Foundation

/// The notification routes of secretary-server (Bearer device token).
struct NotificationsClient: Sendable {
    let client: APIClient

    func page(limit: Int = 50) async throws -> NotificationPage {
        try await client.get("/v1/notifications", query: [URLQueryItem(name: "limit", value: String(limit))])
    }

    func markRead(_ ids: [String]) async throws -> ReadResult {
        struct Body: Encodable { let ids: [String] }
        return try await client.send("POST", "/v1/notifications/read", body: Body(ids: ids), idempotent: true)
    }

    func markAllRead() async throws -> ReadResult {
        struct Body: Encodable { let all = true }
        return try await client.send("POST", "/v1/notifications/read", body: Body(), idempotent: true)
    }

    func settings() async throws -> NotificationSettingsState {
        try await client.get("/v1/notifications/settings")
    }

    func save(_ settings: NotificationSettings) async throws -> NotificationSettingsState {
        try await client.send("PUT", "/v1/notifications/settings", body: settings)
    }

    /// The app is (or is no longer) in front: the server then holds Lucina's
    /// messages back from the other devices.
    func presence(active: Bool) async throws {
        struct Body: Encodable { let active: Bool }
        struct Answer: Decodable { let present: Bool? }
        let _: Answer = try await client.send("POST", "/v1/notifications/presence", body: Body(active: active), timeout: 8, idempotent: true)
    }

    /// Lucina's pending reminders, to schedule as local notifications.
    func upcomingReminders() async throws -> [UpcomingReminder] {
        let list: ReminderList = try await client.get("/v1/reminders", query: [URLQueryItem(name: "scope", value: "upcoming")])
        return list.reminders.filter { $0.status == "pending" }
    }

    /// APNs device token (only when `APNsAdapter.isEnabled`; the server answers
    /// 503 `apns_disabled` without a paid team's key).
    func registerAPNs(token: String, lang: String) async throws {
        struct Body: Encodable { let token: String; let lang: String }
        let _: PushTarget = try await client.send("POST", "/v1/push/apns", body: Body(token: token, lang: lang), idempotent: true)
    }
}
