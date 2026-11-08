import AVFoundation
import Foundation
import os

/// One spoken command from the Lucina tab over the voice socket (protocol sections 2 and 10.3): the microphone streams
/// while the owner talks, the server's VAD ends the turn, the reply grows as text (`delta`/`answer`) and its audio plays
/// as it arrives. Nothing is posted to `/v1/chat`: the turn lands in the main chat through the timeline.
@MainActor
final class StreamedCommand {
    enum Phase: Equatable {
        case listening
        case thinking
        case speaking
        /// Over; the text is a problem to show (nil: it went well).
        case finished(String?)
        /// The server predates section 10: the caller uses SFSpeech and `/v1/chat`.
        case oldServer
    }

    static let log = Logger(subsystem: "tech.getsecretary.voice", category: "command")

    private let socket: VoiceSocket
    private let capture: VoiceCapture
    private let player: PCMStreamPlayer
    private let sessionId: String?
    private let speak: Bool
    private var partials: LivePartials?
    private var rate = 24_000
    private var replyText = ""
    private var heardServerTranscript = false
    private var audioStarted = false
    private var over = false
    private var timings = VoiceTimings()

    /// The words on screen: SFSpeech's partials while talking, then the server's transcript.
    var onTranscript: ((String) -> Void)?
    /// The reply as written so far.
    var onReply: ((String) -> Void)?
    var onPhase: ((Phase) -> Void)?

    init(socket: VoiceSocket, capture: VoiceCapture, player: PCMStreamPlayer, sessionId: String?, speak: Bool) {
        self.socket = socket
        self.capture = capture
        self.player = player
        self.sessionId = sessionId
        self.speak = speak
    }

    /// Opens the turn and the microphone. False when the socket is busy or the microphone did not start.
    func start() async -> Bool {
        guard socket.begin(.command(sessionId: sessionId, speak: speak), handler: { [weak self] in self?.handle($0) }) else { return false }
        let pipe = socket.pipe
        let partials = LivePartials { [weak self] text in
            Task { @MainActor in self?.partial(text) }
        }
        self.partials = partials
        let onBuffer: (@Sendable (AVAudioPCMBuffer) -> Void)? = partials.map { live in { live.append($0) } }
        let started = await capture.start(category: .playAndRecord, options: [.defaultToSpeaker, .duckOthers, .allowBluetoothHFP],
                                          releasesSession: false, onFrame: { pipe.send($0) }, onBuffer: onBuffer)
        guard started, !over else {
            if !over { socket.cancel() }
            partials?.finish()
            self.partials = nil
            return false
        }
        onPhase?(.listening)
        return true
    }

    /// A press while listening: stop here, the server runs the turn now.
    func endNow() {
        guard !over, capture.isActive else { return }
        stopMicrophone(flush: true)
        socket.end()
        timings.end()
        onPhase?(.thinking)
    }

    /// Drops the turn and its audio.
    func cancel() {
        guard !over else { return }
        socket.cancel()
        finish(nil)
    }

    private func partial(_ text: String) {
        guard !over, !heardServerTranscript else { return }
        onTranscript?(text)
    }

    private func stopMicrophone(flush: Bool) {
        partials?.finish()
        partials = nil
        if capture.isActive {
            if flush {
                if let finished = capture.stop() { try? FileManager.default.removeItem(at: finished.url) }
            } else {
                capture.discard()
            }
        }
    }

    private func finish(_ problem: String?) {
        guard !over else { return }
        over = true
        stopMicrophone(flush: false)
        player.stop()
        onPhase?(.finished(problem))
    }

    private func handle(_ event: VoiceEvent) {
        guard !over else { return }
        switch event {
        case let .listening(listening):
            if !listening.understandsStreaming {
                over = true
                stopMicrophone(flush: false)
                socket.cancel()
                onPhase?(.oldServer)
            }
        case .endpoint:
            socket.stopAudio()
            stopMicrophone(flush: false)
            timings.end()
            onPhase?(.thinking)
        case let .transcript(text):
            timings.log(.transcript, purpose: "command")
            heardServerTranscript = true
            onTranscript?(text)
        case let .delta(text):
            timings.log(.text, purpose: "command")
            replyText += text
            onReply?(replyText)
        case let .answer(text, replaces):
            timings.log(.text, purpose: "command")
            replyText = replaces || replyText.isEmpty ? text : replyText
            onReply?(replyText)
        case let .audioFormat(rate):
            self.rate = rate
        case let .audio(data):
            timings.log(.audio, purpose: "command")
            if !audioStarted {
                audioStarted = true
                onPhase?(.speaking)
            }
            player.schedule(data, rate: rate)
        case let .reply(text, _, _):
            if replyText.isEmpty {
                replyText = text
                onReply?(text)
            }
        case .done:
            Self.log.debug("command: done")
            if player.hasAudio {
                player.onDrained = { [weak self] in self?.finish(nil) }
                player.finishInput()
            } else {
                finish(nil)
            }
        case let .error(code, _, _):
            Self.log.debug("command: server error \(code, privacy: .public)")
            finish(Self.problem(code))
        case .closed:
            finish(String(localized: "The voice connection dropped."))
        case .note, .unknown:
            break
        }
    }

    nonisolated static func problem(_ code: String) -> String? {
        switch code {
        case "no_speech": return String(localized: "Lucina didn't hear anything.")
        case "busy": return String(localized: "Lucina is busy with another message.")
        case "rate_limited": return String(localized: "Too many requests. Try again in a moment.")
        case "tts_failed": return String(localized: "Lucina's voice isn't available right now.")
        default: return String(localized: "Lucina couldn't answer. Try again.")
        }
    }
}
