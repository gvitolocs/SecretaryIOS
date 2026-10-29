import Foundation

/// The chat's outgoing text messages, sent one at a time in the order they were written.
///
/// Plain state, no I/O: `ChatStore` asks it which message goes next and reports how each send ended. A send
/// that runs longer than `timeout` is failed by `expire(now:)`, so a request that never finishes cannot hold
/// up the messages behind it. A failed message stays here until it is retried (it goes to the back of the
/// line) or dropped.
struct ChatSendQueue: Equatable, Sendable {
    static let timeout: TimeInterval = APIClient.chatTimeout

    enum State: Equatable, Sendable {
        case queued
        case sending(since: Date)
        case failed
    }

    struct Entry: Equatable, Sendable {
        let clientId: String
        let text: String
        let replyTo: String?
        var state: State
    }

    let timeout: TimeInterval
    /// In send order.
    private(set) var entries: [Entry] = []

    init(timeout: TimeInterval = ChatSendQueue.timeout) {
        self.timeout = timeout
    }

    /// The message on the wire, if any.
    var inFlight: Entry? {
        entries.first { if case .sending = $0.state { return true } else { return false } }
    }

    var isSending: Bool { inFlight != nil }

    func state(of clientId: String) -> State? {
        entries.first { $0.clientId == clientId }?.state
    }

    /// Adds a message at the back of the line. A failed one is moved there again (Retry). False when the
    /// message is already waiting or on the wire.
    @discardableResult
    mutating func enqueue(clientId: String, text: String, replyTo: String?) -> Bool {
        if let index = entries.firstIndex(where: { $0.clientId == clientId }) {
            guard entries[index].state == .failed else { return false }
            entries.remove(at: index)
        }
        entries.append(Entry(clientId: clientId, text: text, replyTo: replyTo, state: .queued))
        return true
    }

    /// The next message to send, now marked as sending; nil while one is on the wire or none is waiting.
    mutating func start(now: Date) -> Entry? {
        guard !isSending, let index = entries.firstIndex(where: { $0.state == .queued }) else { return nil }
        entries[index].state = .sending(since: now)
        return entries[index]
    }

    /// The server took the message: it leaves the queue. False when it was no longer on the wire (it timed
    /// out, or the queue was cleared).
    @discardableResult
    mutating func succeed(_ clientId: String) -> Bool {
        guard let index = sendingIndex(clientId) else { return false }
        entries.remove(at: index)
        return true
    }

    /// The send failed: the message waits for a Retry and no longer blocks the ones behind it.
    @discardableResult
    mutating func fail(_ clientId: String) -> Bool {
        guard let index = sendingIndex(clientId) else { return false }
        entries[index].state = .failed
        return true
    }

    /// Fails every send that started more than `timeout` ago; returns their client ids. Called before each
    /// `start`, and when the app comes back to the foreground or the network returns.
    mutating func expire(now: Date) -> [String] {
        var expired: [String] = []
        for index in entries.indices {
            if case let .sending(since) = entries[index].state, now.timeIntervalSince(since) >= timeout {
                entries[index].state = .failed
                expired.append(entries[index].clientId)
            }
        }
        return expired
    }

    /// Forgets a message (its bubble is gone, or the server copy arrived).
    mutating func remove(_ clientId: String) {
        entries.removeAll { $0.clientId == clientId }
    }

    mutating func removeAll() {
        entries.removeAll()
    }

    private func sendingIndex(_ clientId: String) -> Int? {
        entries.firstIndex { entry in
            guard entry.clientId == clientId, case .sending = entry.state else { return false }
            return true
        }
    }
}

extension ChatSendQueue {
    /// Runs `operation` for at most `seconds`, then cancels it and throws `APIError.timeout`. Returns at the
    /// deadline even when the operation ignores cancellation, so a hung request cannot hold up the queue.
    static func within<T: Sendable>(_ seconds: TimeInterval, _ operation: @escaping @Sendable () async throws -> T) async throws -> T {
        let gate = DeadlineGate<T>()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                gate.install(continuation)
                let work = Task {
                    do { gate.finish(.success(try await operation())) } catch { gate.finish(.failure(error)) }
                }
                let timer = Task {
                    try? await Task.sleep(for: .seconds(seconds))
                    gate.finish(.failure(APIError.timeout))
                }
                gate.onFinish { work.cancel(); timer.cancel() }
            }
        } onCancel: {
            gate.finish(.failure(CancellationError()))
        }
    }
}

/// The first of the operation, the timer and cancellation to finish resumes the caller; the others are ignored.
private final class DeadlineGate<T: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<T, Error>?
    private var result: Result<T, Error>?
    private var cleanups: [@Sendable () -> Void] = []

    func install(_ continuation: CheckedContinuation<T, Error>) {
        lock.lock()
        if let result {
            lock.unlock()
            continuation.resume(with: result)
            return
        }
        self.continuation = continuation
        lock.unlock()
    }

    func onFinish(_ cleanup: @escaping @Sendable () -> Void) {
        lock.lock()
        if result != nil {
            lock.unlock()
            cleanup()
            return
        }
        cleanups.append(cleanup)
        lock.unlock()
    }

    func finish(_ outcome: Result<T, Error>) {
        lock.lock()
        guard result == nil else {
            lock.unlock()
            return
        }
        result = outcome
        let waiting = continuation
        continuation = nil
        let pending = cleanups
        cleanups = []
        lock.unlock()
        waiting?.resume(with: outcome)
        pending.forEach { $0() }
    }
}
