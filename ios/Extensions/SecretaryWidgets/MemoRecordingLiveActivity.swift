import ActivityKit
import AppIntents
import SwiftUI
import WidgetKit

@main
struct SecretaryWidgets: WidgetBundle {
    var body: some Widget {
        MemoRecordingLiveActivity()
    }
}

/// A voice memo being recorded: Lock Screen banner and Dynamic Island.
struct MemoRecordingLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: MemoRecordingAttributes.self) { context in
            LockScreenView(state: context.state)
                .activityBackgroundTint(Color.black.opacity(0.75))
                .activitySystemActionForegroundColor(.white)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Label(context.state.paused ? "Paused" : "Recording", systemImage: "waveform")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(context.state.paused ? .orange : .red)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    ElapsedText(state: context.state).font(.title3.monospacedDigit()).foregroundStyle(.white)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    Controls(state: context.state)
                }
            } compactLeading: {
                Image(systemName: "waveform").foregroundStyle(context.state.paused ? .orange : .red)
            } compactTrailing: {
                ElapsedText(state: context.state).monospacedDigit().frame(maxWidth: 52)
            } minimal: {
                Image(systemName: context.state.paused ? "pause.fill" : "mic.fill").foregroundStyle(.red)
            }
        }
    }
}

private struct ElapsedText: View {
    let state: MemoRecordingAttributes.ContentState

    var body: some View {
        if state.paused {
            Text(Self.clock(state.elapsed))
        } else {
            Text(timerInterval: state.timerStart...Date.distantFuture, countsDown: false)
        }
    }

    static func clock(_ s: Double) -> String {
        let t = Int(s)
        return t >= 3600 ? String(format: "%d:%02d:%02d", t / 3600, (t / 60) % 60, t % 60) : String(format: "%d:%02d", t / 60, t % 60)
    }
}

private struct Controls: View {
    let state: MemoRecordingAttributes.ContentState

    var body: some View {
        HStack(spacing: 14) {
            if state.paused {
                Button(intent: ResumeMemoRecordingIntent()) {
                    Label("Resume", systemImage: "record.circle").frame(maxWidth: .infinity)
                }
                .tint(.red)
            } else {
                Button(intent: PauseMemoRecordingIntent()) {
                    Label("Pause", systemImage: "pause.fill").frame(maxWidth: .infinity)
                }
                .tint(.gray)
            }
            Button(intent: StopMemoRecordingIntent()) {
                Label("Done", systemImage: "stop.fill").frame(maxWidth: .infinity)
            }
            .tint(.red)
        }
        .buttonStyle(.borderedProminent)
        .font(.subheadline.weight(.semibold))
    }
}

private struct LockScreenView: View {
    let state: MemoRecordingAttributes.ContentState

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Image(systemName: state.paused ? "pause.circle.fill" : "record.circle")
                    .foregroundStyle(state.paused ? .orange : .red)
                    .font(.title2)
                VStack(alignment: .leading, spacing: 1) {
                    Text(state.paused ? (state.interrupted ? "Paused by a call" : "Paused") : "Recording memo")
                        .font(.headline).foregroundStyle(.white)
                    Text("Secretary · Lucina").font(.caption).foregroundStyle(.white.opacity(0.7))
                }
                Spacer()
                ElapsedText(state: state).font(.title.monospacedDigit().weight(.light)).foregroundStyle(.white)
            }
            Controls(state: state)
        }
        .padding(14)
    }
}
