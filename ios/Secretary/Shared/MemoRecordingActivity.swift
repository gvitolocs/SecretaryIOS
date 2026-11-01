import ActivityKit
import AppIntents
import Foundation

// The voice-memo recording on the Lock Screen and in the Dynamic Island, like Voice Memos:
// timer, and pause/resume/stop buttons. Compiled into the app and the SecretaryWidgets
// extension. The buttons are LiveActivityIntents, which iOS runs in the app's process (the
// app is alive while it records); the app installs `MemoRecordingCommands.handle`.
// `StartMemoRecordingIntent` (the Control Center / Lock Screen control, the Action button,
// Shortcuts) is an AudioRecordingIntent and a LiveActivityIntent. The second is what makes iOS
// run it in the app's process (launched in the background, also with the phone locked): an
// AudioRecordingIntent alone runs in the widget extension, where there is no recorder and the
// control did nothing (2026-10-26). The recording must show its Live Activity.

struct MemoRecordingAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        var paused: Bool
        /// Recorded seconds when this state was sent.
        var elapsed: Double
        /// When that was (the running timer counts on from it).
        var at: Date
        /// True when a call or another app paused it.
        var interrupted: Bool = false

        /// The date a running `Text(timerInterval:)` starts from.
        var timerStart: Date { at.addingTimeInterval(-elapsed) }
    }

    var startedAt: Date
}

enum MemoRecordingCommand: String, Sendable {
    case pause, resume, stop
}

enum MemoRecordingCommands {
    /// Set by the app at launch; nil in the widget extension (never runs the intents).
    @MainActor static var handle: ((MemoRecordingCommand) -> Void)?
    /// Starts a recording (nil in the widget extension). Throws a message for the owner.
    @MainActor static var start: (() async throws -> Void)?
}

struct StartMemoRecordingIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Record a memo with Lucina"
    static let description = IntentDescription("Starts recording a voice memo at once, also from the Lock Screen. Lucina transcribes and summarises it.")
    /// Like Voice Memos: no unlock needed to start a recording.
    static let authenticationPolicy: IntentAuthenticationPolicy = .alwaysAllowed

    @MainActor
    func perform() async throws -> some IntentResult {
        guard let start = MemoRecordingCommands.start else {
            throw MemoRecordingCommandError.notInApp
        }
        try await start()
        return .result()
    }
}

enum MemoRecordingCommandError: Error, CustomLocalizedStringResourceConvertible {
    case notInApp

    var localizedStringResource: LocalizedStringResource { "Open Secretary once, then try again." }
}

@available(iOS 18.0, *)
extension StartMemoRecordingIntent: AudioRecordingIntent {}

struct PauseMemoRecordingIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Pause recording"

    @MainActor
    func perform() async throws -> some IntentResult {
        MemoRecordingCommands.handle?(.pause)
        return .result()
    }
}

struct ResumeMemoRecordingIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Resume recording"

    @MainActor
    func perform() async throws -> some IntentResult {
        MemoRecordingCommands.handle?(.resume)
        return .result()
    }
}

struct StopMemoRecordingIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Stop and save recording"

    @MainActor
    func perform() async throws -> some IntentResult {
        MemoRecordingCommands.handle?(.stop)
        return .result()
    }
}
