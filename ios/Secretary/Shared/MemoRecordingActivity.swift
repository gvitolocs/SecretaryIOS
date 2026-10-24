import ActivityKit
import AppIntents
import Foundation

// The voice-memo recording on the Lock Screen and in the Dynamic Island, like Voice Memos:
// timer, and pause/resume/stop buttons. Compiled into the app and the SecretaryWidgets
// extension. The buttons are LiveActivityIntents, which iOS runs in the app's process (the
// app is alive while it records); the app installs `MemoRecordingCommands.handle`.

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
}

struct PauseMemoRecordingIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Pause recording"
    static let isDiscoverable = false

    @MainActor
    func perform() async throws -> some IntentResult {
        MemoRecordingCommands.handle?(.pause)
        return .result()
    }
}

struct ResumeMemoRecordingIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Resume recording"
    static let isDiscoverable = false

    @MainActor
    func perform() async throws -> some IntentResult {
        MemoRecordingCommands.handle?(.resume)
        return .result()
    }
}

struct StopMemoRecordingIntent: LiveActivityIntent {
    static let title: LocalizedStringResource = "Stop and save recording"
    static let isDiscoverable = false

    @MainActor
    func perform() async throws -> some IntentResult {
        MemoRecordingCommands.handle?(.stop)
        return .result()
    }
}
