import XCTest
@testable import Secretary

final class TimelineMergeTests: XCTestCase {
    private func item(_ id: String, _ seconds: TimeInterval, _ role: TimelineItem.Role = .user, channel: TimelineChannel = .telegram,
                      text: String = "x", client: String? = nil, replyTo: String? = nil, local: TimelineItem.LocalStatus? = nil) -> TimelineItem {
        TimelineItem(id: id, ts: Date(timeIntervalSince1970: seconds), channel: channel, role: role, text: text,
                     clientMessageId: client, replyTo: replyTo, localStatus: local)
    }

    func testDedupeByIdAndOrderByTime() {
        let current = [item("a", 10), item("b", 20, .lucina)]
        let incoming = [item("c", 15), item("b", 20, .lucina, text: "edited"), item("a", 10)]
        let result = TimelineMerge.merge(current, incoming)
        XCTAssertEqual(result.items.map(\.id), ["a", "c", "b"])
        XCTAssertEqual(result.items.last?.text, "edited", "the incoming copy wins")
        XCTAssertTrue(result.replaced.isEmpty)
    }

    func testUserBeforeLucinaOnTheSameTimestamp() {
        let result = TimelineMerge.merge([], [item("z-lucina", 5, .lucina), item("a-user", 5, .user)])
        XCTAssertEqual(result.items.map(\.role), [.user, .lucina])
    }

    func testServerCopyReplacesTheOptimisticBubbleByClientMessageId() {
        let optimistic = item("local-c1", 100, channel: .ios, client: "c1", local: .sending)
        let server = item("tl_10", 101, channel: .ios, client: "c1")
        let result = TimelineMerge.merge([optimistic], [server])
        XCTAssertEqual(result.items.map(\.id), ["tl_10"])
        XCTAssertNil(result.items.first?.localStatus)
        XCTAssertEqual(result.replaced, ["local-c1": "tl_10"])
    }

    func testLocalReplyIsReconciledAfterItsUserItemInTheSameBatch() {
        // An older server gave no item ids: both bubbles are local placeholders.
        let user = item("local-c1", 100, channel: .ios, client: "c1", local: .sent)
        let reply = item("local-reply-c1", 100.001, .lucina, channel: .ios, replyTo: "local-c1", local: .sent)
        // The stream sends Lucina's item (no clientMessageId) before the user's.
        let serverReply = item("tl_11", 101, .lucina, channel: .ios, replyTo: "tl_10")
        let serverUser = item("tl_10", 100.5, channel: .ios, client: "c1")
        let result = TimelineMerge.merge([user, reply], [serverReply, serverUser])
        XCTAssertEqual(result.items.map(\.id), ["tl_10", "tl_11"])
        XCTAssertEqual(result.replaced["local-c1"], "tl_10")
        XCTAssertEqual(result.replaced["local-reply-c1"], "tl_11")
    }

    func testOtherChannelsWithTheSameTextAreNotMerged() {
        let mine = item("local-c1", 100, channel: .ios, text: "milk", client: "c1", local: .sending)
        let telegram = item("tl_5", 100, channel: .telegram, text: "milk")
        let result = TimelineMerge.merge([mine], [telegram])
        XCTAssertEqual(Set(result.items.map(\.id)), ["local-c1", "tl_5"])
    }

    func testProactiveItemDecoding() throws {
        let decoder = JSONDecoder()
        let item = try decoder.decode(TimelineItem.self, from: Data("""
        {"id":"01K85Z3Q4X5Y6Z7A8B9C0D1E2G","ts":"2026-10-22T11:50:00.000Z","channel":"other","role":"lucina",
         "text":"Tra un'ora hai CRAFT-IT4SD.",
         "meta":{"kind":"proactive","topic":"meeting_prep","priority":"high","outreachId":"po_01K85Z3Q4X5Y6Z7A8B9C0D1E2F",
                 "why":"starts in 60 min","actions":[{"id":"open_note","label":"Apri la nota","route":"/notes/x"},
                 {"id":"useful","label":"Utile","feedback":true},{"id":"not_now","label":"Non ora","feedback":true},
                 {"id":"stop","label":"Basta così","feedback":true}]}}
        """.utf8))
        XCTAssertTrue(item.meta?.isProactive == true)
        XCTAssertEqual(item.meta?.priority, "high")
        XCTAssertEqual(item.meta?.actions?.filter(\.isFeedback).map(\.id), ["useful", "not_now", "stop"])
        XCTAssertEqual(item.meta?.actions?.first?.route, "/notes/x")

        // A bad outreach id is not a proactive card.
        let bad = try decoder.decode(TimelineItem.self, from: Data(#"{"id":"b","ts":1,"channel":"other","role":"lucina","text":"x","meta":{"kind":"proactive","outreachId":"po_nope"}}"#.utf8))
        XCTAssertFalse(bad.meta?.isProactive == true)
    }

    func testItemDecoding() throws {
        let decoder = JSONDecoder()
        let full = try decoder.decode(TimelineItem.self, from: Data("""
        {"id":"tl_1","ts":"2026-10-21T16:30:00.123Z","channel":"discord","role":"user","text":"hi",
         "clientMessageId":null,"replyTo":7,"attachments":[{"kind":"image","name":"a.png"},{"kind":"audio"}],
         "meta":{"guildName":"Home","channelName":"general","isDm":false},"backfilled":true,"extra":1}
        """.utf8))
        XCTAssertEqual(full.channel, .discord)
        XCTAssertEqual(full.replyTo, "7")
        XCTAssertEqual(full.attachments, [TimelineAttachment(kind: "image", name: "a.png"), TimelineAttachment(kind: "audio", name: nil)])
        XCTAssertEqual(full.meta?.guildName, "Home")
        XCTAssertTrue(full.backfilled)
        XCTAssertEqual(full.ts.timeIntervalSince1970, 1792600200.123, accuracy: 0.001)

        let minimal = try decoder.decode(TimelineItem.self, from: Data(#"{"id":2,"ts":1792600200000,"channel":"carrier-pigeon","role":"lucina","text":"x","meta":{"kind":"reminder"}}"#.utf8))
        XCTAssertEqual(minimal.id, "2")
        XCTAssertEqual(minimal.channel, .other)
        XCTAssertTrue(minimal.meta?.isReminder == true)
        XCTAssertEqual(minimal.ts.timeIntervalSince1970, 1792600200, accuracy: 0.001)

        let page = try decoder.decode(TimelinePage.self, from: Data(#"{"items":[{"id":"a","ts":"2026-10-21T16:30:00Z","channel":"web","role":"user","text":"t"}],"nextBefore":null}"#.utf8))
        XCTAssertEqual(page.items.count, 1)
        XCTAssertNil(page.nextBefore)

        // Items with another role, or that don't decode, are skipped; the page survives.
        let mixed = try decoder.decode(TimelinePage.self, from: Data("""
        {"items":[{"id":"a","ts":1,"channel":"web","role":"system","text":"x"},{"ts":2},
                  {"id":"b","ts":3,"channel":"web","role":"user","text":"kept"}],"nextBefore":"a"}
        """.utf8))
        XCTAssertEqual(mixed.items.map(\.id), ["b"])
        XCTAssertEqual(mixed.nextBefore, "a")

        // The cache round-trips local state.
        var local = minimal
        local.localStatus = .error
        local.localError = "Not delivered."
        let back = try decoder.decode(TimelineItem.self, from: JSONEncoder().encode(local))
        XCTAssertEqual(back, local)
    }

    func testChannelBadges() {
        XCTAssertEqual(TimelineChannel.web.symbol, "globe")
        XCTAssertEqual(TimelineChannel.ios.symbol, "iphone")
        XCTAssertEqual(TimelineChannel.telegram.symbol, "paperplane")
        XCTAssertEqual(TimelineChannel.alexa.symbol, "homepod")
        XCTAssertEqual(TimelineChannel(raw: "DISCORD"), .discord)
        XCTAssertEqual(TimelineChannel.discord.label, "Discord")
    }
}

final class SSEParserTests: XCTestCase {
    private func parse(_ text: String) -> (events: [SSEEvent], lastId: String?) {
        var parser = SSEParser()
        var events: [SSEEvent] = []
        var buffer = Array(text.utf8)
        for line in SSEParser.lines(from: &buffer) {
            if let event = parser.feed(line) { events.append(event) }
        }
        return (events, parser.lastEventId)
    }

    func testItemEventsPingsAndComments() {
        let (events, lastId) = parse("""
        retry: 3000

        : a comment
        id: tl_1
        event: item
        data: {"id":"tl_1"}

        event: ping
        data: {}

        id: tl_2
        event: item
        data: {"id":
        data: "tl_2"}


        """)
        XCTAssertEqual(events.map(\.event), ["message", "item", "ping", "item"])
        XCTAssertEqual(events[0].retry, 3000)
        XCTAssertEqual(events[1], SSEEvent(event: "item", data: #"{"id":"tl_1"}"#, id: "tl_1", retry: nil))
        XCTAssertEqual(events[3].data, "{\"id\":\n\"tl_2\"}", "multi-line data joins with newlines")
        XCTAssertEqual(lastId, "tl_2")
    }

    func testCRLFAndNoSpaceAfterColon() {
        let (events, _) = parse("event:item\r\ndata:hello\r\n\r\n")
        XCTAssertEqual(events, [SSEEvent(event: "item", data: "hello", id: nil, retry: nil)])
    }

    func testLinesSurviveChunkBoundaries() {
        var parser = SSEParser()
        var buffer: [UInt8] = []
        var events: [SSEEvent] = []
        for chunk in ["id: 1\r", "\nevent: it", "em\ndata: {\"a\"", ":1}\n", "\n"] {
            buffer += Array(chunk.utf8)
            for line in SSEParser.lines(from: &buffer) {
                if let event = parser.feed(line) { events.append(event) }
            }
        }
        XCTAssertEqual(events, [SSEEvent(event: "item", data: #"{"a":1}"#, id: "1", retry: nil)])
        XCTAssertTrue(buffer.isEmpty)
    }

    func testBlankLinesAloneDispatchNothing() {
        XCTAssertTrue(parse("\n\n\n").events.isEmpty)
        XCTAssertTrue(parse("event: item\n\n").events.isEmpty, "an event without data is dropped")
    }
}

@MainActor
final class ChatStoreTimelineTests: XCTestCase {
    private var fileURL: URL!
    private var defaults: UserDefaults!

    override func setUp() {
        fileURL = FileManager.default.temporaryDirectory.appendingPathComponent("timeline-\(UUID().uuidString).json")
        defaults = UserDefaults(suiteName: "ChatStoreTimelineTests-\(UUID().uuidString)")
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: fileURL)
    }

    private func makeStore() -> ChatStore {
        let client = StubURLProtocol.makeClient()
        return ChatStore(api: { SecretaryAPI(client: client) }, fileURL: fileURL, defaults: defaults)
    }

    private static let page = """
    {"items":[
      {"id":"tl_3","ts":"2026-10-21T10:00:02Z","channel":"telegram","role":"lucina","text":"Fatto."},
      {"id":"tl_2","ts":"2026-10-21T10:00:01Z","channel":"telegram","role":"user","text":"latte"}
    ],"nextBefore":"tl_2"}
    """

    func testSendReconcilesWithTheStreamWithoutDuplicates() async throws {
        StubURLProtocol.handler = { request in
            switch request.url?.path {
            case "/v1/timeline": return .init(body: Self.page)
            case "/v1/chat":
                let body = try? JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: String]
                let client = body?["clientMessageId"] ?? ""
                return .init(body: #"{"reply":"Ciao!","action":"reply","job":null,"userItemId":"tl_10","lucinaItemId":"tl_11","clientMessageId":"\#(client)"}"#)
            default: return .init(status: 404, body: #"{"error":{"code":"not_found","message":"x"}}"#)
            }
        }
        let store = makeStore()
        await store.refresh()
        XCTAssertEqual(store.mode, .server)
        XCTAssertEqual(store.items.map(\.id), ["tl_2", "tl_3"])
        XCTAssertTrue(store.hasOlder)

        let sent = await store.send("ciao")
        XCTAssertTrue(sent)
        let chat = try XCTUnwrap(StubURLProtocol.requests.first { $0.url?.path == "/v1/chat" })
        XCTAssertEqual(chat.value(forHTTPHeaderField: "X-Secretary-Client"), "ios")
        XCTAssertEqual(store.items.suffix(2).map(\.id), ["tl_10", "tl_11"], "server ids from the chat response")
        XCTAssertFalse(store.items.contains { $0.isLocal })

        // The same two items then arrive on the stream: nothing is duplicated.
        let clientId = try XCTUnwrap(store.items.first { $0.id == "tl_10" }?.clientMessageId)
        store.handle(SSEEvent(event: "item", data: #"{"id":"tl_10","ts":"2026-10-21T10:05:00Z","channel":"ios","role":"user","text":"ciao","clientMessageId":"\#(clientId)"}"#, id: "tl_10"))
        store.handle(SSEEvent(event: "item", data: #"{"id":"tl_11","ts":"2026-10-21T10:05:01Z","channel":"ios","role":"lucina","text":"Ciao!","replyTo":"tl_10"}"#, id: "tl_11"))
        store.handle(SSEEvent(event: "ping", data: "{}", id: nil))
        XCTAssertEqual(store.items.map(\.id), ["tl_2", "tl_3", "tl_10", "tl_11"])

        // A message from another channel appears live.
        store.handle(SSEEvent(event: "item", data: #"{"id":"tl_12","ts":"2026-10-21T10:06:00Z","channel":"alexa","role":"user","text":"eggs"}"#, id: "tl_12"))
        XCTAssertEqual(store.items.last?.channel, .alexa)

        // The cache gives the same first paint to a new store.
        XCTAssertEqual(makeStore().items.map(\.id), store.items.map(\.id))
    }

    func testStreamCanBeatTheChatResponse() async {
        let store = makeStore()
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/chat" {
                return .init(body: #"{"reply":"Ciao!","action":"reply","job":null,"userItemId":"tl_10","lucinaItemId":"tl_11"}"#)
            }
            return .init(body: #"{"items":[],"nextBefore":null}"#)
        }
        await store.refresh()
        // Simulate the stream delivering the user's item while the chat call is in flight.
        let task = Task { await store.send("ciao") }
        await Task.yield()
        if let local = store.items.first(where: \.isLocal), let client = local.clientMessageId {
            store.handle(SSEEvent(event: "item", data: #"{"id":"tl_10","ts":"2026-10-21T10:05:00Z","channel":"ios","role":"user","text":"ciao","clientMessageId":"\#(client)"}"#, id: "tl_10"))
        }
        _ = await task.value
        XCTAssertEqual(store.items.map(\.id), ["tl_10", "tl_11"])
    }

    func testLoadOlderDedupesAndStopsAtTheEnd() async {
        StubURLProtocol.handler = { request in
            if request.url?.query?.contains("before=tl_2") == true {
                return .init(body: """
                {"items":[
                  {"id":"tl_2","ts":"2026-10-21T10:00:01Z","channel":"telegram","role":"user","text":"latte"},
                  {"id":"tl_1","ts":"2026-10-20T09:00:00Z","channel":"web","role":"user","text":"older"}
                ],"nextBefore":null}
                """)
            }
            return .init(body: Self.page)
        }
        let store = makeStore()
        await store.refresh()
        await store.loadOlder()
        XCTAssertEqual(store.items.map(\.id), ["tl_1", "tl_2", "tl_3"])
        XCTAssertFalse(store.hasOlder)
        let requests = StubURLProtocol.requests.count
        await store.loadOlder()
        XCTAssertEqual(StubURLProtocol.requests.count, requests, "no request past the end")
    }

    func testServerWithoutTimelineFallsBackToLocalOnly() async {
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/chat" { return .init(body: #"{"reply":"Hi","action":"reply","job":null}"#) }
            return .init(status: 404, body: #"{"error":{"code":"not_found","message":"Not found."}}"#)
        }
        let store = makeStore()
        await store.refresh()
        XCTAssertEqual(store.mode, .localOnly)
        await store.send("hello")
        XCTAssertEqual(store.items.map(\.role), [.user, .lucina])
        XCTAssertEqual(store.items.map(\.localStatus), [.sent, .sent])
        XCTAssertEqual(store.items.last?.text, "Hi")
        // Kept on the phone across launches in this mode.
        XCTAssertEqual(makeStore().items.count, 2)
        XCTAssertEqual(makeStore().mode, .localOnly)
    }

    func testFailedSendCanBeRetriedWithTheSameClientMessageId() async throws {
        var calls = 0
        StubURLProtocol.handler = { request in
            guard request.url?.path == "/v1/chat" else { return .init(body: #"{"items":[],"nextBefore":null}"#) }
            calls += 1
            if calls == 1 { return .init(status: 502, body: #"{"error":{"code":"lucina_unreachable","message":"down"}}"#) }
            return .init(body: #"{"reply":"Back.","action":"reply","job":null,"userItemId":"tl_20","lucinaItemId":"tl_21"}"#)
        }
        let store = makeStore()
        await store.refresh()
        await store.send("are you there?")
        let failed = try XCTUnwrap(store.items.first)
        XCTAssertEqual(failed.localStatus, .error)
        XCTAssertEqual(failed.localError, "Lucina is offline. Not delivered.")
        await store.retry(failed.id)
        let bodies = StubURLProtocol.requests.filter { $0.url?.path == "/v1/chat" }
            .compactMap { try? JSONSerialization.jsonObject(with: $0.httpBody ?? Data()) as? [String: String] }
        XCTAssertEqual(bodies.count, 2)
        XCTAssertEqual(bodies[0]["clientMessageId"], bodies[1]["clientMessageId"])
        XCTAssertEqual(store.items.map(\.id), ["tl_20", "tl_21"])
    }
}
