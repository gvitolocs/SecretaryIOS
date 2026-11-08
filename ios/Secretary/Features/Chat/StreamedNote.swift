import Foundation
import os

/// One voice note streamed over the voice socket (protocol section 10.4): the audio goes out while the owner talks,
/// `end` on release, and the server posts the note. The same audio is written to a local WAV, which is the pending
/// bubble and the fallback: when the socket is down, drops before `note`, or the server is old, the file goes
/// through the ordinary share upload (with the same `shareId`, so a note the server did get is not doubled).
@MainActor
final class StreamedNoteSession {
    nonisolated static let noteTimeout: Duration = .seconds(30)
    static let log = Logger(subsystem: "tech.getsecretary.voice", category: "note")

    private let store: ChatStore
    private let socket: VoiceSocket
    private(set) var turn: String?
    /// The server answered a bare `listening` (it predates section 10).
    private(set) var oldServer = false
    /// The socket let the note down: the file is sent when recording ends.
    private(set) var failed = false
    private var released: (note: VoiceNote, replyTo: String?)?
    private var shareId: String?
    private var noted = false
    private var fellBack = false
    private var timings = VoiceTimings()
    private var timeout: Task<Void, Never>?
    /// A short problem for the composer.
    var onProblem: ((String) -> Void)?
    /// The staged bubble could not be made (disk full): the controller restores the quote bar and shows an error.
    var onStageFailed: (() -> Void)?

    init(store: ChatStore, socket: VoiceSocket) {
        self.store = store
        self.socket = socket
    }

    /// Opens the turn. False when the socket is not ready (the caller uses the AAC recorder).
    func begin(replyTo: String?) -> Bool {
        let start = VoiceStart.note(sessionId: store.sessionId, replyTo: replyTo)
        return socket.begin(start) { [weak self] event in self?.handle(event) }
    }

    /// Audio is going to the server.
    var streaming: Bool { !failed && !oldServer }

    /// The owner let go: `end` goes out (after the last frame). The bubble is staged at once, or when `listening` gives
    /// its id.
    func release(note: VoiceNote, replyTo: String?) {
        released = (note, replyTo)
        if streaming {
            socket.end()
            timings.end()
        }
        if turn != nil || !streaming { stage() }
    }

    /// Recording was cancelled or too short: nothing is sent.
    func abort() {
        timeout?.cancel()
        if !noted { socket.cancel() }
        released = nil
    }

    private func stage() {
        guard let (note, replyTo) = released else { return }
        released = nil
        guard let id = store.stageStreamedNote(note, shareId: streaming ? turn : nil, replyTo: replyTo) else {
            if !noted { socket.cancel() }
            onStageFailed?()
            return
        }
        shareId = id
        if !streaming {
            fallBack()
            return
        }
        timeout = Task { [weak self] in
            try? await Task.sleep(for: Self.noteTimeout)
            guard !Task.isCancelled else { return }
            self?.fallBack()
        }
    }

    /// Sends the kept file the ordinary way.
    private func fallBack() {
        failed = true
        timeout?.cancel()
        guard !noted, !fellBack else { return }
        if let shareId {
            fellBack = true
            Self.log.debug("note: fallback upload")
            store.uploadStagedNote(shareId)
        } else if released != nil {
            stage()
        }
    }

    private func handle(_ event: VoiceEvent) {
        switch event {
        case let .listening(listening):
            guard listening.understandsStreaming else {
                // An old server would run this as a spoken command: stop it, keep recording to the file.
                oldServer = true
                socket.cancel()
                if released != nil { stage() }
                return
            }
            turn = listening.turn
            if released != nil { stage() }
        case let .transcript(text):
            timings.log(.transcript, purpose: "note")
            if let shareId { store.streamedNoteTranscript(shareId: shareId, text: text) }
        case let .note(_, userItemId):
            timings.log(.note, purpose: "note")
            noted = true
            timeout?.cancel()
            if let shareId { store.streamedNoteAccepted(shareId: shareId, userItemId: userItemId) }
        case .reply:
            timings.log(.reply, purpose: "note")
        case let .error(code, _, shareId) where shareId == nil:
            Self.log.debug("note: server error \(code, privacy: .public)")
            if code == "no_speech", let staged = self.shareId {
                timeout?.cancel()
                store.discardStagedNote(staged)
                onProblem?(String(localized: "Lucina didn't hear anything."))
            } else {
                fallBack()
            }
        case .closed:
            fallBack()
        default:
            break
        }
    }
}
