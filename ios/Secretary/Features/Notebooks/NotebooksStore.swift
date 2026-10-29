import AVFoundation
import Foundation
import Observation

/// The notebooks list.
@Observable
@MainActor
final class NotebooksStore {
    private(set) var notebooks: [Notebook] = []
    private(set) var loaded = false
    var error: String?
    /// The server does not have Notebooks (yet): show that instead of a retry loop.
    private(set) var unavailable = false
    var showArchived = false

    @ObservationIgnored private let api: () -> NotebooksAPI
    @ObservationIgnored private var generation = 0

    init(api: @escaping () -> NotebooksAPI) {
        self.api = api
    }

    func refresh() async {
        generation += 1
        let mine = generation
        do {
            let list = try await api().list(showArchived ? .only : .exclude)
            guard mine == generation else { return }
            notebooks = list.sorted { ($0.updated ?? .distantPast) > ($1.updated ?? .distantPast) }
            loaded = true
            error = nil
            unavailable = false
        } catch is CancellationError {
            return
        } catch {
            guard mine == generation else { return }
            unavailable = NotebooksError.isUnavailable(error)
            self.error = NotebooksError.message(error)
            loaded = true
        }
    }

    @discardableResult
    func create(name: String, description: String) async -> Notebook? {
        do {
            let created = try await api().create(name: name.trimmingCharacters(in: .whitespacesAndNewlines),
                                                 description: description.trimmingCharacters(in: .whitespacesAndNewlines))
            notebooks.insert(created, at: 0)
            loaded = true
            error = nil
            unavailable = false
            return created
        } catch {
            self.error = NotebooksError.message(error)
            return nil
        }
    }

    func setArchived(_ notebook: Notebook, _ archived: Bool) async {
        do {
            _ = try await api().update(notebook.id, archived: archived)
            notebooks.removeAll { $0.id == notebook.id }
        } catch {
            self.error = NotebooksError.message(error)
        }
    }

    func delete(_ notebook: Notebook, deleteSources: Bool) async {
        do {
            try await api().delete(notebook.id, deleteSources: deleteSources)
            notebooks.removeAll { $0.id == notebook.id }
        } catch {
            self.error = NotebooksError.message(error)
        }
    }

    /// Keeps the list in step with a notebook that changed on its own screen.
    func replace(_ notebook: Notebook) {
        if let i = notebooks.firstIndex(where: { $0.id == notebook.id }) { notebooks[i] = notebook }
    }
}

/// One notebook: its sources, linked notes, chats and podcasts.
@Observable
@MainActor
final class NotebookStore {
    let id: String
    private(set) var detail: NotebookDetail
    private(set) var loaded = false
    var error: String?
    private(set) var busy = false
    /// Transformations for "Run transformation" (loaded once).
    private(set) var transformations: [Transformation] = []

    @ObservationIgnored let api: () -> NotebooksAPI

    init(notebook: Notebook, api: @escaping () -> NotebooksAPI) {
        id = notebook.id
        detail = NotebookDetail(notebook: notebook)
        self.api = api
    }

    var notebook: Notebook { detail.notebook }
    var sources: [NotebookSource] { detail.sources }
    var notes: [NotebookNote] { detail.notes }
    var chats: [NotebookChat] { detail.chats }
    var podcasts: [NotebookPodcast] { detail.podcasts }

    /// Something is still being read or made: worth asking again in a few seconds.
    var hasPendingWork: Bool { detail.sources.contains(where: \.isPending) || detail.podcasts.contains(where: \.isPending) }

    func load() async {
        do {
            let fresh = try await api().detail(id)
            detail = fresh
            loaded = true
            error = nil
        } catch is CancellationError {
            return
        } catch {
            // A background refresh that fails keeps what is on screen.
            if !loaded || self.error != nil { self.error = NotebooksError.message(error) }
            loaded = true
        }
    }

    func loadTransformations() async {
        guard transformations.isEmpty else { return }
        transformations = (try? await api().transformations()) ?? []
    }

    private func run(_ work: () async throws -> Void) async -> Bool {
        busy = true
        defer { busy = false }
        do {
            try await work()
            error = nil
            return true
        } catch is CancellationError {
            return false
        } catch {
            self.error = NotebooksError.message(error)
            return false
        }
    }

    // MARK: Notebook

    func update(name: String, description: String) async -> Bool {
        await run {
            let updated = try await api().update(id, name: name, description: description)
            detail.notebook.name = updated.name
            detail.notebook.description = updated.description
            detail.notebook.updatedAt = updated.updatedAt
        }
    }

    // MARK: Sources

    func addSource(_ source: NotebooksAPI.NewSource) async -> Bool {
        await run {
            let created = try await api().addSource(id, source)
            detail.sources.insert(created, at: 0)
            detail.notebook.counts.sources += 1
        }
    }

    func upload(data: Data, filename: String) async -> Bool {
        await run {
            let created = try await api().upload(id, data: data, filename: filename)
            detail.sources.insert(created, at: 0)
            detail.notebook.counts.sources += 1
        }
    }

    func setContext(_ source: NotebookSource, _ context: SourceContext) async {
        guard let i = detail.sources.firstIndex(where: { $0.id == source.id }) else { return }
        let before = detail.sources[i].context
        detail.sources[i].context = context.rawValue
        do {
            _ = try await api().setContext(id, source: source.id, context)
        } catch {
            if let i = detail.sources.firstIndex(where: { $0.id == source.id }) { detail.sources[i].context = before }
            self.error = NotebooksError.message(error)
        }
    }

    func retry(_ source: NotebookSource) async {
        _ = await run {
            let fresh = try await api().retry(source.id)
            if let i = detail.sources.firstIndex(where: { $0.id == source.id }) {
                var updated = fresh
                updated.context = updated.context ?? detail.sources[i].context
                detail.sources[i] = updated
            }
        }
    }

    func removeSource(_ source: NotebookSource) async {
        let ok = await run { try await api().removeSource(id, source: source.id) }
        if ok {
            detail.sources.removeAll { $0.id == source.id }
            detail.notebook.counts.sources = max(0, detail.notebook.counts.sources - 1)
        }
    }

    // MARK: Notes

    func writeNote(title: String, text: String) async -> Bool {
        await run {
            let note = try await api().writeNote(id, title: title, text: text)
            detail.notes.removeAll { $0.slug == note.slug }
            detail.notes.insert(note, at: 0)
            detail.notebook.counts.notes = detail.notes.count
        }
    }

    func linkNote(slug: String) async -> Bool {
        await run {
            let note = try await api().linkNote(id, slug: slug)
            detail.notes.removeAll { $0.slug == note.slug }
            detail.notes.insert(note, at: 0)
            detail.notebook.counts.notes = detail.notes.count
        }
    }

    func unlinkNote(_ note: NotebookNote) async {
        let ok = await run { try await api().unlinkNote(id, slug: note.slug) }
        if ok {
            detail.notes.removeAll { $0.slug == note.slug }
            detail.notebook.counts.notes = detail.notes.count
        }
    }

    /// A note saved from a chat answer or an insight shows up in the Notes tab.
    func noteAdded(_ note: NotebookNote) {
        detail.notes.removeAll { $0.slug == note.slug }
        detail.notes.insert(note, at: 0)
        detail.notebook.counts.notes = detail.notes.count
    }

    // MARK: Chats

    func createChat() async -> NotebookChat? {
        var created: NotebookChat?
        _ = await run {
            let chat = try await api().createChat(id)
            detail.chats.insert(chat, at: 0)
            detail.notebook.counts.chats = detail.chats.count
            created = chat
        }
        return created
    }

    func renameChat(_ chat: NotebookChat, title: String) async {
        _ = await run {
            let updated = try await api().renameChat(chat.id, title: title)
            if let i = detail.chats.firstIndex(where: { $0.id == chat.id }) { detail.chats[i].title = updated.title }
        }
    }

    func deleteChat(_ chat: NotebookChat) async {
        let ok = await run { try await api().deleteChat(chat.id) }
        if ok {
            detail.chats.removeAll { $0.id == chat.id }
            detail.notebook.counts.chats = detail.chats.count
        }
    }

    /// The chat answered: keep its count and title in step.
    func chatChanged(_ chat: NotebookChat) {
        if let i = detail.chats.firstIndex(where: { $0.id == chat.id }) { detail.chats[i] = chat }
    }

    // MARK: Podcasts

    func createPodcast(episode: String?, speaker: String?, briefing: String, minutes: Double?) async -> NotebookPodcast? {
        var created: NotebookPodcast?
        _ = await run {
            let podcast = try await api().createPodcast(id, episodeProfile: episode, speakerProfile: speaker, briefing: briefing, minutes: minutes)
            detail.podcasts.insert(podcast, at: 0)
            detail.notebook.counts.podcasts = detail.podcasts.count
            created = podcast
        }
        return created
    }

    func deletePodcast(_ podcast: NotebookPodcast) async {
        let ok = await run { try await api().deletePodcast(podcast.id) }
        if ok {
            detail.podcasts.removeAll { $0.id == podcast.id }
            detail.notebook.counts.podcasts = detail.podcasts.count
        }
    }
}

/// One chat: the messages, and the call that waits for the answer.
@Observable
@MainActor
final class NotebookChatStore {
    let chatId: String
    private(set) var chat: NotebookChat
    private(set) var messages: [NotebookMessage] = []
    private(set) var loaded = false
    private(set) var sending = false
    var error: String?
    /// Answers saved to Keep from this screen: message id → note.
    private(set) var savedNotes: [String: NotebookNote] = [:]
    private(set) var saving: Set<String> = []

    @ObservationIgnored private let api: () -> NotebooksAPI
    @ObservationIgnored var onNoteSaved: ((NotebookNote) -> Void)?
    @ObservationIgnored var onChatChanged: ((NotebookChat) -> Void)?

    init(chat: NotebookChat, api: @escaping () -> NotebooksAPI) {
        chatId = chat.id
        self.chat = chat
        self.api = api
    }

    func load() async {
        do {
            let detail = try await api().chat(chatId)
            guard !sending else { return }
            chat = detail.chat
            messages = detail.messages
            loaded = true
            error = nil
        } catch is CancellationError {
            return
        } catch {
            self.error = NotebooksError.message(error)
            loaded = true
        }
    }

    /// Sends a question. The question shows at once; the answer replaces the placeholder when it arrives.
    /// Returns false (and puts nothing on screen) when the call failed, so the caller can keep the draft.
    func send(_ text: String, model: NotebookModel) async -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !sending else { return false }
        let local = NotebookMessage(id: "local-\(UUID().uuidString)", role: "user", text: trimmed)
        messages.append(local)
        sending = true
        error = nil
        defer { sending = false }
        do {
            let exchange = try await api().ask(chatId, text: trimmed, model: model)
            messages.removeAll { $0.id == local.id }
            messages.append(exchange.user)
            messages.append(exchange.reply)
            chat.messageCount = messages.count
            onChatChanged?(chat)
            return true
        } catch {
            messages.removeAll { $0.id == local.id }
            self.error = NotebooksError.message(error)
            return false
        }
    }

    func saveToNote(_ message: NotebookMessage) async {
        guard !message.isUser, savedNotes[message.id] == nil, !saving.contains(message.id) else { return }
        saving.insert(message.id)
        defer { saving.remove(message.id) }
        do {
            let note = try await api().saveAnswer(chat: chatId, message: message.id)
            savedNotes[message.id] = note
            onNoteSaved?(note)
        } catch {
            self.error = NotebooksError.message(error)
        }
    }
}

/// A source's passages and insights.
@Observable
@MainActor
final class SourceStore {
    private(set) var source: NotebookSource
    private(set) var chunks: [SourceChunk] = []
    private(set) var insights: [SourceInsight] = []
    private(set) var loaded = false
    private(set) var moreChunks = false
    private(set) var running: Set<String> = []
    var error: String?

    @ObservationIgnored private let api: () -> NotebooksAPI
    @ObservationIgnored private let pageSize = 40

    init(source: NotebookSource, api: @escaping () -> NotebooksAPI) {
        self.source = source
        self.api = api
    }

    var hasPendingInsight: Bool { insights.contains(where: \.isPending) }

    func load() async {
        do {
            let api = api()
            source = try await api.source(source.id).withContext(source.context)
            if source.isReady {
                chunks = try await api.chunks(source.id, from: 0, limit: pageSize)
                moreChunks = chunks.count >= pageSize
                insights = try await api.insights(source.id)
            }
            loaded = true
            error = nil
        } catch is CancellationError {
            return
        } catch {
            self.error = NotebooksError.message(error)
            loaded = true
        }
    }

    func loadMoreChunks() async {
        guard moreChunks, let last = chunks.last else { return }
        do {
            let next = try await api().chunks(source.id, from: last.ordinal + 1, limit: pageSize)
            chunks.append(contentsOf: next.filter { new in !chunks.contains(where: { $0.id == new.id }) })
            moreChunks = next.count >= pageSize
        } catch {
            self.error = NotebooksError.message(error)
        }
    }

    func refreshInsights() async {
        if let fresh = try? await api().insights(source.id) { insights = fresh }
    }

    func run(_ transformation: Transformation) async {
        running.insert(transformation.id)
        defer { running.remove(transformation.id) }
        do {
            let insight = try await api().runTransformation(source.id, transformation: transformation.id)
            insights.removeAll { $0.id == insight.id }
            insights.insert(insight, at: 0)
            error = nil
        } catch {
            self.error = NotebooksError.message(error)
        }
    }

    func delete(_ insight: SourceInsight) async {
        do {
            try await api().deleteInsight(insight.id)
            insights.removeAll { $0.id == insight.id }
        } catch {
            self.error = NotebooksError.message(error)
        }
    }

    func saveAsNote(_ insight: SourceInsight) async -> NotebookNote? {
        do {
            return try await api().saveInsight(insight.id)
        } catch {
            self.error = NotebooksError.message(error)
            return nil
        }
    }
}

private extension NotebookSource {
    /// `GET /sources/{id}` has no `context` (it belongs to the notebook link): keep the one we know.
    func withContext(_ context: String?) -> NotebookSource {
        var copy = self
        if copy.context == nil { copy.context = context }
        return copy
    }
}

/// Plays a podcast. The audio route needs the Bearer token, which `AVPlayer` would not send, so the
/// MP3 is fetched with the app's client (as the voice replies are) and played from a file.
@Observable
@MainActor
final class PodcastPlayer {
    private(set) var loadedId: String?
    private(set) var loading = false
    private(set) var isPlaying = false
    private(set) var position: Double = 0
    private(set) var duration: Double = 0
    var rate: Float = 1 {
        didSet { if isPlaying { player?.rate = rate } }
    }
    var error: String?

    @ObservationIgnored private var player: AVPlayer?
    @ObservationIgnored private var observer: Any?
    @ObservationIgnored private var endObserver: NSObjectProtocol?
    @ObservationIgnored private var file: URL?

    func load(_ podcast: NotebookPodcast, api: NotebooksAPI) async {
        guard loadedId != podcast.id else { return }
        stop()
        loading = true
        error = nil
        defer { loading = false }
        do {
            let data = try await api.podcastAudio(podcast.id)
            let url = FileManager.default.temporaryDirectory.appendingPathComponent("podcast-\(podcast.id).mp3")
            try data.write(to: url, options: .atomic)
            file = url
            try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .spokenAudio)
            try? AVAudioSession.sharedInstance().setActive(true)
            let item = AVPlayerItem(url: url)
            let player = AVPlayer(playerItem: item)
            self.player = player
            loadedId = podcast.id
            position = 0
            duration = podcast.durationS ?? 0
            observer = player.addPeriodicTimeObserver(forInterval: CMTime(seconds: 0.5, preferredTimescale: 600), queue: .main) { [weak self] time in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.position = max(0, time.seconds)
                    if let d = self.player?.currentItem?.duration.seconds, d.isFinite, d > 0 { self.duration = d }
                }
            }
            endObserver = NotificationCenter.default.addObserver(forName: .AVPlayerItemDidPlayToEndTime, object: item, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.isPlaying = false
                    self?.player?.seek(to: .zero)
                    self?.position = 0
                }
            }
        } catch {
            self.error = NotebooksError.message(error)
        }
    }

    func toggle() {
        guard let player else { return }
        if isPlaying {
            player.pause()
            isPlaying = false
        } else {
            player.rate = rate
            isPlaying = true
        }
    }

    func seek(to seconds: Double) {
        let target = max(0, min(seconds, duration > 0 ? duration : seconds))
        position = target
        player?.seek(to: CMTime(seconds: target, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
    }

    func skip(_ delta: Double) { seek(to: position + delta) }

    func stop() {
        if let observer, let player { player.removeTimeObserver(observer) }
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        observer = nil
        endObserver = nil
        player?.pause()
        player = nil
        isPlaying = false
        loadedId = nil
        position = 0
        if let file { try? FileManager.default.removeItem(at: file) }
        file = nil
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }
}
