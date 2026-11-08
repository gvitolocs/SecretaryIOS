import Foundation

/// The server's notification rules, applied on the iPhone to what it alerts or
/// schedules itself: category switches, this device's overrides, quiet hours
/// and Do Not Disturb. Mirrors `Settings::allows` / `quiet_at` in
/// `crates/secretary-server/src/notify/settings.rs`.
struct NotificationRules: Sendable {
    let settings: NotificationSettings
    /// This app's device id (`dev_…`): its entry in `settings.devices`.
    let deviceId: String?

    init(settings: NotificationSettings = NotificationSettings(), deviceId: String? = nil) {
        self.settings = settings
        self.deviceId = deviceId
    }

    func allows(_ category: NotificationCategory) -> Bool {
        guard settings.isOn(category) else { return false }
        guard let deviceId, let prefs = settings.devices[deviceId] else { return true }
        return prefs.enabled && (prefs.categories[category.rawValue] ?? true)
    }

    /// `HH:MM` → minutes after midnight.
    static func minutes(_ hhmm: String) -> Int? {
        let parts = hhmm.split(separator: ":")
        guard parts.count == 2, parts[0].count == 2, parts[1].count == 2,
              let h = Int(parts[0]), let m = Int(parts[1]), (0..<24).contains(h), (0..<60).contains(m) else { return nil }
        return h * 60 + m
    }

    func isQuiet(at date: Date) -> Bool {
        if let dnd = settings.dndUntil, Double(dnd) / 1000 > date.timeIntervalSince1970 { return true }
        let q = settings.quietHours
        guard q.enabled, let start = Self.minutes(q.start), let end = Self.minutes(q.end), start != end else { return false }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: settings.timeZone) ?? .current
        let parts = calendar.dateComponents([.hour, .minute], from: date)
        let now = (parts.hour ?? 0) * 60 + (parts.minute ?? 0)
        return start < end ? (now >= start && now < end) : (now >= start || now < end)
    }

    /// Alert now (or at `date`) for `category`?
    func shouldAlert(_ category: NotificationCategory, at date: Date = Date()) -> Bool {
        guard allows(category) else { return false }
        guard isQuiet(at: date) else { return true }
        return category == .reminder && settings.remindersBypassQuiet
    }
}
