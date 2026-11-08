import Foundation
import Observation
import os
import UIKit

/// Where the audio frames go from the capture thread: sends only while a turn is streaming.
final class VoiceAudioPipe: @unchecked Sendable {
    private let lock = NSLock()
    private var task: URLSessionWebSocketTask?
    private var active = false

    func attach(_ task: URLSessionWebSocketTask?) {
        lock.lock()
        self.task = task
        if task == nil { active = false }
        lock.unlock()
    }

    func setActive(_ on: Bool) {
        lock.lock()
        active = on
        lock.unlock()
    }

    /// Safe from any thread; URLSession keeps the order of the sends.
    func send(_ frame: Data) {
        lock.lock()
        let task = active ? self.task : nil
        lock.unlock()
        task?.send(.data(frame)) { _ in }
    }
}

/// The voice WebSocket (`GET /v1/voice/ws`): opened when a voice screen shows (the chat, the Lucina tab), kept while
/// one is visible, reopened with backoff, closed 60 s after the last one went away. One turn at a time; a finished
/// note is followed by its `shareId`.
@Observable
@MainActor
final class VoiceSocket: NSObject {
    enum State: Equatable { case closed, connecting, open }
    typealias Handler = (VoiceEvent) -> Void

    nonisolated static let path = "/v1/voice/ws"
    nonisolated static let idleClose: Duration = .seconds(60)
    nonisolated static let maxWatchedNotes = 4

    private(set) var state: State = .closed
    /// Some turn is running (a new `begin` is refused).
    var busy: Bool { turn != nil }
    /// Open, and no turn running.
    var isReady: Bool { state == .open && turn == nil }

    @ObservationIgnored let pipe = VoiceAudioPipe()
    @ObservationIgnored private let client: APIClient
    @ObservationIgnored private var owners: Set<String> = []
    @ObservationIgnored private var session: URLSession?
    @ObservationIgnored private var task: URLSessionWebSocketTask?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var attempt = 0
    @ObservationIgnored private var reconnect: Task<Void, Never>?
    @ObservationIgnored private var idle: Task<Void, Never>?
    @ObservationIgnored private var keepAlive: Task<Void, Never>?
    @ObservationIgnored private var turn: Handler?
    @ObservationIgnored private var notes: [String: Handler] = [:]
    @ObservationIgnored private var noteOrder: [String] = []
    @ObservationIgnored private var observers: [NSObjectProtocol] = []
    @ObservationIgnored private var foreground = true
    static let log = Logger(subsystem: "tech.getsecretary.voice", category: "socket")

    init(client: APIClient) {
        self.client = client
        super.init()
        let center = NotificationCenter.default
        observers.append(center.addObserver(forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.becameActive() }
        })
        observers.append(center.addObserver(forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.enteredBackground() }
        })
    }

    deinit {
        for observer in observers { NotificationCenter.default.removeObserver(observer) }
    }

    // MARK: URL

    /// `https://host/base` becomes `wss://host/base/v1/voice/ws`.
    nonisolated static func url(base: URL) -> URL? {
        guard var components = URLComponents(url: base, resolvingAgainstBaseURL: false), let scheme = components.scheme?.lowercased()
        else { return nil }
        switch scheme {
        case "https": components.scheme = "wss"
        case "http": components.scheme = "ws"
        default: return nil
        }
        let basePath = components.percentEncodedPath.hasSuffix("/") ? String(components.percentEncodedPath.dropLast()) : components.percentEncodedPath
        components.percentEncodedPath = basePath + path
        components.query = nil
        return components.url
    }

    nonisolated static func request(base: URL, accessKey: String) -> URLRequest? {
        guard let url = url(base: base) else { return nil }
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 15)
        request.setValue("Bearer \(accessKey)", forHTTPHeaderField: "Authorization")
        request.setValue("ios", forHTTPHeaderField: "X-Secretary-Client")
        return request
    }

    // MARK: Lifetime (the voice screens)

    /// A voice screen appeared: open the socket if it is not.
    func retain(_ owner: String) {
        owners.insert(owner)
        idle?.cancel()
        idle = nil
        if foreground, state == .closed { connect() }
    }

    /// A voice screen went away; 60 s after the last one the socket closes.
    func release(_ owner: String) {
        owners.remove(owner)
        guard owners.isEmpty, idle == nil else { return }
        idle = Task { [weak self] in
            try? await Task.sleep(for: Self.idleClose)
            guard !Task.isCancelled, let self, self.owners.isEmpty else { return }
            self.idle = nil
            self.disconnect()
        }
    }

    private func becameActive() {
        foreground = true
        if !owners.isEmpty, state == .closed { connect() }
    }

    private func enteredBackground() {
        foreground = false
        disconnect()
    }

    // MARK: Connection

    private func connect() {
        reconnect?.cancel()
        reconnect = nil
        guard state == .closed, let key = client.accessKey, !key.isEmpty,
              let request = Self.request(base: client.baseURL, accessKey: key) else { return }
        generation += 1
        let mine = generation
        let delegate = SocketDelegate(owner: self, generation: mine)
        let session = URLSession(configuration: .default, delegate: delegate, delegateQueue: nil)
        let task = session.webSocketTask(with: request)
        task.maximumMessageSize = 1 << 20
        self.session = session
        self.task = task
        pipe.attach(task)
        state = .connecting
        task.resume()
        Task { [weak self] in await self?.receive(task, generation: mine) }
    }

    private func disconnect() {
        reconnect?.cancel()
        reconnect = nil
        guard state != .closed || task != nil else { return }
        dropConnection(reason: "closed by the app", retry: false)
    }

    fileprivate func opened(generation mine: Int) {
        guard mine == generation, state == .connecting else { return }
        state = .open
        attempt = 0
        Self.log.debug("voice socket open")
        keepAlive?.cancel()
        guard let current = task else { return }
        keepAlive = Task { [weak self, weak current] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(25))
                guard !Task.isCancelled, let current else { return }
                current.sendPing { error in
                    guard error != nil else { return }
                    Task { @MainActor in self?.failed(generation: mine, reason: "ping failed") }
                }
            }
        }
    }

    fileprivate func failed(generation: Int, reason: String) {
        guard generation == self.generation, task != nil else { return }
        dropConnection(reason: reason, retry: true)
    }

    private func dropConnection(reason: String, retry: Bool) {
        generation += 1
        keepAlive?.cancel()
        keepAlive = nil
        task?.cancel(with: .goingAway, reason: nil)
        session?.invalidateAndCancel()
        task = nil
        session = nil
        pipe.attach(nil)
        let wasOpen = state != .closed
        state = .closed
        if wasOpen { Self.log.debug("voice socket closed: \(reason, privacy: .public)") }
        let handler = turn
        turn = nil
        notes = [:]
        noteOrder = []
        handler?(.closed)
        guard retry, !owners.isEmpty, foreground else { return }
        let delay = VoiceBackoff.delay(attempt: attempt)
        attempt += 1
        reconnect?.cancel()
        reconnect = Task { [weak self] in
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled else { return }
            self?.connect()
        }
    }

    private func receive(_ task: URLSessionWebSocketTask, generation: Int) async {
        while !Task.isCancelled {
            do {
                let message = try await task.receive()
                guard generation == self.generation else { return }
                switch message {
                case let .data(data): route(.audio(data))
                case let .string(text):
                    if let event = VoiceEvent.parse(text: text) { route(event) }
                @unknown default: break
                }
            } catch {
                failed(generation: generation, reason: "receive failed")
                return
            }
        }
    }

    // MARK: Turns

    /// Starts a turn and sends `start`; false when the socket is not open or a turn is running. Audio frames then go
    /// through `pipe.send` (the caller's capture), and `handler` gets every message of the turn.
    @discardableResult
    func begin(_ start: VoiceStart, handler: @escaping Handler) -> Bool {
        guard isReady, let task else { return false }
        turn = handler
        pipe.setActive(true)
        task.send(.string(start.json())) { _ in }
        return true
    }

    /// The owner let go (or the app gives up listening): the server runs the turn. Stops the audio frames.
    func end() {
        guard turn != nil else { return }
        pipe.setActive(false)
        task?.send(.string(VoiceStart.end)) { _ in }
    }

    /// Drops the turn; the server answers nothing for it.
    func cancel() {
        guard turn != nil else { return }
        turn = nil
        pipe.setActive(false)
        task?.send(.string(VoiceStart.cancel)) { _ in }
    }

    /// Stops sending audio without ending the turn (the server's `endpoint` arrived).
    func stopAudio() { pipe.setActive(false) }

    private func route(_ event: VoiceEvent) {
        switch event {
        case .unknown: return
        case .note(let shareId, _):
            let handler = turn
            turn = nil
            pipe.setActive(false)
            if let handler { watch(shareId, handler) }
            handler?(event)
            return
        default: break
        }
        if let shareId = event.shareId {
            notes[shareId]?(event)
            switch event {
            case .done, .error: unwatch(shareId)
            default: break
            }
            return
        }
        let handler = turn
        switch event {
        case .done, .error:
            turn = nil
            pipe.setActive(false)
        case let .listening(listening) where !listening.understandsStreaming:
            // An old server: the caller decides (it sends `cancel` and uses its old path).
            break
        default: break
        }
        handler?(event)
    }

    private func watch(_ shareId: String, _ handler: @escaping Handler) {
        notes[shareId] = handler
        noteOrder.removeAll { $0 == shareId }
        noteOrder.append(shareId)
        while noteOrder.count > Self.maxWatchedNotes { notes[noteOrder.removeFirst()] = nil }
    }

    private func unwatch(_ shareId: String) {
        notes[shareId] = nil
        noteOrder.removeAll { $0 == shareId }
    }
}

/// URLSession's delegate callbacks, forwarded to the main actor.
private final class SocketDelegate: NSObject, URLSessionWebSocketDelegate, @unchecked Sendable {
    weak var owner: VoiceSocket?
    let generation: Int

    init(owner: VoiceSocket, generation: Int) {
        self.owner = owner
        self.generation = generation
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didOpenWithProtocol protocol: String?) {
        let generation = generation
        Task { @MainActor [owner] in owner?.opened(generation: generation) }
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didCloseWith closeCode: URLSessionWebSocketTask.CloseCode,
                    reason: Data?) {
        let generation = generation
        Task { @MainActor [owner] in owner?.failed(generation: generation, reason: "closed by the server") }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard error != nil else { return }
        let generation = generation
        Task { @MainActor [owner] in owner?.failed(generation: generation, reason: "connection failed") }
    }
}
