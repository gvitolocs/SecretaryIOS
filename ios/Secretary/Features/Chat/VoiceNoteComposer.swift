import AVFoundation
import SwiftUI
import UIKit

/// The mic button's state machine, Telegram style:
/// - press and hold: records (`holding`); let go: sends;
/// - slide left past 100 pt: cancels; slide up past 80 pt: locks (`locked`, hands-free);
/// - locked: a tap on the send button sends, the trash discards;
/// - under half a second: thrown away with a "Hold to record" tip;
/// - VoiceOver: double-tap starts a locked recording, double-tap again sends;
/// - ten minutes: sends by itself.
@Observable
@MainActor
final class VoiceNoteController {
    enum Phase: Equatable { case idle, starting, holding, locked }

    private(set) var phase: Phase = .idle
    /// The finger's offset from where it went down (global points).
    private(set) var translation: CGSize = .zero
    /// "Hold to record" after a tap that was too short.
    private(set) var tip: String?
    /// A short problem under the composer (microphone refused or busy, send failed); clears itself.
    private(set) var error: String? {
        didSet {
            errorTask?.cancel()
            guard error != nil else { return }
            errorTask = Task { [weak self] in
                try? await Task.sleep(for: .seconds(6))
                if !Task.isCancelled { self?.error = nil }
            }
        }
    }
    @ObservationIgnored private var errorTask: Task<Void, Never>?

    @ObservationIgnored private weak var store: ChatStore?
    /// The finger is down on the button (the current touch began).
    @ObservationIgnored private var touching = false
    /// This touch locked the recording: lifting it must not send.
    @ObservationIgnored private var lockedThisTouch = false
    /// This touch began on the locked recording's send button.
    @ObservationIgnored private var touchOnLocked = false
    /// The finger lifted while the recorder was still starting.
    @ObservationIgnored private var releasedWhileStarting: VoiceGesture.Release?
    /// VoiceOver started it: lock as soon as the recorder runs.
    @ObservationIgnored private var lockWhenStarted = false
    @ObservationIgnored private var watchdog: Task<Void, Never>?
    @ObservationIgnored private var tipTask: Task<Void, Never>?

    var isRecording: Bool { phase != .idle }
    var recorder: MemoRecorder? { store?.noteRecorder }

    func attach(_ store: ChatStore) {
        self.store = store
    }

    // MARK: Touches

    func touchChanged(_ value: CGSize) {
        if !touching {
            touching = true
            lockedThisTouch = false
            touchOnLocked = phase == .locked
            translation = .zero
            if phase == .idle { begin(locked: false) }
            return
        }
        guard !touchOnLocked else { return }
        translation = value
        guard phase == .holding else { return }
        switch VoiceGesture.outcome(value) {
        case .cancel: cancel()
        case .lock: lock()
        case .keep: break
        }
    }

    func touchEnded(_ value: CGSize) {
        defer {
            touching = false
            touchOnLocked = false
            lockedThisTouch = false
            translation = .zero
        }
        if touchOnLocked {
            if phase == .locked, VoiceGesture.isTap(value) { send() }
            return
        }
        if lockedThisTouch { return }
        switch phase {
        case .starting:
            releasedWhileStarting = VoiceGesture.release(value)
        case .holding:
            switch VoiceGesture.release(value) {
            case .send: send()
            case .cancel: cancel()
            case .lock: lock()
            }
        case .idle, .locked:
            break
        }
    }

    /// The system took the touch away (a call, Control Center) without an end: keep the note, locked.
    func touchCancelled() {
        guard touching else { return }
        touching = false
        touchOnLocked = false
        translation = .zero
        if phase == .holding { lock() }
        if phase == .starting { lockWhenStarted = true }
    }

    // MARK: VoiceOver

    /// Double-tap: starts a locked recording; again: sends it.
    func accessibilityToggle() {
        switch phase {
        case .idle: begin(locked: true)
        case .holding, .locked: send()
        case .starting: break
        }
    }

    // MARK: Actions

    private func begin(locked: Bool) {
        guard let store, let recorder = recorder else { return }
        error = nil
        tip = nil
        switch AVAudioApplication.shared.recordPermission {
        case .undetermined:
            // First use: the system prompt takes the touch; the next press records.
            Task { _ = await AVAudioApplication.requestRecordPermission() }
            return
        case .denied:
            error = String(localized: "Secretary can't use the microphone. Allow it in Settings › Privacy › Microphone.")
            return
        default:
            break
        }
        ChatAudio.shared.stop()
        store.voiceReplies.stop()
        phase = .starting
        releasedWhileStarting = nil
        lockWhenStarted = locked
        Haptics.light()
        Task {
            await recorder.start()
            guard phase == .starting else {
                // Discarded (signed out) while starting.
                if recorder.isActive { recorder.discard() }
                return
            }
            guard recorder.isActive else {
                phase = .idle
                error = recorder.error
                return
            }
            phase = .holding
            startWatchdog()
            if lockWhenStarted {
                lock()
                lockedThisTouch = touching
                UIAccessibility.post(notification: .announcement, argument: String(localized: "Recording. Double-tap to send."))
            } else if let release = releasedWhileStarting {
                switch release {
                case .send: send()
                case .cancel: cancel()
                case .lock: lock()
                }
            } else {
                switch VoiceGesture.outcome(translation) {
                case .cancel: cancel()
                case .lock: lock()
                case .keep: break
                }
            }
        }
    }

    private func lock() {
        guard phase == .holding else { return }
        phase = .locked
        lockedThisTouch = touching
        translation = .zero
        Haptics.light()
    }

    /// Stops and sends; a note under half a second is dropped with the tip.
    func send() {
        guard phase == .holding || phase == .locked, let store, let recorder = recorder else { return }
        let levels = recorder.levelHistory
        let finished = recorder.stop()
        reset()
        guard let finished else {
            showTip()
            return
        }
        Haptics.light()
        let note = VoiceNote(url: finished.url, duration: finished.duration, levels: levels, recordedAt: finished.startedAt)
        Task {
            if !(await store.sendVoiceNote(note)) {
                try? FileManager.default.removeItem(at: note.url)
                error = String(localized: "Couldn't send this. Try again.")
            }
        }
    }

    /// Throws the recording away (slide left, or the trash when locked).
    func cancel() {
        guard phase != .idle else { return }
        if phase != .starting { recorder?.discard() }
        reset()
        Haptics.light()
    }

    private func reset() {
        phase = .idle
        translation = .zero
        releasedWhileStarting = nil
        lockWhenStarted = false
        watchdog?.cancel()
        watchdog = nil
    }

    private func showTip() {
        tip = String(localized: "Hold to record")
        tipTask?.cancel()
        tipTask = Task {
            try? await Task.sleep(for: .seconds(1.6))
            if !Task.isCancelled { tip = nil }
        }
    }

    /// Sends at the ten-minute cap; notices a recorder that stopped by itself (audio system reset).
    private func startWatchdog() {
        watchdog?.cancel()
        watchdog = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(250))
                guard let self, !Task.isCancelled, let recorder = self.recorder else { return }
                if !recorder.isActive, self.phase == .holding || self.phase == .locked {
                    self.error = recorder.error
                    self.reset()
                    return
                }
                if recorder.elapsed >= VoiceNote.maxDuration {
                    self.send()
                    return
                }
            }
        }
    }
}

enum Haptics {
    @MainActor static func light() {
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
    }
}

/// The mic in place of the send button. One view across idle, holding and locked, so the touch that
/// started a recording keeps being tracked while the composer turns into the recording bar.
struct VoiceMicButton: View {
    @Bindable var voice: VoiceNoteController
    @GestureState private var pressing = false

    private var holding: Bool { voice.phase == .holding || voice.phase == .starting }
    private var locked: Bool { voice.phase == .locked }

    var body: some View {
        ZStack {
            if locked {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 34))
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(Theme.accent)
            } else {
                Image(systemName: "mic.fill")
                    .font(.system(size: holding ? 22 : 18, weight: .semibold))
                    .foregroundStyle(holding ? Color.white : Theme.accent)
                    .frame(width: holding ? 58 : 34, height: holding ? 58 : 34)
                    .background(holding ? AnyShapeStyle(Theme.accentFill) : AnyShapeStyle(Theme.accentSoft), in: Circle())
            }
        }
        .frame(width: 38, height: 38)
        .offset(x: holding ? max(-VoiceGesture.cancelDistance, min(0, voice.translation.width)) : 0,
                y: holding ? max(-VoiceGesture.lockDistance, min(0, voice.translation.height)) : 0)
        .animation(.spring(response: 0.25, dampingFraction: 0.8), value: voice.phase)
        .overlay(alignment: .bottom) {
            if holding { lockHint.offset(y: -64) }
        }
        .overlay(alignment: .topTrailing) {
            if let tip = voice.tip {
                Text(tip)
                    .font(.caption.weight(.semibold))
                    .fixedSize()
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(.regularMaterial, in: Capsule())
                    .offset(y: -40)
                    .transition(.opacity)
                    .accessibilityIdentifier("voiceNoteTip")
            }
        }
        .contentShape(Rectangle())
        .gesture(
            DragGesture(minimumDistance: 0, coordinateSpace: .global)
                .updating($pressing) { _, state, _ in state = true }
                .onChanged { voice.touchChanged($0.translation) }
                .onEnded { voice.touchEnded($0.translation) }
        )
        .onChange(of: pressing) { _, down in
            // A touch the system cancelled resets the gesture state without calling onEnded.
            guard !down else { return }
            Task { @MainActor in voice.touchCancelled() }
        }
        .accessibilityElement()
        .accessibilityAddTraits(.isButton)
        .accessibilityLabel(Text(voice.isRecording ? "Send voice note" : "Record voice note"))
        .accessibilityHint(Text(voice.isRecording ? "Double-tap to send." : "Double-tap to start recording, double-tap again to send."))
        .accessibilityAction { voice.accessibilityToggle() }
        .accessibilityIdentifier("chatMic")
    }

    /// The lock above the button while holding: it rises with the finger and closes at the threshold.
    private var lockHint: some View {
        let progress = min(1, VoiceGesture.lockProgress(voice.translation))
        return VStack(spacing: 4) {
            Image(systemName: progress >= 1 ? "lock.fill" : "lock.open")
                .font(.system(size: 15, weight: .semibold))
            Image(systemName: "chevron.up")
                .font(.system(size: 11, weight: .bold))
                .opacity(1 - progress)
        }
        .foregroundStyle(Theme.accent)
        .frame(width: 36, height: 58)
        .background(.regularMaterial, in: Capsule())
        .offset(y: -progress * 24)
        .accessibilityHidden(true)
    }
}

/// What takes the place of the attach button and the text field while a note records: the red dot and
/// the time, the live waveform, and "‹ slide to cancel" (holding) or the trash (locked).
struct VoiceRecordingBar: View {
    @Bindable var voice: VoiceNoteController
    let recorder: MemoRecorder

    var body: some View {
        HStack(spacing: 10) {
            if voice.phase == .locked {
                Button { voice.cancel() } label: {
                    Image(systemName: "trash")
                        .font(.system(size: 19))
                        .foregroundStyle(Theme.negative)
                        .frame(width: 34, height: 38)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text("Discard voice note"))
                .accessibilityIdentifier("voiceNoteDiscard")
            }
            HStack(spacing: 6) {
                TimelineView(.periodic(from: .now, by: 0.5)) { context in
                    Circle()
                        .fill(Color.red)
                        .frame(width: 9, height: 9)
                        .opacity(recorder.state == .recording && Int(context.date.timeIntervalSinceReferenceDate * 2) % 2 == 0 ? 1 : 0.35)
                }
                Text(MemoTime.clock(recorder.elapsed))
                    .font(.body.monospacedDigit())
                    .accessibilityIdentifier("voiceNoteTimer")
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text("Recording, \(MemoTime.clock(recorder.elapsed))"))
            LiveWaveform(levels: recorder.levels, live: recorder.state == .recording)
                .frame(height: 26)
                .frame(maxWidth: voice.phase == .locked ? .infinity : 90)
            if voice.phase != .locked {
                Spacer(minLength: 0)
                Label("Slide to cancel", systemImage: "chevron.left")
                    .labelStyle(SlideHintLabelStyle())
                    .font(.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                    .opacity(VoiceGesture.cancelHintOpacity(voice.translation))
                    .offset(x: max(-VoiceGesture.cancelDistance, min(0, voice.translation.width)) * 0.5)
                    .lineLimit(1)
                    .accessibilityHidden(true)
            }
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 40)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("voiceNoteBar")
    }
}

/// "‹ Slide to cancel": the chevron before the words, smaller and tighter than a plain Label.
private struct SlideHintLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 4) {
            configuration.icon.font(.caption.weight(.semibold))
            configuration.title
        }
    }
}
