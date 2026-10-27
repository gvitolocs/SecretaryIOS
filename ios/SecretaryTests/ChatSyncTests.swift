import XCTest
@testable import Secretary

final class ConversationRoutingTests: XCTestCase {
    private func item(_ channel: TimelineChannel, device: String? = nil, via: String? = nil, caller: String? = nil) -> TimelineItem {
        var meta = TimelineMeta()
        meta.device = device
        meta.via = via
        meta.caller = caller
        return TimelineItem(id: "01", ts: Date(), channel: channel, role: .user, text: "hello", meta: meta)
    }

    func testConversationRuleAndPrecedence() {
        XCTAssertEqual(conversationKey(item(.ios, device: "D")), "device:D")
        XCTAssertEqual(conversationKey(item(.web, device: "D")), "device:D")
        XCTAssertEqual(conversationKey(item(.ios)), "legacy:ios")
        XCTAssertEqual(conversationKey(item(.web)), "legacy:web")
        for channel in [TimelineChannel.telegram, .discord, .alexa] {
            XCTAssertEqual(conversationKey(item(channel, device: "D")), channel.rawValue)
        }
        XCTAssertEqual(conversationKey(item(.other)), "lucina")
        XCTAssertEqual(conversationKey(item(.ios, device: "D", via: "mcp", caller: " CoDeX /_!ü-42")), "agent:codex_-42")
        XCTAssertEqual(conversationKey(item(.web, via: "mcp", caller: " !")), "agent:mcp")
        XCTAssertEqual(conversationKey(item(.other, via: "mcp")), "agent:mcp")
    }

    func testMineAndUnknownDevice() {
        XCTAssertTrue(isMine(item(.ios), deviceId: nil))
        XCTAssertTrue(isMine(item(.other), deviceId: "D"))
        XCTAssertTrue(isMine(item(.web, device: "D"), deviceId: "D"))
        XCTAssertFalse(isMine(item(.ios, device: "other"), deviceId: "D"))
        XCTAssertFalse(isMine(item(.telegram), deviceId: "D"))
        XCTAssertFalse(isMine(item(.other, device: "D"), deviceId: nil))
        XCTAssertFalse(isMine(item(.web), deviceId: "D"))
        XCTAssertFalse(isMine(item(.ios, device: "D", via: "mcp"), deviceId: "D"))
        var unknown = item(.ios, device: "D")
        XCTAssertFalse(isMine(unknown, deviceId: nil))
        unknown.clientMessageId = "sent"
        XCTAssertTrue(isMine(unknown, deviceId: nil, sentClientIDs: ["sent"]))
        XCTAssertFalse(isMine(unknown, deviceId: "other", sentClientIDs: ["sent"]))
    }

    func testConversationAndDeviceMetadataDecoding() throws {
        let page = try Fixtures.decode(ConversationPage.self, """
        {"items":[{"key":"device:D","kind":"device","title":"My phone","channel":"ios","device":"D",
        "deviceName":"iPhone","count":12,"lastId":42,"lastTs":"2026-10-21T16:30:00.123Z","lastRole":"user","lastText":"Hi"}]}
        """)
        let conversation = try XCTUnwrap(page.items.first)
        XCTAssertEqual(conversation.device, "D")
        XCTAssertEqual(conversation.count, 12)
        XCTAssertEqual(conversation.lastId, "42")
        XCTAssertEqual(conversation.lastTs.timeIntervalSince1970, 1792600200.123, accuracy: 0.001)
        XCTAssertEqual(try JSONDecoder().decode(ChatConversation.self, from: JSONEncoder().encode(conversation)), conversation)
        let item = try Fixtures.decode(TimelineItem.self, #"{"id":"1","channel":"ios","role":"user","meta":{"device":"D","deviceName":"iPhone"}}"#)
        XCTAssertEqual(item.meta?.device, "D")
        XCTAssertEqual(item.meta?.deviceName, "iPhone")
    }
}

@MainActor
final class ChatDatabaseTests: XCTestCase {
    private var directory: URL!
    override func setUp() {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    }
    override func tearDown() { try? FileManager.default.removeItem(at: directory) }
    private func item(_ id: String, channel: TimelineChannel = .ios) -> TimelineItem {
        TimelineItem(id: id, ts: Date(timeIntervalSince1970: Double(id) ?? 0), channel: channel, role: .user, text: "text " + id)
    }

    func testRoundTripScopesCursorAndReset() async throws {
        let db = ChatDatabase(url: directory.appendingPathComponent("chat.sqlite"))
        var device = item("04")
        device.meta = TimelineMeta()
        device.meta?.device = "D"
        let originals = [item("01"), item("02", channel: .telegram), item("03", channel: .other), device]
        try await db.write(originals, state: ["mine": "cursor", "conversation:telegram": "telegram-cursor"])
        let mine = try await db.read(.mine("D"))
        XCTAssertEqual(mine, [originals[0], originals[2], device])
        let unknown = try await db.read(.mine(nil))
        XCTAssertEqual(unknown.map(\.id), ["01", "03"])
        let telegram = try await db.read(.conversation("telegram"))
        XCTAssertEqual(telegram.map(\.id), ["02"])
        let older = try await db.read(.mine("D"), before: "04", limit: 1)
        XCTAssertEqual(older.map(\.id), ["03"])
        let cursor = try await db.value("mine")
        XCTAssertEqual(cursor, "cursor")
        try await db.write([item("05")], state: ["mine": "new"], reset: .mine("D"))
        let reset = try await db.read(.mine("D"))
        XCTAssertEqual(reset.map(\.id), ["05"])
        let unaffected = try await db.read(.conversation("telegram"))
        XCTAssertEqual(unaffected.map(\.id), ["02"])
        let otherCursor = try await db.value("conversation:telegram")
        XCTAssertEqual(otherCursor, "telegram-cursor")
        try await db.clear()
        let empty = try await db.read(.mine("D"))
        let clearedCursor = try await db.value("mine")
        XCTAssertTrue(empty.isEmpty)
        XCTAssertNil(clearedCursor)
    }

    func testCapDropsOldestAndUpserts() async throws {
        let db = ChatDatabase(url: directory.appendingPathComponent("chat.sqlite"), cap: 3)
        try await db.write((1...5).map { item(String(format: "%02d", $0)) })
        var updated = item("05")
        updated.text = "edited"
        try await db.write([updated])
        let page = try await db.read(.mine(nil))
        XCTAssertEqual(page.map(\.id), ["03", "04", "05"])
        XCTAssertEqual(page.last?.text, "edited")
    }

    func testFirstPaintMigrationAndMergeWithoutNetwork() async throws {
        let url = directory.appendingPathComponent("timeline.json")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let cache = ChatStore.Cache(items: [item("01")], jobs: [:], lastEventId: "event", localOnly: false)
        try JSONEncoder().encode(cache).write(to: url)
        let db = ChatDatabase(url: directory.appendingPathComponent("chat.sqlite"))
        let client = StubURLProtocol.makeClient()
        StubURLProtocol.requests = []
        let defaults = UserDefaults(suiteName: UUID().uuidString)!
        let store = ChatStore(api: { SecretaryAPI(client: client) }, fileURL: url, defaults: defaults, database: db)
        await store.prepare()
        XCTAssertEqual(store.items.map(\.id), ["01"])
        XCTAssertTrue(StubURLProtocol.requests.isEmpty)
        XCTAssertFalse(FileManager.default.fileExists(atPath: url.path))
        StubURLProtocol.handler = { _ in .init(body: #"{"items":[{"id":"02","ts":2000,"channel":"ios","role":"lucina","text":"new"}],"cursor":"c2"}"#) }
        await store.refresh()
        XCTAssertEqual(store.items.map(\.id), ["01", "02"], "the first server page preserves the cached bubble")
        await store.flushPersistence()
        let restored = ChatStore(api: { SecretaryAPI(client: client) }, fileURL: url, defaults: defaults, database: db)
        let requests = StubURLProtocol.requests.count
        await restored.prepare()
        XCTAssertEqual(restored.items.map(\.id), ["01", "02"])
        XCTAssertEqual(StubURLProtocol.requests.count, requests)
        store.clear()
        await store.flushPersistence()
        let empty = try await db.read(.mine(nil))
        XCTAssertTrue(empty.isEmpty)
    }

    func testLiveStoresOtherDevicesAndCallsBridge() async throws {
        let db = ChatDatabase(url: directory.appendingPathComponent("chat.sqlite"))
        let store = ChatStore(api: { SecretaryAPI(client: StubURLProtocol.makeClient()) },
                              fileURL: directory.appendingPathComponent("timeline.json"),
                              defaults: UserDefaults(suiteName: UUID().uuidString)!, database: db)
        await store.prepare()
        store.setDeviceId("D")
        var bridged = 0
        store.onItems = { bridged += $0.count }
        for (id, device) in [("01", "D"), ("02", "other")] {
            store.handle(SSEEvent(event: "item", data: """
            {"id":"\(id)","ts":1000,"channel":"ios","role":"user","text":"live","meta":{"device":"\(device)"}}
            """, id: id))
        }
        XCTAssertEqual(store.items.map(\.id), ["01"])
        XCTAssertEqual(bridged, 2)
        await store.flushPersistence()
        let other = try await db.read(.conversation("device:other"))
        XCTAssertEqual(other.map(\.id), ["02"])
    }
}

@MainActor
final class ChatCursorSyncTests: XCTestCase {
    override func setUp() { StubURLProtocol.requests = []; StubURLProtocol.handler = nil }

    func testCursorLoopAndQuery() async throws {
        StubURLProtocol.handler = { request in
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            XCTAssertEqual(query.first { $0.name == "scope" }?.value, "mine")
            XCTAssertEqual(query.first { $0.name == "limit" }?.value, "200")
            let after = query.first { $0.name == "after" }?.value
            XCTAssertTrue(after == "c0" || after == "c1")
            return .init(body: after == "c0" ? #"{"items":[],"cursor":"c1","hasMore":true}"# : #"{"items":[],"cursor":"c2","hasMore":false}"#)
        }
        var cursors: [String] = []
        try await ChatSync.run(api: SecretaryAPI(client: StubURLProtocol.makeClient()), cursor: "c0") { page, reset in
            XCTAssertFalse(reset)
            cursors.append(page.cursor ?? "")
        }
        XCTAssertEqual(cursors, ["c1", "c2"])
        XCTAssertEqual(StubURLProtocol.requests.count, 2)
    }

    func testTenCallCapThenFreshWindow() async throws {
        var calls = 0
        StubURLProtocol.handler = { request in
            calls += 1
            if calls <= 10 {
                XCTAssertTrue(request.url!.query!.contains("after="))
                return .init(body: "{\"items\":[],\"cursor\":\"c\(calls)\",\"hasMore\":true}")
            }
            XCTAssertFalse(request.url!.query!.contains("after="))
            XCTAssertTrue(request.url!.query!.contains("limit=50"))
            return .init(body: #"{"items":[],"cursor":"fresh"}"#)
        }
        var resets: [Bool] = []
        try await ChatSync.run(api: SecretaryAPI(client: StubURLProtocol.makeClient()), cursor: "start") { _, reset in resets.append(reset) }
        XCTAssertEqual(calls, 11)
        XCTAssertEqual(resets, Array(repeating: false, count: 10) + [true])
    }

    func testExpiredCursorAndConversationQuery() async throws {
        StubURLProtocol.handler = { request in
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems!
            XCTAssertEqual(query.first { $0.name == "conversation" }?.value, "device:other")
            XCTAssertNil(query.first { $0.name == "scope" })
            if query.contains(where: { $0.name == "after" }) {
                return .init(status: 410, body: #"{"error":{"code":"cursor_expired","message":"Expired"}}"#)
            }
            return .init(body: #"{"items":[],"cursor":"fresh"}"#)
        }
        var resets: [Bool] = []
        try await ChatSync.run(api: SecretaryAPI(client: StubURLProtocol.makeClient()), cursor: "old", conversation: "device:other") { page, reset in
            XCTAssertEqual(page.cursor, "fresh")
            resets.append(reset)
        }
        XCTAssertEqual(resets, [true])
        XCTAssertEqual(StubURLProtocol.requests.count, 2)
    }

    func testWithoutCursorFetchesOneSmallPage() async throws {
        StubURLProtocol.handler = { request in
            XCTAssertTrue(request.url!.query!.contains("limit=50"))
            XCTAssertFalse(request.url!.query!.contains("after="))
            return .init(body: #"{"items":[],"cursor":"first"}"#)
        }
        try await ChatSync.run(api: SecretaryAPI(client: StubURLProtocol.makeClient()), cursor: nil) { page, reset in
            XCTAssertEqual(page.cursor, "first")
            XCTAssertFalse(reset)
        }
        XCTAssertEqual(StubURLProtocol.requests.count, 1)
    }
}
