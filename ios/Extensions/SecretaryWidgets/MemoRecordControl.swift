import AppIntents
import SwiftUI
import WidgetKit

/// The Control Center / Lock Screen / Action button control: one tap starts a memo, like
/// Voice Memos' own control. The intent runs in the app's process (see MemoRecordingActivity).
@available(iOS 18.0, *)
struct MemoRecordControl: ControlWidget {
    var body: some ControlWidgetConfiguration {
        StaticControlConfiguration(kind: "tech.getsecretary.app.memo-record") {
            ControlWidgetButton(action: StartMemoRecordingIntent()) {
                Label("Lucina memo", systemImage: "waveform")
            }
        }
        .displayName("Record a memo with Lucina")
        .description("Starts a voice memo at once. Lucina transcribes and summarises it.")
    }
}
