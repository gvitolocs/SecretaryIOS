import XCTest
@testable import Secretary

@MainActor
final class ConversationStoreTests: XCTestCase {
    private var directory: URL!
    override func setUp() {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }
    override func tearDown() { try? FileManager.default.removeItem(at: directory) }

    func testConversationListCacheAndAPI() async throws {
        StubURLProtocol.handler = { request in
            XCTAssertEqual(request.url?.path, "/v1/timeline/conversations")
            return .init(body: #"{"items":[{"key":"agent:codex","kind":"agent","title":"Codex","channel":"web","count":4,"lastId":"04","lastTs":1000,"lastRole":"lucina","lastText":"Done"}]}"#)
        }
        let list = try await SecretaryAPI(client: StubURLProtocol.makeClient()).conversations()
        XCTAssertEqual(list.first?.kind, "agent")
        XCTAssertFalse(list.first?.canContinue ?? true)
        let db = ChatDatabase(url: directory.appendingPathComponent("chat.sqlite"))
        try await db.saveConversations(list)
        let reopened = ChatDatabase(url: directory.appendingPathComponent("chat.sqlite"))
        let cached = try await reopened.cachedConversations()
        XCTAssertEqual(cached, list)
        XCTAssertEqual(StubURLProtocol.requests.count, 1, "reading the cached list performs no request")
        try await db.clear()
        let cleared = try await db.cachedConversations()
        XCTAssertTrue(cleared.isEmpty)
    }

    func testConversationUsesCursorAndDatabaseBeforePagingServer() async throws {
        let db = ChatDatabase(url: directory.appendingPathComponent("chat.sqlite"))
        let initial = (1...60).map { value in
            TimelineItem(id: String(format: "%03d", value), ts: Date(timeIntervalSince1970: Double(value)),
                         channel: .telegram, role: .user, text: "message")
        }
        try await db.write(initial, state: ["conversation:telegram": "c60"])
        StubURLProtocol.handler = { request in
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems!
            XCTAssertEqual(query.first { $0.name == "conversation" }?.value, "telegram")
            if query.contains(where: { $0.name == "before" }) {
                XCTAssertEqual(query.first { $0.name == "before" }?.value, "001")
                return .init(body: #"{"items":[],"nextBefore":null}"#)
            }
            XCTAssertEqual(query.first { $0.name == "after" }?.value, "c60")
            return .init(body: #"{"items":[{"id":"061","ts":61000,"channel":"telegram","role":"lucina","text":"new"}],"cursor":"c61","hasMore":false}"#)
        }
        let client = StubURLProtocol.makeClient()
        let store = ConversationStore(key: "telegram", database: db, api: { SecretaryAPI(client: client) })
        await store.open()
        XCTAssertEqual(store.items.count, 51)
        XCTAssertEqual(store.items.first?.id, "011")
        XCTAssertEqual(store.items.last?.id, "061")
        let cursor = try await db.value("conversation:telegram")
        XCTAssertEqual(cursor, "c61")
        await store.loadOlder()
        XCTAssertEqual(store.items.count, 61)
        XCTAssertEqual(StubURLProtocol.requests.count, 1, "older disk pages are used before the server")
        await store.loadOlder()
        XCTAssertFalse(store.hasOlder)
        XCTAssertEqual(StubURLProtocol.requests.count, 2)
    }

    func testContinuePassesLoadedItemToMainComposer() async throws {
        let conversation = try Fixtures.decode(ChatConversation.self, #"{"key":"device:other","kind":"device","title":"Other phone","lastId":"01"}"#)
        XCTAssertTrue(conversation.canContinue)
        let last = TimelineItem(id: "01", ts: Date(), channel: .web, role: .lucina, text: "The last message")
        let chat = ChatStore(api: { SecretaryAPI(client: StubURLProtocol.makeClient()) },
                             fileURL: directory.appendingPathComponent("timeline.json"),
                             defaults: UserDefaults(suiteName: UUID().uuidString)!)
        await chat.prepare()
        XCTAssertTrue(chat.beginReply(last))
        XCTAssertEqual(chat.replyingTo, last)
        XCTAssertEqual(chat.takeReply()?.id, "01")
    }
}
