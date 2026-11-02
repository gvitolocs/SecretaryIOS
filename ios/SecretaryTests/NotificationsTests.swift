import UserNotifications
import XCTest
@testable import Secretary

/// Records what would go to `UNUserNotificationCenter`.
final class FakeScheduler: NotificationScheduling, @unchecked Sendable {
    var added: [UNNotificationRequest] = []
    var pending: [String] = []
    var removed: [String] = []
    var status: UNAuthorizationStatus = .authorized
    var badge = 0

    func add(_ request: UNNotificationRequest) async throws {
        added.append(request)
        if request.trigger != nil { pending.append(request.identifier) }
    }

    func pendingIdentifiers() async -> [String] { pending }

    func removePending(_ identifiers: [String]) {
        removed.append(contentsOf: identifiers)
        pending.removeAll { identifiers.contains($0) }
    }

    func authorizationStatus() async -> UNAuthorizationStatus { status }

    func requestAuthorization() async throws -> Bool { status = .authorized; return true }

    func setBadge(_ count: Int) async { badge = count }
}

final class NotificationModelTests: XCTestCase {
    func testPageDecodingSkipsUnknownCategoriesAndUnsafeRoutes() throws {
        let page = try Fixtures.decode(NotificationPage.self, """
        {"items": [
          {"id": "n_2", "ts": 1792654200000, "category": "reminder", "body": "Dentist", "route": "//evil.example", "read": false,
           "dedupKey": "reminder:42", "timelineId": "01JA"},
          {"id": "n_1", "ts": 1792654100000, "category": "weather", "body": "?", "route": "/", "read": false},
          {"id": "n_0", "ts": 1792654000000, "category": "shopping", "title": "Casa", "body": "Milk", "route": "/shopping", "read": true}
        ], "unread": 4, "nextBefore": null}
        """)
        XCTAssertEqual(page.items.map(\.id), ["n_2", "n_0"])
        XCTAssertEqual(page.items[0].route, "/")
        XCTAssertEqual(page.items[0].dedupKey, "reminder:42")
        XCTAssertEqual(page.items[1].displayTitle, "Casa")
        XCTAssertEqual(page.unread, 4)
        XCTAssertEqual(page.items[0].date.timeIntervalSince1970, 1_792_654_200, accuracy: 0.001)
    }

    func testSettingsRoundTripKeepsUnknownCategoriesAndNullDnd() throws {
        let state = try Fixtures.decode(NotificationSettingsState.self, """
        {"settings": {"categories": {"lucina_message": false, "future_thing": true}, "quietHours": {"enabled": true, "start": "23:00", "end": "07:00"},
                      "timeZone": "Europe/Rome", "dndUntil": null, "calendarLeadMinutes": 15, "remindersBypassQuiet": true,
                      "digest": false, "previews": true, "language": "it", "devices": {"dev_1": {"enabled": false, "categories": {}}}},
         "subscriptions": [], "currentDevice": {"id": "dev_1", "name": "iPhone"}, "apns": {"enabled": false, "environment": null}}
        """)
        XCTAssertFalse(state.settings.isOn(.lucinaMessage))
        XCTAssertTrue(state.settings.isOn(.reminder), "missing keys take the default")
        XCTAssertFalse(state.settings.isOn(.system))
        XCTAssertEqual(state.currentDevice?.id, "dev_1")
        XCTAssertEqual(state.apns?.enabled, false)
        let json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state.settings)) as! [String: Any]
        XCTAssertTrue(json["dndUntil"] is NSNull, "null turns DND off on the server")
        XCTAssertEqual((json["categories"] as! [String: Bool])["future_thing"], true)
        XCTAssertEqual(json["calendarLeadMinutes"] as? Int, 15)
    }

    func testRoutesMapToTabs() {
        XCTAssertEqual(NotificationRoute.tab(for: "/"), .chat)
        XCTAssertEqual(NotificationRoute.tab(for: "/shopping"), .shopping)
        XCTAssertEqual(NotificationRoute.tab(for: "/gym/history/w-1"), .fitness)
        XCTAssertEqual(NotificationRoute.tab(for: "/weight"), .fitness)
        XCTAssertEqual(NotificationRoute.tab(for: "/memos/mm_1"), .plugins)
        XCTAssertEqual(NotificationRoute.tab(for: "/lucina"), .lucina)
        XCTAssertEqual(NotificationRoute.tab(for: "//evil.example/shopping"), .chat)
        XCTAssertEqual(NotificationRoute.tab(for: "https://evil.example"), .chat)
    }
}

final class NotificationRulesTests: XCTestCase {
    // 2026-10-22T21:00:00Z = 23:00 in Rome.
    let t23 = Date(timeIntervalSince1970: 1_792_702_800)

    func testCategoriesAndDeviceOverrides() {
        var s = NotificationSettings()
        s.set(.shopping, false)
        s.devices["dev_1"] = .init(enabled: true, categories: ["calendar": false])
        s.devices["dev_2"] = .init(enabled: false, categories: [:])
        XCTAssertFalse(NotificationRules(settings: s).allows(.shopping))
        XCTAssertFalse(NotificationRules(settings: s).allows(.presence), "off by default")
        XCTAssertTrue(NotificationRules(settings: s, deviceId: "dev_9").allows(.calendar))
        XCTAssertFalse(NotificationRules(settings: s, deviceId: "dev_1").allows(.calendar))
        XCTAssertTrue(NotificationRules(settings: s, deviceId: "dev_1").allows(.reminder))
        XCTAssertFalse(NotificationRules(settings: s, deviceId: "dev_2").allows(.reminder))
    }

    func testQuietHoursWrapMidnightAndDnd() {
        var s = NotificationSettings()
        XCTAssertFalse(NotificationRules(settings: s).isQuiet(at: t23))
        s.quietHours = .init(enabled: true, start: "22:30", end: "07:30")
        let rules = NotificationRules(settings: s)
        XCTAssertTrue(rules.isQuiet(at: t23))
        XCTAssertTrue(rules.isQuiet(at: t23.addingTimeInterval(8 * 3600)))   // 07:00
        XCTAssertFalse(rules.isQuiet(at: t23.addingTimeInterval(9 * 3600)))  // 08:00
        XCTAssertFalse(rules.shouldAlert(.lucinaMessage, at: t23))
        XCTAssertTrue(rules.shouldAlert(.reminder, at: t23), "reminders still come through")
        s.remindersBypassQuiet = false
        XCTAssertFalse(NotificationRules(settings: s).shouldAlert(.reminder, at: t23))
        var d = NotificationSettings()
        d.dndUntil = Int64((t23.timeIntervalSince1970 + 60) * 1000)
        XCTAssertTrue(NotificationRules(settings: d).isQuiet(at: t23))
        XCTAssertFalse(NotificationRules(settings: d).isQuiet(at: t23.addingTimeInterval(61)))
        XCTAssertEqual(NotificationRules.minutes("07:05"), 425)
        XCTAssertNil(NotificationRules.minutes("7:05"))
    }
}

@MainActor
final class LocalNotifierTests: XCTestCase {
    let now = Date(timeIntervalSince1970: 1_792_654_200) // 2026-10-22 09:30 Rome

    func testRemindersAreScheduledAndStaleOnesRemoved() async {
        let fake = FakeScheduler()
        fake.pending = ["reminder.old", "calendar.x.1", "other"]
        let notifier = LocalNotifier(center: fake)
        let ids = await notifier.schedule(reminders: [
            UpcomingReminder(id: "r2", text: "Later", dueAt: now.addingTimeInterval(7200)),
            UpcomingReminder(id: "r1", text: "Dentist", dueAt: now.addingTimeInterval(600)),
            UpcomingReminder(id: "past", text: "Gone", dueAt: now.addingTimeInterval(-60)),
        ], rules: NotificationRules(), now: now)
        XCTAssertEqual(ids, ["reminder.r1", "reminder.r2"])
        XCTAssertEqual(fake.removed, ["reminder.old"])
        XCTAssertEqual(fake.added.count, 2)
        let first = fake.added[0]
        XCTAssertEqual(first.content.body, "Dentist")
        XCTAssertEqual(first.content.threadIdentifier, "reminder")
        let trigger = first.trigger as? UNCalendarNotificationTrigger
        XCTAssertEqual(trigger.flatMap { Calendar.current.date(from: $0.dateComponents) }, now.addingTimeInterval(600))
    }

    func testRemindersOffOrQuietAreNotScheduled() async {
        let fake = FakeScheduler()
        let notifier = LocalNotifier(center: fake)
        var s = NotificationSettings()
        s.set(.reminder, false)
        let none = await notifier.schedule(reminders: [UpcomingReminder(id: "r", text: "x", dueAt: now.addingTimeInterval(60))],
                                           rules: NotificationRules(settings: s), now: now)
        XCTAssertEqual(none, [])
        var q = NotificationSettings()
        q.quietHours = .init(enabled: true, start: "09:00", end: "12:00")
        q.remindersBypassQuiet = false
        let quiet = await notifier.schedule(reminders: [UpcomingReminder(id: "r", text: "x", dueAt: now.addingTimeInterval(60))],
                                            rules: NotificationRules(settings: q), now: now)
        XCTAssertEqual(quiet, [])
    }

    func testCalendarFiresLeadMinutesBefore() async {
        let fake = FakeScheduler()
        let notifier = LocalNotifier(center: fake)
        var s = NotificationSettings()
        s.calendarLeadMinutes = 15
        let soon = CalendarAlert(id: "E1", title: "Stand-up", start: now.addingTimeInterval(3600))
        let tooSoon = CalendarAlert(id: "E2", title: "Now", start: now.addingTimeInterval(300))
        let ids = await notifier.schedule(events: [soon, tooSoon], rules: NotificationRules(settings: s), now: now)
        XCTAssertEqual(ids, [LocalNotifier.calendarId(soon)])
        let trigger = fake.added.first?.trigger as? UNCalendarNotificationTrigger
        XCTAssertEqual(trigger.flatMap { Calendar.current.date(from: $0.dateComponents) }, now.addingTimeInterval(2700))
        XCTAssertTrue(fake.added.first?.content.body.hasPrefix("Stand-up · ") == true)
    }

    func testInboxPostCarriesRouteAndId() async {
        let fake = FakeScheduler()
        let notifier = LocalNotifier(center: fake)
        await notifier.post(AppNotification(id: "n_1", date: now, category: .shopping, body: "Milk", route: "/shopping"), previews: false)
        let r = fake.added[0]
        XCTAssertEqual(r.identifier, "inbox.n_1")
        XCTAssertNil(r.trigger)
        XCTAssertEqual(r.content.userInfo[LocalNotifier.routeKey] as? String, "/shopping")
        XCTAssertEqual(r.content.userInfo[LocalNotifier.idKey] as? String, "n_1")
        XCTAssertNotEqual(r.content.body, "Milk", "previews off")
    }
}

@MainActor
final class NotificationCenterStoreTests: XCTestCase {
    var defaults: UserDefaults!

    override func setUp() {
        super.setUp()
        defaults = UserDefaults(suiteName: "notif-tests-\(UUID().uuidString)")
        StubURLProtocol.requests = []
    }

    private func page(_ items: [(String, String, Bool)], unread: Int) throws -> NotificationPage {
        let rows = items.map { id, cat, read in
            #"{"id":"\#(id)","ts":\#(Int(Date().timeIntervalSince1970 * 1000)),"category":"\#(cat)","body":"b \#(id)","route":"/","read":\#(read),"timelineId":"T\#(id)"}"#
        }
        return try Fixtures.decode(NotificationPage.self, #"{"items":[\#(rows.joined(separator: ","))],"unread":\#(unread)}"#)
    }

    func testFirstSyncDoesNotAlertTheBacklogThenNewOnesAlertOnce() async throws {
        let fake = FakeScheduler()
        let store = NotificationCenterStore(api: { NotificationsClient(client: StubURLProtocol.makeClient()) },
                                            notifier: LocalNotifier(center: fake), defaults: defaults)
        await store.apply(try page([("n_002", "lucina_message", false), ("n_001", "system", false)], unread: 2), alert: true)
        XCTAssertTrue(fake.added.isEmpty)
        XCTAssertEqual(fake.badge, 2)
        await store.apply(try page([("n_004", "lucina_message", false), ("n_003", "shopping", true), ("n_002", "lucina_message", false)], unread: 2), alert: true)
        XCTAssertEqual(fake.added.map(\.identifier), ["inbox.n_004"])
        await store.apply(try page([("n_004", "lucina_message", false)], unread: 1), alert: true)
        XCTAssertEqual(fake.added.count, 1, "alerted once")
    }

    func testTimelineAlertsWhenChatIsNotInViewAndSkipsTheInboxCopy() async throws {
        let fake = FakeScheduler()
        let store = NotificationCenterStore(api: { NotificationsClient(client: StubURLProtocol.makeClient()) },
                                            notifier: LocalNotifier(center: fake), defaults: defaults)
        let now = Date()
        let lucina = TimelineItem(id: "Tn_006", ts: now, channel: .telegram, role: .lucina, text: "Ciao!")
        let old = TimelineItem(id: "Told", ts: now.addingTimeInterval(-3600), channel: .telegram, role: .lucina, text: "Vecchio")
        let mine = TimelineItem(id: "Tme", ts: now, channel: .ios, role: .user, text: "hi")
        await store.consider(timeline: [lucina, old, mine], chatVisible: false, now: now)
        XCTAssertEqual(fake.added.map(\.identifier), ["tl.Tn_006"])
        await store.consider(timeline: [lucina], chatVisible: false, now: now)
        XCTAssertEqual(fake.added.count, 1)
        let visible = TimelineItem(id: "Tn_007", ts: now, channel: .web, role: .lucina, text: "Visto")
        await store.consider(timeline: [visible], chatVisible: true, now: now)
        XCTAssertEqual(fake.added.count, 1, "the chat is in view")

        await store.apply(try page([("n_005", "system", false)], unread: 1), alert: true) // first sync
        await store.apply(try page([("n_007", "lucina_message", false), ("n_006", "lucina_message", false)], unread: 2), alert: true)
        XCTAssertEqual(fake.added.count, 1, "both were already shown from the stream")
    }

    func testLucinasChatsWithAgentsMakeNoLocalAlert() async throws {
        let fake = FakeScheduler()
        let store = NotificationCenterStore(api: { NotificationsClient(client: StubURLProtocol.makeClient()) },
                                            notifier: LocalNotifier(center: fake), defaults: defaults)
        let now = Date()
        func item(_ id: String, _ meta: TimelineMeta?) -> TimelineItem {
            var i = TimelineItem(id: id, ts: now, channel: .web, role: .lucina, text: "Giuseppe should look at this")
            i.meta = meta
            return i
        }
        var mcp = TimelineMeta()
        mcp.via = "mcp"
        mcp.caller = "some-new-provider"
        var reach = TimelineMeta()
        reach.kind = "reach"
        var reminder = TimelineMeta()
        reminder.kind = "reminder"
        reminder.via = "mcp"
        await store.consider(timeline: [item("Tagent", mcp), item("Treach", reach)], chatVisible: false, now: now)
        XCTAssertTrue(fake.added.isEmpty, "agent chat stays in the chat")
        await store.consider(timeline: [item("Tme", nil), item("Trem", reminder)], chatVisible: false, now: now)
        XCTAssertEqual(fake.added.map(\.identifier), ["tl.Tme", "tl.Trem"], "messages to him still alert")
    }

    func testMarkReadSendsIdsAndTakesTheServerCount() async throws {
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/notifications/read" { return .init(body: #"{"updated":1,"unread":0}"#) }
            return .init(status: 404, body: "{}")
        }
        let fake = FakeScheduler()
        let store = NotificationCenterStore(api: { NotificationsClient(client: StubURLProtocol.makeClient()) },
                                            notifier: LocalNotifier(center: fake), defaults: defaults)
        await store.apply(try page([("n_1", "reminder", false), ("n_0", "reminder", true)], unread: 1), alert: false)
        let tab = await store.open(store.items[0])
        XCTAssertEqual(tab, .chat)
        XCTAssertEqual(store.unread, 0)
        XCTAssertTrue(store.items[0].read)
        let sent = StubURLProtocol.requests.first { $0.url?.path == "/v1/notifications/read" }
        let body = try JSONSerialization.jsonObject(with: sent?.httpBody ?? Data()) as? [String: [String]]
        XCTAssertEqual(body?["ids"], ["n_1"])
        XCTAssertEqual(sent?.value(forHTTPHeaderField: "Authorization"), "Bearer sec_test")
        XCTAssertEqual(fake.badge, 0)
    }
}
