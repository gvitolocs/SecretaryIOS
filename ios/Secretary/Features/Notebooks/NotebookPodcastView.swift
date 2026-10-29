import SwiftUI

/// Podcasts made from the notebook: a list with the status of each, a form to make one, and a player
/// with the transcript.
struct PodcastsSheet: View {
    @Environment(\.dismiss) private var dismiss
    let store: NotebookStore
    @State private var player = PodcastPlayer()
    @State private var creating = false
    @State private var pendingDelete: NotebookPodcast?

    var body: some View {
        NavigationStack {
            List {
                if let error = store.error {
                    Section { ErrorBanner(message: error) { store.error = nil } }
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
                if store.podcasts.isEmpty {
                    Section {
                        EmptyCard(icon: "waveform", title: "No podcasts yet",
                                  detail: "Lucina turns the notebook into a spoken conversation you can listen to, with a transcript.")
                        PrimaryButton(title: "New podcast", icon: "plus") { creating = true }
                    }
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
                } else {
                    Section {
                        ForEach(store.podcasts) { podcast in
                            NavigationLink {
                                PodcastDetailView(podcast: podcast, store: store, player: player)
                            } label: {
                                PodcastRow(podcast: podcast)
                            }
                            .swipeActions {
                                Button("Delete", systemImage: "trash", role: .destructive) { pendingDelete = podcast }
                            }
                        }
                    }
                }
            }
            .listStyle(.insetGrouped)
            .refreshable { await store.load() }
            .navigationTitle("Podcasts")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                ToolbarItem(placement: .primaryAction) {
                    Button { creating = true } label: { Image(systemName: "plus") }
                        .accessibilityLabel("New podcast")
                        .accessibilityIdentifier("newPodcastButton")
                }
            }
            .navigationDestination(isPresented: $creating) {
                NewPodcastView(store: store) { creating = false }
            }
            .confirmationDialog(pendingDelete.map { "Delete “\($0.displayTitle)”?" } ?? "",
                                isPresented: Binding(get: { pendingDelete != nil }, set: { if !$0 { pendingDelete = nil } }),
                                titleVisibility: .visible, presenting: pendingDelete) { podcast in
                Button("Delete podcast", role: .destructive) {
                    Task {
                        if player.loadedId == podcast.id { player.stop() }
                        await store.deletePodcast(podcast)
                    }
                }
                Button("Cancel", role: .cancel) {}
            }
            .onAppear { store.error = nil }
        }
        .onDisappear { player.stop() }
        .task {
            // The sheet can open while a podcast is still being made: keep its status fresh.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(4))
                if store.podcasts.contains(where: \.isPending) { await store.load() }
            }
        }
    }
}

private struct PodcastRow: View {
    let podcast: NotebookPodcast

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: podcast.isReady ? "play.circle.fill" : podcast.isFailed ? "exclamationmark.triangle.fill" : "waveform")
                .font(.title2)
                .foregroundStyle(podcast.isFailed ? Color.red : Theme.accent)
                .frame(width: 34)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(podcast.displayTitle).font(.headline).lineLimit(2)
                HStack(spacing: 6) {
                    if podcast.isPending { ProgressView().controlSize(.mini) }
                    Text(podcast.statusLabel)
                    if let d = podcast.durationS, d > 0 { Text("· \(NotebookTime.clock(d))") }
                    if let date = NoteDates.parse(podcast.createdAt) { Text("· \(date.formatted(date: .abbreviated, time: .omitted))") }
                }
                .font(.caption)
                .foregroundStyle(podcast.isFailed ? Color.red : Color.secondary)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}

// MARK: - New podcast

private struct NewPodcastView: View {
    let store: NotebookStore
    let done: () -> Void
    @State private var profiles = PodcastProfiles()
    @State private var loadedProfiles = false
    @State private var episode = ""
    @State private var speaker = ""
    @State private var briefing = ""
    @State private var minutes = 8.0
    @State private var busy = false

    var body: some View {
        Form {
            Section {
                if profiles.episodes.isEmpty && loadedProfiles {
                    Text("Lucina will pick the format.").foregroundStyle(.secondary)
                } else {
                    Picker("Format", selection: $episode) {
                        ForEach(profiles.episodes) { Text($0.name).tag($0.id) }
                    }
                    if let current = profiles.episodes.first(where: { $0.id == episode }), !current.description.isEmpty {
                        Text(current.description).font(.footnote).foregroundStyle(.secondary)
                    }
                }
                if !profiles.speakers.isEmpty {
                    Picker("Voices", selection: $speaker) {
                        ForEach(profiles.speakers) { profile in
                            Text(profile.language.isEmpty ? profile.name : "\(profile.name)").tag(profile.id)
                        }
                    }
                }
            }
            Section {
                TextField("What should it focus on? (optional)", text: $briefing, axis: .vertical).lineLimit(3...8)
            } header: { Text("Briefing") } footer: {
                Text("Leave it empty to use the format's default.")
            }
            Section {
                Stepper(value: $minutes, in: 2...30, step: 1) {
                    LabeledContent("Length", value: "\(Int(minutes)) min")
                }
            }
            if let error = store.error {
                Section { ErrorBanner(message: error) { store.error = nil }.listRowInsets(EdgeInsets()).listRowBackground(Color.clear) }
            }
            Section {
                PrimaryButton(title: "Make podcast", icon: "waveform", isBusy: busy) { Task { await create() } }
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
                    .accessibilityIdentifier("makePodcast")
            } footer: {
                Text("It takes a few minutes. You can close this and come back.")
            }
        }
        .navigationTitle("New podcast")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            store.error = nil
            if let loaded = try? await store.api().profiles() {
                profiles = loaded
                if episode.isEmpty { episode = loaded.episodes.first?.id ?? "" }
                if speaker.isEmpty {
                    speaker = loaded.episodes.first(where: { $0.id == episode })?.speakerProfile ?? loaded.speakers.first?.id ?? ""
                }
            }
            loadedProfiles = true
        }
        .onChange(of: episode) { _, id in
            // The format suggests its voices; the choice stays the owner's afterwards.
            if let suggested = profiles.episodes.first(where: { $0.id == id })?.speakerProfile, !suggested.isEmpty,
               profiles.speakers.contains(where: { $0.id == suggested }) { speaker = suggested }
        }
    }

    private func create() async {
        busy = true
        defer { busy = false }
        let created = await store.createPodcast(episode: episode.isEmpty ? nil : episode, speaker: speaker.isEmpty ? nil : speaker,
                                                briefing: briefing.trimmingCharacters(in: .whitespacesAndNewlines), minutes: minutes)
        if created != nil { done() }
    }
}

// MARK: - Player and transcript

private struct PodcastDetailView: View {
    let store: NotebookStore
    let player: PodcastPlayer
    @State private var podcast: NotebookPodcast
    @State private var showTranscript = true

    init(podcast: NotebookPodcast, store: NotebookStore, player: PodcastPlayer) {
        _podcast = State(initialValue: podcast)
        self.store = store
        self.player = player
    }

    var body: some View {
        List {
            Section {
                VStack(alignment: .leading, spacing: 6) {
                    Text(podcast.displayTitle).font(.title3.bold())
                    HStack(spacing: 6) {
                        if podcast.isPending { ProgressView().controlSize(.mini) }
                        Text(podcast.statusLabel)
                        if let d = podcast.durationS, d > 0 { Text("· \(NotebookTime.clock(d))") }
                    }
                    .font(.subheadline).foregroundStyle(.secondary)
                    if podcast.isFailed, let error = podcast.error, !error.isEmpty {
                        Text(error).font(.footnote).foregroundStyle(.red)
                    }
                    if !podcast.briefing.isEmpty {
                        Text(podcast.briefing).font(.footnote).foregroundStyle(.secondary).lineLimit(4)
                    }
                }
                if podcast.isReady && podcast.audio {
                    PlayerControls(podcast: podcast, player: player, api: store.api)
                }
            }
            if !podcast.outline.isEmpty && !podcast.isReady {
                Section("Outline") {
                    ForEach(Array(podcast.outline.enumerated()), id: \.offset) { _, item in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(item.name).font(.subheadline.weight(.semibold))
                            if !item.description.isEmpty { Text(item.description).font(.footnote).foregroundStyle(.secondary) }
                        }
                    }
                }
            }
            if !podcast.transcript.isEmpty {
                Section {
                    DisclosureGroup("Transcript", isExpanded: $showTranscript) {
                        ForEach(podcast.transcript) { line in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(line.speaker).font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
                                Text(line.text).font(.subheadline).textSelection(.enabled)
                            }
                            .padding(.vertical, 2)
                        }
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle("Podcast")
        .navigationBarTitleDisplayMode(.inline)
        .task(id: podcast.status) {
            // Fetch the full podcast (outline and transcript are on the single route) and follow it until it is done.
            while !Task.isCancelled {
                if let fresh = try? await store.api().podcast(podcast.id) { podcast = fresh }
                guard podcast.isPending else { return }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }
}

private struct PlayerControls: View {
    let podcast: NotebookPodcast
    let player: PodcastPlayer
    let api: () -> NotebooksAPI
    @State private var scrubbing: Double?

    var body: some View {
        VStack(spacing: 10) {
            if player.loadedId != podcast.id {
                Button {
                    Task { await player.load(podcast, api: api()); if player.error == nil { player.toggle() } }
                } label: {
                    HStack {
                        if player.loading { ProgressView() } else { Image(systemName: "play.fill") }
                        Text(player.loading ? "Loading audio…" : "Play")
                    }
                    .font(.headline).frame(maxWidth: .infinity).padding(.vertical, 8)
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accentFill)
                .disabled(player.loading)
                .accessibilityIdentifier("playPodcast")
            } else {
                Slider(value: Binding(get: { scrubbing ?? player.position }, set: { scrubbing = $0 }),
                       in: 0...max(player.duration, 1)) { editing in
                    if !editing, let target = scrubbing { player.seek(to: target); scrubbing = nil }
                }
                .tint(Theme.accentFill)
                HStack {
                    Text(NotebookTime.clock(scrubbing ?? player.position)).monospacedDigit()
                    Spacer()
                    Text(NotebookTime.clock(player.duration)).monospacedDigit()
                }
                .font(.caption).foregroundStyle(.secondary)
                HStack(spacing: 28) {
                    Menu {
                        ForEach([Float(0.8), 1, 1.25, 1.5, 2], id: \.self) { rate in
                            Button {
                                player.rate = rate
                            } label: {
                                if player.rate == rate { Label("\(rate, format: .number)×", systemImage: "checkmark") } else { Text("\(rate, format: .number)×") }
                            }
                        }
                    } label: {
                        Text("\(player.rate, format: .number)×").font(.subheadline.weight(.semibold)).frame(minWidth: 44)
                    }
                    Button { player.skip(-15) } label: { Image(systemName: "gobackward.15").font(.title2) }
                        .accessibilityLabel("Back 15 seconds")
                    Button { player.toggle() } label: {
                        Image(systemName: player.isPlaying ? "pause.circle.fill" : "play.circle.fill").font(.system(size: 46))
                    }
                    .accessibilityLabel(player.isPlaying ? "Pause" : "Play")
                    Button { player.skip(30) } label: { Image(systemName: "goforward.30").font(.title2) }
                        .accessibilityLabel("Forward 30 seconds")
                    Color.clear.frame(width: 44, height: 1)
                }
                .buttonStyle(.borderless)
                .foregroundStyle(Theme.accent)
            }
            if let error = player.error {
                Text(error).font(.footnote).foregroundStyle(.red)
            }
        }
        .padding(.vertical, 4)
    }
}
