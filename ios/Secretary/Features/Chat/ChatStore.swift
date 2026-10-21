import Foundation
import Observation

struct ChatJob: Codable, Equatable, Sendable {
    var id: String
    var status: String
    var action: String?
    var result: String?
    var error: String?
    var imageUrl: String?
    /// A transient polling problem, shown under the card while it keeps trying.
    var pollError: String?

    var isTerminal: Bool { JobInfo.isTerminal(status) }
}

struct ChatMessage: Codable, Identifiable, Equatable, Sendable {
    enum Role: String, Codable, Sendable { case user, lucina }
    enum Status: String, Codable, Sendable { case sending, sent, error }

    /// For user messages this is also the `clientMessageId`, so a retry is idempotent.
    let id: String
    let role: Role
    var text: String
    let createdAt: Date
    var status: Status?
    var error: String?
    var action: String?
    var job: ChatJob?
}

/// The conversation with Lucina: optimistic sends, retries with the same
/// `clientMessageId`, job polling every 3 s, persisted to a small JSON file.
@Observable
@MainActor
final class ChatStore {
    nonisolated static let maxCharacters = 4000
    nonisolated static let pollInterval: Duration = .seconds(3)

    private(set) var messages: [ChatMessage] = []
    private(set) var isSending = false

    @ObservationIgnored private let api: () -> SecretaryAPI
    @ObservationIgnored private let fileURL: URL
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var pollers: [String: Task<Void, Never>] = [:]
    @ObservationIgnored private let pollInterval: Duration
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored var onTransportSuccess: (() -> Void)?

    nonisolated static let sessionIdKey = "secretary.chat.sessionId"

    init(api: @escaping () -> SecretaryAPI, fileURL: URL = ChatStore.defaultFileURL, defaults: UserDefaults = .standard,
         pollInterval: Duration = ChatStore.pollInterval) {
        self.api = api
        self.fileURL = fileURL
        self.defaults = defaults
        self.pollInterval = pollInterval
        messages = Self.load(from: fileURL)
    }

    nonisolated static var defaultFileURL: URL {
        let base = (try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("Secretary", isDirectory: true).appendingPathComponent("chat.json")
    }

    /// Stable per install: `[A-Za-z0-9_-]{1,64}` as the server requires.
    var sessionId: String {
        if let existing = defaults.string(forKey: Self.sessionIdKey), Self.isValidSessionId(existing) { return existing }
        let fresh = "ios-" + UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(16).lowercased()
        defaults.set(fresh, forKey: Self.sessionIdKey)
        return fresh
    }

    static func isValidSessionId(_ value: String) -> Bool {
        !value.isEmpty && value.count <= 64 && value.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "_" || $0 == "-") }
    }

    /// The server accepts 1-4000 characters and no control characters other than newline and tab.
    static func clean(_ text: String) -> String {
        let normalized = text.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        let scalars = normalized.unicodeScalars.filter { scalar in
            scalar == "\n" || scalar == "\t" || !(scalar.value < 0x20 || scalar.value == 0x7F)
        }
        return String(String.UnicodeScalarView(scalars)).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: - Sending

    /// Adds an optimistic bubble and sends it. Returns false when nothing was sent.
    @discardableResult
    func send(_ text: String) async -> Bool {
        let message = Self.clean(text)
        guard !message.isEmpty, message.count <= Self.maxCharacters, !isSending else { return false }
        let id = UUID().uuidString
        messages.append(ChatMessage(id: id, role: .user, text: message, createdAt: Date(), status: .sending))
        persist()
        await deliver(id: id, text: message)
        return true
    }

    /// Resends a failed message with the same `clientMessageId`.
    func retry(_ id: String) async {
        guard let index = messages.firstIndex(where: { $0.id == id }), messages[index].role == .user,
              messages[index].status == .error, !isSending else { return }
        messages[index].status = .sending
        messages[index].error = nil
        persist()
        await deliver(id: id, text: messages[index].text)
    }

    private func deliver(id: String, text: String) async {
        isSending = true
        defer { isSending = false }
        do {
            let response = try await api().chat(ChatRequest(message: text, sessionId: sessionId, clientMessageId: id))
            onTransportSuccess?()
            guard let index = messages.firstIndex(where: { $0.id == id }) else { return }
            messages[index].status = .sent
            messages[index].error = nil
            // A retry the server already answered comes back with duplicate: true; don't add the reply twice.
            if response.duplicate, messages.contains(where: { $0.role == .lucina && $0.id == "reply-\(id)" }) {
                persist()
                return
            }
            let job = response.job.map { ChatJob(id: $0.id, status: $0.status, action: response.action) }
            let reply = ChatMessage(id: "reply-\(id)", role: .lucina, text: response.reply, createdAt: Date(),
                                    action: response.action, job: job)
            messages.insert(reply, at: index + 1)
            persist()
            if let job, !job.isTerminal { startPolling(messageId: reply.id, delay: pollInterval) }
        } catch {
            let apiError = APIError.wrap(error)
            onTransportError?(apiError)
            guard let index = messages.firstIndex(where: { $0.id == id }) else { return }
            messages[index].status = .error
            messages[index].error = Self.describe(apiError)
            persist()
        }
    }

    static func describe(_ error: APIError) -> String {
        switch error.code {
        case "lucina_unreachable": return "Lucina is offline. Not delivered."
        case "lucina_timeout": return "Lucina didn't answer in time."
        case "lucina_error": return "Lucina ran into a problem."
        case "timeout": return "No answer in time."
        default: break
        }
        if error.isConnectivity { return "Couldn't reach the server." }
        if error.status == 401 { return "You were signed out." }
        if error.status == 429 { return "Too many messages; wait a moment." }
        return error.userMessage
    }

    // MARK: - Jobs

    func resumePolling() {
        for message in messages where message.job.map({ !$0.isTerminal }) == true && pollers[message.id] == nil {
            startPolling(messageId: message.id, delay: .zero)
        }
    }

    func stopPolling() {
        pollers.values.forEach { $0.cancel() }
        pollers.removeAll()
    }

    private func startPolling(messageId: String, delay: Duration) {
        pollers[messageId]?.cancel()
        pollers[messageId] = Task { [weak self] in
            var wait = delay
            while !Task.isCancelled {
                if wait > .zero { try? await Task.sleep(for: wait) }
                guard let self, !Task.isCancelled else { return }
                let keepGoing = await self.pollOnce(messageId: messageId)
                if !keepGoing { break }
                wait = self.pollInterval
            }
            self?.pollers[messageId] = nil
        }
    }

    /// Returns whether to keep polling.
    private func pollOnce(messageId: String) async -> Bool {
        guard let jobId = messages.first(where: { $0.id == messageId })?.job?.id else { return false }
        do {
            let info = try await api().job(jobId)
            guard let index = messages.firstIndex(where: { $0.id == messageId }), var job = messages[index].job else { return false }
            job.status = info.status
            job.pollError = nil
            if let action = info.action { job.action = action }
            if let result = info.result, !result.isNull { job.result = result.displayText }
            job.error = info.error
            if let url = info.imageUrl { job.imageUrl = url }
            messages[index].job = job
            persist()
            return !job.isTerminal
        } catch {
            let apiError = APIError.wrap(error)
            guard let index = messages.firstIndex(where: { $0.id == messageId }) else { return false }
            if apiError.code == "job_not_found" || apiError.status == 404 {
                messages[index].job?.status = "error"
                messages[index].job?.pollError = "This job no longer exists."
                persist()
                return false
            }
            if apiError.endsSession {
                onTransportError?(apiError)
                return false
            }
            messages[index].job?.pollError = "Couldn't check on this job. Retrying…"
            return true
        }
    }

    // MARK: - Persistence

    func clear() {
        stopPolling()
        messages = []
        try? FileManager.default.removeItem(at: fileURL)
    }

    private func persist() {
        do {
            try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            let encoder = JSONEncoder()
            encoder.dateEncodingStrategy = .millisecondsSince1970
            // Keep the file small: the last 500 messages are plenty for the phone.
            let data = try encoder.encode(Array(messages.suffix(500)))
            try data.write(to: fileURL, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
        } catch {
            // Losing the local copy is not fatal; Lucina keeps the conversation.
        }
    }

    static func load(from url: URL) -> [ChatMessage] {
        guard let data = try? Data(contentsOf: url) else { return [] }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        guard let stored = try? decoder.decode([ChatMessage].self, from: data) else { return [] }
        return stored.map { message in
            var message = message
            if message.status == .sending {
                message.status = .error
                message.error = "Not sent: the app closed before Lucina answered."
            }
            return message
        }
    }
}
