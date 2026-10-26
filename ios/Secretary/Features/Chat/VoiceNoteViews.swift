import SwiftUI

/// A voice note, Telegram style: play/pause, the waveform filling as it plays (tap it to jump), the time,
/// the speed (1× → 1.5× → 2×), and the words under it (three lines; tap for all).
struct VoiceNoteBubble: View {
    @Environment(AppModel.self) private var app
    @Environment(ChatStore.self) private var store
    let item: TimelineItem
    let entry: BubbleEntry
    let shareId: String?
    let isUser: Bool
    @State private var expanded = false

    private var audio: ChatAudio { ChatAudio.shared }
    private var key: String { entry.attachment.mediaId ?? "\(shareId ?? "")#\(entry.fileIndex ?? 0)" }
    private var active: Bool { audio.currentKey == key }
    private var playing: Bool { active && audio.isPlaying }
    private var foreground: Color { isUser ? Theme.onAccent : Theme.text }
    private var tint: Color { isUser ? Theme.onAccent : Theme.accent }
    private var transcript: String? { store.voiceTranscript(for: item, attachment: entry.attachment, fileIndex: entry.fileIndex) }

    /// The attachment's waveform; a flat line when the server didn't send one.
    private var samples: [Float] {
        VoiceWaveform.decode(entry.attachment.waveform) ?? Array(repeating: 0.08, count: VoiceWaveform.buckets)
    }

    private var total: Double {
        if active, audio.duration > 0 { return audio.duration }
        return Double(entry.attachment.durationMs ?? 0) / 1000
    }

    private var progress: Double {
        guard active, total > 0 else { return 0 }
        return min(1, audio.position / total)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Button { audio.toggle(key, resolve: resolve) } label: {
                    ZStack {
                        Circle().fill(isUser ? Color.white.opacity(0.22) : Theme.accentSoft)
                        if audio.loadingKey == key {
                            ProgressView().tint(foreground)
                        } else {
                            Image(systemName: playing ? "pause.fill" : "play.fill")
                                .font(.system(size: 17, weight: .semibold))
                                .foregroundStyle(tint)
                        }
                    }
                    .frame(width: 42, height: 42)
                }
                .buttonStyle(.plain)
                .disabled(!playable)
                .accessibilityLabel(Text(playing ? "Pause" : "Play"))
                .accessibilityIdentifier("voiceNotePlay")
                VStack(alignment: .leading, spacing: 4) {
                    VoiceWaveformBars(samples: samples, progress: progress, played: tint, pending: foreground.opacity(0.35))
                        .frame(height: 26)
                        .overlay {
                            GeometryReader { geometry in
                                Color.clear.contentShape(Rectangle())
                                    .onTapGesture(coordinateSpace: .local) { point in
                                        if active { audio.seek(key, fraction: point.x / max(1, geometry.size.width)) }
                                        else { audio.toggle(key, resolve: resolve) }
                                    }
                            }
                        }
                        .accessibilityHidden(true)
                    HStack(spacing: 6) {
                        Text(timeText).monospacedDigit()
                        if item.localStatus == .sending, let shareId, let fraction = store.shareProgress[shareId] {
                            ProgressView(value: fraction).progressViewStyle(.circular).controlSize(.mini).tint(foreground)
                        }
                        Spacer(minLength: 4)
                        Button { audio.cycleRate() } label: {
                            Text(Self.rateLabel(audio.rate))
                                .font(.caption2.weight(.bold).monospacedDigit())
                                .padding(.horizontal, 7)
                                .padding(.vertical, 2)
                                .background(foreground.opacity(0.15), in: Capsule())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel(Text("Playback speed"))
                        .accessibilityValue(Text(Self.rateLabel(audio.rate)))
                        .accessibilityIdentifier("voiceNoteSpeed")
                    }
                    .font(.caption)
                    .foregroundStyle(foreground.opacity(0.85))
                }
            }
            if let transcript {
                Text(transcript)
                    .font(.footnote)
                    .foregroundStyle(foreground.opacity(0.75))
                    .lineLimit(expanded ? nil : 3)
                    .multilineTextAlignment(.leading)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                    .onTapGesture { withAnimation(.easeOut(duration: 0.2)) { expanded.toggle() } }
                    .textSelection(.enabled)
                    .accessibilityLabel(Text("Transcript: \(transcript)"))
                    .accessibilityHint(Text(expanded ? "Double-tap to show less" : "Double-tap to show all"))
                    .accessibilityIdentifier("voiceNoteTranscript")
            }
            if active, let notice = audio.notice {
                Text(notice).font(.caption2).foregroundStyle(foreground.opacity(0.8))
            }
        }
        .padding(10)
        .frame(width: 262)
        .background(isUser ? Theme.accentFill : Theme.card, in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
        .opacity(item.localStatus == .sending ? 0.85 : 1)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Voice note, \(MediaFormat.time(total))"))
        .accessibilityIdentifier("voiceNote")
    }

    static func rateLabel(_ rate: Float) -> String {
        rate == 1.5 ? "1.5×" : "\(Int(rate))×"
    }

    private var timeText: String {
        if active, audio.position > 0 { return MediaFormat.time(audio.position) }
        return total > 0 ? MediaFormat.time(total) : "--:--"
    }

    private var playable: Bool {
        entry.attachment.mediaId != nil || store.localFile(shareId: shareId, fileIndex: entry.fileIndex ?? -1) != nil
    }

    private func resolve() async -> URL? {
        if let local = store.localFile(shareId: shareId, fileIndex: entry.fileIndex ?? -1) { return local }
        guard let id = entry.attachment.mediaId else { return nil }
        return await ChatMedia.playURL(id, client: app.client)
    }
}

/// Vertical bars from 0...1 samples (resampled to the width); the played part in `played`.
struct VoiceWaveformBars: View {
    let samples: [Float]
    var progress: Double = 0
    var played: Color
    var pending: Color

    var body: some View {
        Canvas { context, size in
            let bar: CGFloat = 3
            let gap: CGFloat = 2
            let count = max(1, Int((size.width + gap) / (bar + gap)))
            let shown = VoiceWaveform.resample(Self.stretch(samples, to: count), count: count)
            let filled = Int((progress * Double(shown.count)).rounded(.down))
            for (index, level) in shown.enumerated() {
                let height = max(3, CGFloat(level) * size.height)
                let rect = CGRect(x: CGFloat(index) * (bar + gap), y: (size.height - height) / 2, width: bar, height: height)
                context.fill(Path(roundedRect: rect, cornerRadius: 1.5), with: .color(index < filled ? played : pending))
            }
        }
    }

    /// Fewer samples than bars: each sample repeated so the bars still span the width.
    static func stretch(_ samples: [Float], to count: Int) -> [Float] {
        guard !samples.isEmpty, samples.count < count else { return samples }
        return (0..<count).map { samples[$0 * samples.count / count] }
    }
}

/// The small speaker under a reply Lucina wanted read aloud: plays her voice again (or stops it).
struct VoiceReplyButton: View {
    @Environment(AppModel.self) private var app
    @Environment(ChatStore.self) private var store
    let item: TimelineItem

    private var replies: LucinaVoiceReplies { store.voiceReplies }

    var body: some View {
        Button { replies.play(item, api: app.api) } label: {
            Group {
                if replies.loadingId == item.id {
                    ProgressView().controlSize(.mini)
                } else {
                    Image(systemName: replies.playingId == item.id ? "speaker.wave.2.fill" : "speaker.wave.2")
                        .symbolEffect(.variableColor.iterative, isActive: replies.playingId == item.id)
                }
            }
            .font(.caption)
            .foregroundStyle(Theme.accent)
            .frame(width: 28, height: 22)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(replies.playingId == item.id ? "Stop Lucina's voice" : "Play Lucina's voice"))
        .accessibilityIdentifier("voiceReplyPlay")
    }
}
