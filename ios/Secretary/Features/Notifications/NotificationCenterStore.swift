import Foundation
import Observation
import UserNotifications

/// The in-app notification center (synced with the server inbox), its
/// settings, and the local notifications this iPhone shows itself.
///
/// Without APNs (free Personal Team) the app cannot be woken by the server, so:
/// - while it runs (and on background refreshes) new inbox entries become local
///   notifications, once each (`lastAlerted`), never the backlog of a first sync;
/// - Lucina's timeline items alert at once when the chat is not in view;
/// - Lucina's reminders and calendar events are scheduled ahead, so they fire
///   even when the app is closed. Inbox entries for those are then skipped.
@Observable
@MainActor
final class NotificationCenterStore {
    private(set) var items: [AppNotification] = []
    private(set) var unread = 0
    private(set) var state: NotificationSettingsState?
    private(set) var authorization: UNAuthorizationStatus = .notDetermined
    private(set) var loaded = false
    var errorMessage: String?

    @ObservationIgnored private let api: () -> NotificationsClient
    @ObservationIgnored let notifier: LocalNotifier
    @ObservationIgnored private let defaults: UserDefaults
    /// Upcoming timed calendar events (EventKit, when calendar access is on).
    @ObservationIgnored var calendarEvents: () async -> [CalendarAlert] = { [] }
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    /// Timeline ids alerted from the stream (their inbox entries are skipped).
    @ObservationIgnored private var alertedTimeline: [String] = []

    static let lastAlertedKey = "secretary.notifications.lastAlerted"
    static let settingsCacheKey = "secretary.notifications.settings"
    static let scheduledKey = "secretary.notifications.scheduledReminders"
    static let presenceInterval: Duration = .seconds(45)
    /// Timeline items older than this are history, not news.
    static let freshness: TimeInterval = 120

    init(api: @escaping () -> NotificationsClient, notifier: LocalNotifier = LocalNotifier(), defaults: UserDefaults = .standard) {
        self.api = api
        self.notifier = notifier
        self.defaults = defaults
    }

    // MARK: - Rules

    private var cachedState: NotificationSettingsState? {
        guard let data = defaults.data(forKey: Self.settingsCacheKey) else { return nil }
        struct Cache: Codable { let settings: NotificationSettings; let deviceId: String? }
        guard let cache = try? JSONDecoder().decode(Cache.self, from: data) else { return nil }
        return NotificationSettingsState(settings: cache.settings, currentDevice: cache.deviceId.map { .init(id: $0, name: "") })
    }

    private func cache(_ s: NotificationSettingsState) {
        struct Cache: Codable { let settings: NotificationSettings; let deviceId: String? }
        if let data = try? JSONEncoder().encode(Cache(settings: s.settings, deviceId: s.currentDevice?.id)) {
            defaults.set(data, forKey: Self.settingsCacheKey)
        }
    }

    /// The rules in force (the last settings seen, also offline and in background refreshes).
    var rules: NotificationRules {
        let s = state ?? cachedState
        return NotificationRules(settings: s?.settings ?? NotificationSettings(), deviceId: s?.currentDevice?.id)
    }

    // MARK: - Inbox

    func refresh(alert: Bool = true) async {
        do {
            let page = try await api().page()
            await apply(page, alert: alert)
            errorMessage = nil
        } catch {
            let e = APIError.wrap(error)
            errorMessage = e.userMessage
            onTransportError?(e)
        }
        loaded = true
    }

    /// Show the page; alert the entries that are new since the last alert.
    func apply(_ page: NotificationPage, alert: Bool) async {
        items = page.items
        unread = page.unread
        await notifier.center.setBadge(unread)
        guard let newest = page.items.map(\.id).max() else { return }
        let last = defaults.string(forKey: Self.lastAlertedKey)
        if newest > (last ?? "") { defaults.set(newest, forKey: Self.lastAlertedKey) }
        // A first sync shows the backlog in the list, without a burst of alerts.
        guard alert, let last else { return }
        let fresh = page.items.filter { !$0.read && $0.id > last }.sorted { $0.id < $1.id }
        let previews = rules.settings.previews
        for n in fresh where shouldPost(n) {
            await notifier.post(n, previews: previews)
        }
    }

    /// Reminder ids scheduled locally → due time (epoch seconds).
    private var scheduledReminders: [String: Double] {
        get { (defaults.dictionary(forKey: Self.scheduledKey) as? [String: Double]) ?? [:] }
        set { defaults.set(newValue, forKey: Self.scheduledKey) }
    }

    func shouldPost(_ n: AppNotification, now: Date = Date()) -> Bool {
        guard rules.shouldAlert(n.category, at: now) else { return false }
        if let t = n.timelineId, alertedTimeline.contains(t) { return false }
        switch n.category {
        case .reminder:
            // Already fired from the local schedule?
            let scheduled = scheduledReminders
            if let key = n.dedupKey, key.hasPrefix("reminder:"), scheduled[String(key.dropFirst("reminder:".count))] != nil { return false }
            let at = n.date.timeIntervalSince1970
            if scheduled.values.contains(where: { abs($0 - at) < 180 }) { return false }
        case .calendar:
            if let key = n.dedupKey, key.hasPrefix("cal:"), rules.allows(.calendar) {
                let parts = key.split(separator: ":")
                if parts.count == 3, let start = Double(parts[2]), start / 1000 > now.timeIntervalSince1970 - 3600 { return false }
            }
        default:
            break
        }
        return true
    }

    /// Timeline items from the live stream: Lucina's new ones alert when the chat is not in view.
    func consider(timeline: [TimelineItem], chatVisible: Bool, now: Date = Date()) async {
        guard !chatVisible else {
            alertedTimeline.append(contentsOf: timeline.map(\.id))
            trimAlerted()
            return
        }
        let rules = rules
        for item in timeline where item.role == .lucina && !item.backfilled && !item.isLocal
            && now.timeIntervalSince(item.ts) < Self.freshness && !alertedTimeline.contains(item.id) {
            alertedTimeline.append(item.id)
            if item.meta?.isDeviceCommand == true { continue } // the confirmation sheet handles these
            let category: NotificationCategory = item.meta?.isReminder == true ? .reminder : .lucinaMessage
            guard rules.shouldAlert(category, at: now), !item.text.isEmpty else { continue }
            await notifier.post(timelineId: item.id, category: category, text: item.text, previews: rules.settings.previews)
        }
        trimAlerted()
    }

    private func trimAlerted() {
        if alertedTimeline.count > 300 { alertedTimeline.removeFirst(alertedTimeline.count - 300) }
    }

    func markRead(_ ids: [String]) async {
        let fresh = ids.filter { id in items.contains { $0.id == id && !$0.read } }
        guard !fresh.isEmpty else { return }
        for i in items.indices where fresh.contains(items[i].id) { items[i].read = true }
        unread = max(0, unread - fresh.count)
        await notifier.center.setBadge(unread)
        if let r = try? await api().markRead(fresh) {
            unread = r.unread
            await notifier.center.setBadge(unread)
        }
    }

    func markAllRead() async {
        for i in items.indices { items[i].read = true }
        unread = 0
        await notifier.center.setBadge(0)
        if let r = try? await api().markAllRead() { unread = r.unread }
    }

    /// Tapped in the list: mark it read; the tab to show.
    func open(_ n: AppNotification) async -> AppModel.Tab {
        await markRead([n.id])
        return NotificationRoute.tab(for: n.route)
    }

    /// Tapped in Notification Center (`userInfo` of a local notification).
    func openFromSystem(userInfo: [AnyHashable: Any]) async -> AppModel.Tab {
        if let id = userInfo[LocalNotifier.idKey] as? String {
            if items.isEmpty { await refresh(alert: false) }
            await markRead([id])
        }
        return NotificationRoute.tab(for: userInfo[LocalNotifier.routeKey] as? String ?? "/")
    }

    // MARK: - Settings

    func loadSettings() async {
        do {
            let s = try await api().settings()
            state = s
            cache(s)
        } catch {
            errorMessage = APIError.wrap(error).userMessage
        }
    }

    /// Optimistic save; the server's answer wins. Returns an error message.
    @discardableResult
    func update(_ change: (inout NotificationSettings) -> Void) async -> String? {
        guard var next = state else { return nil }
        let before = next
        change(&next.settings)
        next.settings.language = AppLanguage.effectiveCode
        state = next
        do {
            let saved = try await api().save(next.settings)
            state = saved
            cache(saved)
            await reschedule()
            return nil
        } catch {
            state = before
            return APIError.wrap(error).userMessage
        }
    }

    /// This iPhone's own switch (`devices[<device id>].enabled`).
    var thisDeviceEnabled: Bool {
        guard let id = state?.currentDevice?.id else { return true }
        return state?.settings.devices[id]?.enabled ?? true
    }

    func setThisDevice(enabled: Bool) async -> String? {
        guard let id = state?.currentDevice?.id else { return nil }
        return await update { s in
            var prefs = s.devices[id] ?? NotificationSettings.DevicePrefs()
            prefs.enabled = enabled
            s.devices[id] = prefs
        }
    }

    // MARK: - Permission, scheduling, presence

    func refreshAuthorization() async {
        authorization = await notifier.center.authorizationStatus()
    }

    /// From a button tap only.
    func requestAuthorization() async {
        _ = try? await notifier.center.requestAuthorization()
        await refreshAuthorization()
        if authorization == .authorized || authorization == .provisional {
            APNsAdapter.registerIfEnabled()
            await reschedule()
        }
    }

    /// Schedule Lucina's reminders and the calendar's events as local notifications.
    func reschedule(now: Date = Date()) async {
        let status = await notifier.center.authorizationStatus()
        authorization = status
        guard status == .authorized || status == .provisional else { return }
        let rules = rules
        if let reminders = try? await api().upcomingReminders() {
            let ids = await notifier.schedule(reminders: reminders, rules: rules, now: now)
            var map: [String: Double] = [:]
            for r in reminders where ids.contains("reminder.\(r.id)") { map[r.id] = r.dueAt.timeIntervalSince1970 }
            // Keep the ones that already fired in the last day (for inbox dedup).
            for (id, due) in scheduledReminders where due < now.timeIntervalSince1970 && due > now.timeIntervalSince1970 - 86_400 {
                map[id] = due
            }
            scheduledReminders = map
        }
        await notifier.schedule(events: await calendarEvents(), rules: rules, now: now)
    }

    /// Tells the server the app is in front, every 45 s, until cancelled.
    func presenceLoop() async {
        while !Task.isCancelled {
            try? await api().presence(active: true)
            try? await Task.sleep(for: Self.presenceInterval)
        }
        let api = api
        Task.detached { try? await api().presence(active: false) }
    }

    /// `BGAppRefreshTask`: sync the inbox (alerting new entries) and the schedules.
    func backgroundRefresh() async {
        await refresh(alert: true)
        await reschedule()
        BackgroundRefresh.schedule()
    }

    func reset() {
        items = []
        unread = 0
        state = nil
        loaded = false
        errorMessage = nil
        alertedTimeline = []
        defaults.removeObject(forKey: Self.lastAlertedKey)
        defaults.removeObject(forKey: Self.settingsCacheKey)
        defaults.removeObject(forKey: Self.scheduledKey)
        Task { await notifier.removeAll() }
    }
}

extension NotificationCenterStore {
    /// While the app is in front: presence every 45 s and an inbox sync with it.
    func foregroundLoop() async {
        await refreshAuthorization()
        await loadSettings()
        await reschedule()
        await withTaskGroup(of: Void.self) { group in
            group.addTask { await self.presenceLoop() }
            group.addTask {
                while !Task.isCancelled {
                    await self.refresh(alert: true)
                    try? await Task.sleep(for: Self.presenceInterval)
                }
            }
        }
    }
}
