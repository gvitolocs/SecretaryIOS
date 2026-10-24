import SwiftUI

// Voice memos, like Apple's Voice Memos: a list of recordings with their status, a big red
// record button, a live waveform and timer with pause/resume, and playback. When an upload
// finishes, lucina-memo on nezopt transcribes it (Whisper), Lucina writes the summary, a
// PDF and an Obsidian note, and notifies the owner.

/// Memos open from a button next to the chat title (like Notes and Tasks).
private struct MemosEntry: ViewModifier {
    @Environment(AppModel.self) private var app

    func body(content: Content) -> some View {
        @Bindable var app = app
        content
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { app.showMemos = true } label: {
                        Image(systemName: app.memos.recorder.isActive ? "waveform.circle.fill" : "waveform")
                            .foregroundStyle(app.memos.recorder.isActive ? Color.red : Color.accentColor)
                    }
                    .accessibilityLabel("Voice memos")
                    .accessibilityIdentifier("memosButton")
                }
            }
            .navigationDestination(isPresented: $app.showMemos) { MemosView().environment(app.memos) }
    }
}

extension View {
    /// The Memos button and screen, on the chat.
    func memosEntry() -> some View { modifier(MemosEntry()) }
}

struct MemosView: View {
    @Environment(MemosStore.self) private var store
    @Environment(AppModel.self) private var app
    @State private var expanded: String?

    var body: some View {
        @Bindable var store = store
        List {
            if let error = store.loadError {
                Section {
                    ErrorBanner(message: error) { store.loadError = nil }
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
            }
            if let error = store.actionError {
                Section {
                    ErrorBanner(message: error) { store.actionError = nil }
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
            }
            let rows = store.rows
            if rows.isEmpty && store.loaded {
                Section {
                    VStack(spacing: 8) {
                        Image(systemName: "waveform").font(.largeTitle).foregroundStyle(.secondary)
                        Text("No memos yet").font(.headline)
                        Text("Tap the red button and talk. Lucina transcribes it, writes the summary, makes a PDF and puts the note in Obsidian.")
                            .font(.subheadline).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 24)
                }
            }
            ForEach(rows) { row in
                MemoRowView(row: row, expanded: expanded == row.id) {
                    withAnimation(.snappy) { expanded = expanded == row.id ? nil : row.id }
                }
                .swipeActions(edge: .trailing) {
                    Button(role: .destructive) { Task { await store.delete(row) } } label: { Label("Delete", systemImage: "trash") }
                }
            }
        }
        .navigationTitle("Memos")
        .navigationDestination(item: $store.openMemoId) { id in
            MemoDetailView(memoId: id).environment(store)
        }
        .safeAreaInset(edge: .bottom) { RecordPanel() }
        .refreshable {
            store.queue.retryNow()
            await store.refresh()
        }
        .task {
            await store.refresh()
            store.queue.kick()
            await store.pollWhileBusy()
        }
    }
}

private struct MemoRowView: View {
    @Environment(MemosStore.self) private var store
    let row: MemoRow
    let expanded: Bool
    let toggle: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Button(action: toggle) {
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(row.title).font(.headline).lineLimit(2).foregroundStyle(.primary)
                        HStack(spacing: 6) {
                            Text(row.recordedAt, format: .dateTime.day().month(.abbreviated).hour().minute())
                            PhaseLabel(phase: row.phase)
                        }
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    }
                    Spacer(minLength: 8)
                    Text(MemoTime.clock(row.duration)).font(.footnote.monospacedDigit()).foregroundStyle(.secondary)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if expanded {
                if let preview = row.preview {
                    Text(preview).font(.subheadline).foregroundStyle(.secondary).lineLimit(4)
                }
                InlinePlayer(row: row)
                if let id = row.serverId {
                    Button {
                        store.openMemoId = id
                    } label: {
                        Label(row.phase == .ready ? "Summary and transcript" : "Details", systemImage: "text.alignleft")
                            .font(.subheadline.weight(.semibold))
                    }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier("memoDetailsButton")
                }
            }
        }
        .padding(.vertical, 4)
    }
}

private struct PhaseLabel: View {
    let phase: MemoRow.Phase

    var body: some View {
        switch phase {
        case let .uploading(p):
            Label("\(Int(p * 100))%", systemImage: "icloud.and.arrow.up").labelStyle(.titleAndIcon).foregroundStyle(Color.accentColor)
        case let .waiting(reason):
            Label(reason ?? String(localized: "Upload pending"), systemImage: "clock.arrow.circlepath").lineLimit(1)
        case let .processing(status):
            HStack(spacing: 4) {
                ProgressView().controlSize(.mini)
                Text(status.label)
            }
            .foregroundStyle(Color.accentColor)
        case .ready:
            Image(systemName: "checkmark.circle.fill").foregroundStyle(.green).accessibilityLabel("Ready")
        case let .failed(reason):
            Label(reason.map { String($0.prefix(60)) } ?? String(localized: "Failed"), systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange).lineLimit(1)
        }
    }
}

/// Play, scrub and skip, under an expanded row.
struct InlinePlayer: View {
    @Environment(MemosStore.self) private var store
    let row: MemoRow
    @State private var loading = false

    var body: some View {
        let player = store.player
        let active = player.memoId == row.id
        VStack(spacing: 6) {
            Slider(value: Binding(get: { active ? player.position : 0 }, set: { player.seek($0) }),
                   in: 0...max(0.1, active ? player.duration : row.duration))
                .disabled(!active)
            HStack {
                Text(MemoTime.clock(active ? player.position : 0))
                Spacer()
                Button { player.skip(-15) } label: { Image(systemName: "gobackward.15") }.disabled(!active)
                Spacer()
                Button {
                    Task {
                        loading = true
                        await store.play(row)
                        loading = false
                    }
                } label: {
                    if loading { ProgressView() } else {
                        Image(systemName: active && player.playing ? "pause.fill" : "play.fill").font(.title2)
                    }
                }
                .accessibilityLabel(active && player.playing ? "Pause" : "Play")
                Spacer()
                Button { player.skip(15) } label: { Image(systemName: "goforward.15") }.disabled(!active)
                Spacer()
                Text("-" + MemoTime.clock(max(0, (active ? player.duration : row.duration) - (active ? player.position : 0))))
            }
            .font(.footnote.monospacedDigit())
            .buttonStyle(.borderless)
        }
    }
}

/// The bottom panel: the red button, or the live recording with waveform, timer, pause/resume.
private struct RecordPanel: View {
    @Environment(MemosStore.self) private var store
    @State private var confirmDiscard = false

    var body: some View {
        let rec = store.recorder
        VStack(spacing: 12) {
            if let error = rec.error {
                Text(error).font(.footnote).foregroundStyle(.red).multilineTextAlignment(.center)
            }
            if rec.isActive {
                LiveWaveform(levels: rec.levels, live: rec.state == .recording)
                    .frame(height: 64)
                Text(MemoTime.clock(rec.elapsed) + String(format: ".%01d", Int((rec.elapsed * 10).truncatingRemainder(dividingBy: 10))))
                    .font(.system(size: 34, weight: .light).monospacedDigit())
                    .accessibilityIdentifier("memoTimer")
                if rec.interrupted {
                    Text("Paused by another app or a call").font(.footnote).foregroundStyle(.secondary)
                }
                HStack(spacing: 44) {
                    Button { confirmDiscard = true } label: {
                        Image(systemName: "trash").font(.title3).frame(width: 52, height: 52)
                    }
                    .accessibilityLabel("Discard")
                    Button { store.finishRecording() } label: {
                        ZStack {
                            Circle().strokeBorder(.secondary, lineWidth: 3).frame(width: 72, height: 72)
                            RoundedRectangle(cornerRadius: 6).fill(.red).frame(width: 30, height: 30)
                        }
                    }
                    .accessibilityLabel("Stop and save")
                    .accessibilityIdentifier("memoStopButton")
                    Button { rec.state == .paused ? rec.resume() : rec.pause() } label: {
                        Image(systemName: rec.state == .paused ? "record.circle" : "pause.fill")
                            .font(.title2).foregroundStyle(rec.state == .paused ? .red : .primary).frame(width: 52, height: 52)
                    }
                    .accessibilityLabel(rec.state == .paused ? "Resume" : "Pause")
                    .accessibilityIdentifier("memoPauseButton")
                }
            } else {
                Button { Task { await store.record() } } label: {
                    ZStack {
                        Circle().strokeBorder(.secondary.opacity(0.6), lineWidth: 3).frame(width: 72, height: 72)
                        Circle().fill(.red).frame(width: 58, height: 58)
                    }
                }
                .accessibilityLabel("Record")
                .accessibilityIdentifier("memoRecordButton")
            }
        }
        .buttonStyle(.plain)
        .padding(.top, 14)
        .padding(.bottom, 8)
        .frame(maxWidth: .infinity)
        .background(.bar)
        .confirmationDialog("Discard this recording?", isPresented: $confirmDiscard, titleVisibility: .visible) {
            Button("Discard", role: .destructive) { store.recorder.discard() }
        }
    }
}

/// Bars of the recent levels, newest on the right, like Voice Memos.
struct LiveWaveform: View {
    let levels: [Float]
    let live: Bool

    var body: some View {
        Canvas { ctx, size in
            let bar: CGFloat = 3
            let gap: CGFloat = 2
            let count = Int(size.width / (bar + gap))
            let shown = levels.suffix(count)
            let start = CGFloat(count - shown.count) * (bar + gap)
            for (i, level) in shown.enumerated() {
                let h = max(2, CGFloat(level) * size.height)
                let rect = CGRect(x: start + CGFloat(i) * (bar + gap), y: (size.height - h) / 2, width: bar, height: h)
                ctx.fill(Path(roundedRect: rect, cornerRadius: 1.5), with: .color(live ? .red : .secondary))
            }
        }
        .accessibilityHidden(true)
    }
}
