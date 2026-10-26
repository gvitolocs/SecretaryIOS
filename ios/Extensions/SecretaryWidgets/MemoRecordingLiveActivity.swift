import ActivityKit
import AppIntents
import SwiftUI
import WidgetKit

@main
struct SecretaryWidgets: WidgetBundle {
    var body: some Widget {
        MemoRecordingLiveActivity()
        if #available(iOS 18.0, *) {
            MemoRecordControl()
        }
    }
}

/// A voice memo being recorded: Lock Screen banner and Dynamic Island. Laid out like an
/// incoming call: the title and the running timer on the left, two big round buttons (pause
/// or resume, stop) side by side on the right.
struct MemoRecordingLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: MemoRecordingAttributes.self) { context in
            LockScreenView(state: context.state)
                .activityBackgroundTint(LucinaTokens.Colors.card.opacity(0.92))
                .activitySystemActionForegroundColor(LucinaTokens.Colors.text)
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Label(context.state.paused ? "Paused" : "Recording", systemImage: "waveform")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(context.state.paused ? LucinaTokens.Colors.warning : LucinaTokens.Colors.negative)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    HStack {
                        ElapsedText(state: context.state).font(.title2.monospacedDigit().weight(.light)).foregroundStyle(LucinaTokens.Colors.text)
                        Spacer()
                        Controls(state: context.state, size: 50)
                    }
                }
            } compactLeading: {
                Image(systemName: "waveform").foregroundStyle(context.state.paused ? LucinaTokens.Colors.warning : LucinaTokens.Colors.negative)
            } compactTrailing: {
                ElapsedText(state: context.state).monospacedDigit().frame(maxWidth: 52)
            } minimal: {
                Image(systemName: context.state.paused ? "pause.fill" : "mic.fill").foregroundStyle(LucinaTokens.Colors.negative)
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

/// Pause (or resume) and stop, as round call-style buttons.
private struct Controls: View {
    let state: MemoRecordingAttributes.ContentState
    var size: CGFloat = 56

    var body: some View {
        HStack(spacing: 12) {
            if state.paused {
                RoundButton(intent: ResumeMemoRecordingIntent(), systemImage: "mic.fill", label: "Resume",
                            color: LucinaTokens.Colors.accentFill, size: size)
            } else {
                RoundButton(intent: PauseMemoRecordingIntent(), systemImage: "pause.fill", label: "Pause",
                            color: LucinaTokens.Colors.accentFill, size: size)
            }
            RoundButton(intent: StopMemoRecordingIntent(), systemImage: "stop.fill", label: "Done",
                        color: LucinaTokens.Colors.negative, size: size)
        }
    }
}

private struct RoundButton<Intent: AppIntent>: View {
    let intent: Intent
    let systemImage: String
    let label: LocalizedStringKey
    let color: Color
    let size: CGFloat

    var body: some View {
        Button(intent: intent) {
            Image(systemName: systemImage)
                .font(.system(size: size * 0.4, weight: .semibold))
                .foregroundStyle(LucinaTokens.Colors.onAccent)
                .frame(width: size, height: size)
                .background(Circle().fill(color))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(label))
    }
}

private struct LockScreenView: View {
    let state: MemoRecordingAttributes.ContentState

    var body: some View {
        HStack(spacing: 14) {
            VStack(alignment: .leading, spacing: 2) {
                Text(state.paused ? (state.interrupted ? "Paused by a call" : "Paused") : "Recording memo")
                    .font(.headline).foregroundStyle(LucinaTokens.Colors.text)
                    .lineLimit(1)
                ElapsedText(state: state).font(.title2.monospacedDigit().weight(.light)).foregroundStyle(LucinaTokens.Colors.text)
                Text("Secretary · Lucina").font(.caption).foregroundStyle(LucinaTokens.Colors.textSecondary)
            }
            .layoutPriority(1)
            Spacer(minLength: 4)
            Controls(state: state)
        }
        .padding(16)
    }
}
