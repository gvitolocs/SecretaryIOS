import XCTest
@testable import Secretary

/// The chat's send queue: one message on the wire at a time, in order, and nothing waits forever behind a
/// send that never finishes.
final class ChatSendQueueTests: XCTestCase {
    private let t0 = Date(timeIntervalSince1970: 1_800_000_000)

    func testTimeoutIs45Seconds() {
        XCTAssertEqual(ChatSendQueue.timeout, 45)
        XCTAssertEqual(ChatSendQueue().timeout, 45)
        XCTAssertEqual(APIClient.chatTimeout, 45, "the request's own timeout matches the queue's")
    }

    func testSendOk() {
        var queue = ChatSendQueue()
        XCTAssertTrue(queue.enqueue(clientId: "a", text: "test", replyTo: nil))
        XCTAssertEqual(queue.state(of: "a"), .queued)
        XCTAssertFalse(queue.isSending)

        let entry = queue.start(now: t0)
        XCTAssertEqual(entry?.clientId, "a")
        XCTAssertEqual(entry?.text, "test")
        XCTAssertEqual(queue.state(of: "a"), .sending(since: t0))
        XCTAssertTrue(queue.isSending)

        XCTAssertTrue(queue.succeed("a"))
        XCTAssertNil(queue.state(of: "a"), "a delivered message leaves the queue")
        XCTAssertFalse(queue.isSending)
        XCTAssertNil(queue.start(now: t0), "nothing else to send")
    }

    func testTimeoutFailsThenRetrySucceeds() {
        var queue = ChatSendQueue()
        queue.enqueue(clientId: "a", text: "test", replyTo: "tl_1")
        _ = queue.start(now: t0)

        XCTAssertEqual(queue.expire(now: t0.addingTimeInterval(44)), [], "still within the timeout")
        XCTAssertEqual(queue.expire(now: t0.addingTimeInterval(45)), ["a"])
        XCTAssertEqual(queue.state(of: "a"), .failed)
        XCTAssertFalse(queue.isSending)
        XCTAssertNil(queue.start(now: t0.addingTimeInterval(46)), "a failed message is not resent on its own")

        // The late answer of the timed-out attempt does not count as this attempt's end.
        XCTAssertFalse(queue.succeed("a"))
        XCTAssertFalse(queue.fail("a"))

        // Retry: same client id, same text and reply.
        XCTAssertTrue(queue.enqueue(clientId: "a", text: "test", replyTo: "tl_1"))
        XCTAssertEqual(queue.state(of: "a"), .queued)
        let retry = queue.start(now: t0.addingTimeInterval(50))
        XCTAssertEqual(retry?.clientId, "a")
        XCTAssertEqual(retry?.replyTo, "tl_1")
        XCTAssertTrue(queue.succeed("a"))
        XCTAssertTrue(queue.entries.isEmpty)
    }

    func testAnErrorFailsTheSendAndRetryWorks() {
        var queue = ChatSendQueue()
        queue.enqueue(clientId: "a", text: "one", replyTo: nil)
        _ = queue.start(now: t0)
        XCTAssertTrue(queue.fail("a"))
        XCTAssertEqual(queue.state(of: "a"), .failed)
        XCTAssertTrue(queue.enqueue(clientId: "a", text: "one", replyTo: nil))
        XCTAssertEqual(queue.start(now: t0)?.clientId, "a")
        XCTAssertTrue(queue.succeed("a"))
    }

    func testMessagesGoInOrderOneAtATime() {
        var queue = ChatSendQueue()
        queue.enqueue(clientId: "a", text: "one", replyTo: nil)
        queue.enqueue(clientId: "b", text: "two", replyTo: nil)
        queue.enqueue(clientId: "c", text: "three", replyTo: nil)
        XCTAssertFalse(queue.enqueue(clientId: "b", text: "two", replyTo: nil), "already waiting: not added twice")

        XCTAssertEqual(queue.start(now: t0)?.clientId, "a")
        XCTAssertNil(queue.start(now: t0), "one on the wire at a time")
        queue.succeed("a")
        XCTAssertEqual(queue.start(now: t0)?.clientId, "b")
        queue.succeed("b")
        XCTAssertEqual(queue.start(now: t0)?.clientId, "c")
    }

    func testSecondSendIsNotBlockedByAStuckFirst() {
        var queue = ChatSendQueue()
        queue.enqueue(clientId: "stuck", text: "first", replyTo: nil)
        _ = queue.start(now: t0) // never answers
        // The second message is accepted at once (its bubble shows), it is not dropped.
        XCTAssertTrue(queue.enqueue(clientId: "next", text: "test", replyTo: nil))
        XCTAssertEqual(queue.state(of: "next"), .queued)

        let later = t0.addingTimeInterval(ChatSendQueue.timeout)
        XCTAssertEqual(queue.expire(now: later), ["stuck"])
        XCTAssertEqual(queue.start(now: later)?.clientId, "next")
        XCTAssertTrue(queue.succeed("next"))
        XCTAssertEqual(queue.state(of: "stuck"), .failed, "the stuck one waits for a Retry")
    }

    func testAFailedMessageDoesNotBlockLaterOnes() {
        var queue = ChatSendQueue()
        queue.enqueue(clientId: "a", text: "one", replyTo: nil)
        _ = queue.start(now: t0)
        queue.fail("a")
        queue.enqueue(clientId: "b", text: "two", replyTo: nil)
        XCTAssertEqual(queue.start(now: t0)?.clientId, "b")
        // A retry goes behind the ones already waiting.
        queue.enqueue(clientId: "c", text: "three", replyTo: nil)
        queue.enqueue(clientId: "a", text: "one", replyTo: nil)
        queue.succeed("b")
        XCTAssertEqual(queue.start(now: t0)?.clientId, "c")
        queue.succeed("c")
        XCTAssertEqual(queue.start(now: t0)?.clientId, "a")
    }

    /// Back from the background (or the network came back) after the send hung: it is failed, and the queue
    /// is free again.
    func testForegroundResetFailsStuckSendsAndFreesTheQueue() {
        var queue = ChatSendQueue()
        queue.enqueue(clientId: "a", text: "one", replyTo: nil)
        queue.enqueue(clientId: "b", text: "two", replyTo: nil)
        _ = queue.start(now: t0)
        XCTAssertTrue(queue.isSending)

        // Ten minutes in the background.
        let foreground = t0.addingTimeInterval(600)
        XCTAssertEqual(queue.expire(now: foreground), ["a"])
        XCTAssertFalse(queue.isSending, "no in-flight flag survives the reset")
        XCTAssertEqual(queue.expire(now: foreground), [], "reset is idempotent")
        XCTAssertEqual(queue.start(now: foreground)?.clientId, "b")

        // A send that started just before the app left is left alone.
        XCTAssertEqual(queue.expire(now: foreground.addingTimeInterval(10)), [])
        XCTAssertEqual(queue.state(of: "b"), .sending(since: foreground))
    }

    func testRemoveAllClearsEverything() {
        var queue = ChatSendQueue()
        queue.enqueue(clientId: "a", text: "one", replyTo: nil)
        _ = queue.start(now: t0)
        queue.enqueue(clientId: "b", text: "two", replyTo: nil)
        queue.removeAll()
        XCTAssertTrue(queue.entries.isEmpty)
        XCTAssertFalse(queue.isSending)
    }

    // MARK: Deadline

    func testDeadlineReturnsEvenWhenTheRequestNeverFinishes() async {
        let started = Date()
        do {
            _ = try await ChatSendQueue.within(0.2) { () async throws -> Int in
                // Ignores cancellation, like a request that never comes back.
                await withUnsafeContinuation { (_: UnsafeContinuation<Void, Never>) in }
                return 1
            }
            XCTFail("expected a timeout")
        } catch {
            XCTAssertEqual(APIError.wrap(error).code, "timeout")
            XCTAssertEqual(ChatStore.describe(APIError.wrap(error)), "No answer in time.")
        }
        XCTAssertLessThan(Date().timeIntervalSince(started), 5)
    }

    func testDeadlinePassesTheAnswerThrough() async throws {
        let value = try await ChatSendQueue.within(5) { 42 }
        XCTAssertEqual(value, 42)
    }
}

/// The store side: a message is never dropped because another one is on its way.
@MainActor
final class ChatStoreSendQueueTests: XCTestCase {
    private var fileURL: URL!
    private var defaults: UserDefaults!

    override func setUp() {
        fileURL = FileManager.default.temporaryDirectory.appendingPathComponent("timeline-\(UUID().uuidString).json")
        defaults = UserDefaults(suiteName: "ChatStoreSendQueueTests-\(UUID().uuidString)")
        StubURLProtocol.requests = []
        StubURLProtocol.handler = { request in
            guard request.url?.path == "/v1/chat" else { return .init(body: #"{"items":[],"nextBefore":null}"#) }
            let body = try? JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: String]
            return .init(body: #"{"reply":"ok \#(body?["message"] ?? "")","action":"reply","job":null}"#)
        }
    }

    override func tearDown() {
        StubURLProtocol.handler = nil
        try? FileManager.default.removeItem(at: fileURL)
        for suffix in ["", "-wal", "-shm"] {
            try? FileManager.default.removeItem(atPath: fileURL.deletingPathExtension().appendingPathExtension("sqlite").path + suffix)
        }
    }

    private func makeStore() -> ChatStore {
        let client = StubURLProtocol.makeClient()
        return ChatStore(api: { SecretaryAPI(client: client) }, fileURL: fileURL, defaults: defaults)
    }

    func testASecondMessageWhileTheFirstIsInFlightIsShownAndSent() async {
        let store = makeStore()
        await store.prepare()
        let first = Task { await store.send("one") }
        let second = Task { await store.send("test") }
        await Task.yield()
        let sentFirst = await first.value
        let sentSecond = await second.value
        XCTAssertTrue(sentFirst)
        XCTAssertTrue(sentSecond, "never dropped because another send was outstanding")
        let messages = StubURLProtocol.requests.filter { $0.url?.path == "/v1/chat" }
            .compactMap { try? JSONSerialization.jsonObject(with: $0.httpBody ?? Data()) as? [String: String] }
            .compactMap { $0["message"] }
        XCTAssertEqual(messages, ["one", "test"], "sent in order")
        XCTAssertEqual(store.items.filter { $0.role == .user }.map(\.text), ["one", "test"])
        XCTAssertEqual(store.items.filter { $0.role == .user }.map(\.localStatus), [.sent, .sent])
        XCTAssertFalse(store.isSending)
    }

    func testForegroundResetFailsABubbleStuckInSending() async throws {
        let store = makeStore()
        await store.prepare()
        // A bubble left "Sending…" with no send behind it.
        store.apply([TimelineItem(id: TimelineItem.localPrefix + "c1", ts: Date().addingTimeInterval(-120), channel: .ios,
                                  role: .user, text: "test", clientMessageId: "c1", localStatus: .sending)],
                    persistAfter: false)
        let stuck = store.items.first { $0.id == TimelineItem.localPrefix + "c1" }
        XCTAssertEqual(stuck?.localStatus, .sending)
        store.resumeSends()
        let failed = try XCTUnwrap(store.items.first { $0.id == TimelineItem.localPrefix + "c1" })
        XCTAssertEqual(failed.localStatus, .error)
        XCTAssertEqual(failed.localError, "No answer in time.")
        XCTAssertFalse(store.isSending)

        // Retry sends it.
        await store.retry(failed.id)
        XCTAssertEqual(StubURLProtocol.requests.filter { $0.url?.path == "/v1/chat" }.count, 1)
        XCTAssertEqual(store.items.first { $0.clientMessageId == "c1" && $0.role == .user }?.localStatus, .sent)
    }
}
