import Foundation

// Notebooks (docs/design/notebooks-api.md): a NotebookLM-style workspace on Lucina. A notebook groups
// sources, linked Keep notes and source-grounded chats. Served by p1 at /v1/notebooks/...
// The wire format is snake_case; every field is decoded leniently (unknown fields are ignored and a
// missing optional one never fails the whole list). Ported from Open Notebook by Luis Novo (MIT).

private extension KeyedDecodingContainer {
    /// An `Int` that may arrive as a number or as an array whose length is the count.
    func countOrArray(_ key: Key) -> Int? {
        if let n = lenientOptional(Int.self, key) { return n }
        if let items = try? nestedUnkeyedContainer(forKey: key) { return items.count ?? 0 }
        return nil
    }
}

/// Decodes a list one entry at a time: one bad entry must not hide the others.
struct LenientList<T: Decodable>: Decodable {
    let items: [T]

    init(items: [T]) { self.items = items }

    init(from decoder: Decoder) throws {
        var out: [T] = []
        var container = try decoder.unkeyedContainer()
        while !container.isAtEnd {
            if let item = try? container.decode(T.self) { out.append(item) } else { _ = try? container.decode(JSONValue.self) }
        }
        items = out
    }
}

extension KeyedDecodingContainer {
    func lenientList<T: Decodable>(_ type: T.Type, _ key: Key) -> [T] {
        ((try? decodeIfPresent(LenientList<T>.self, forKey: key)) ?? nil)?.items ?? []
    }
}

// MARK: - Notebook

struct NotebookCounts: Decodable, Equatable, Hashable, Sendable {
    var sources = 0
    var notes = 0
    var chats = 0
    var podcasts = 0

    init(sources: Int = 0, notes: Int = 0, chats: Int = 0, podcasts: Int = 0) {
        self.sources = sources
        self.notes = notes
        self.chats = chats
        self.podcasts = podcasts
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sources = c.lenientOptional(Int.self, .sources) ?? 0
        notes = c.lenientOptional(Int.self, .notes) ?? 0
        chats = c.lenientOptional(Int.self, .chats) ?? 0
        podcasts = c.lenientOptional(Int.self, .podcasts) ?? 0
    }

    private enum CodingKeys: String, CodingKey { case sources, notes, chats, podcasts }
}

struct Notebook: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    var name: String
    var description: String
    var archived: Bool
    let createdAt: String
    var updatedAt: String
    var counts: NotebookCounts

    init(id: String, name: String, description: String = "", archived: Bool = false, createdAt: String = "",
         updatedAt: String = "", counts: NotebookCounts = NotebookCounts()) {
        self.id = id
        self.name = name
        self.description = description
        self.archived = archived
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.counts = counts
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = c.lenientOptional(String.self, .name) ?? id
        description = c.lenientOptional(String.self, .description) ?? ""
        archived = c.lenientOptional(Bool.self, .archived) ?? false
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
        updatedAt = c.lenientOptional(String.self, .updatedAt) ?? ""
        counts = c.lenientOptional(NotebookCounts.self, .counts) ?? NotebookCounts()
    }

    private enum CodingKeys: String, CodingKey {
        case id, name, description, archived, counts
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    var updated: Date? { NoteDates.parse(updatedAt) }
}

/// `GET /{nb}`: the notebook with everything in it.
struct NotebookDetail: Decodable, Equatable, Sendable {
    var notebook: Notebook
    var sources: [NotebookSource]
    var notes: [NotebookNote]
    var chats: [NotebookChat]
    var podcasts: [NotebookPodcast]

    init(notebook: Notebook, sources: [NotebookSource] = [], notes: [NotebookNote] = [], chats: [NotebookChat] = [],
         podcasts: [NotebookPodcast] = []) {
        self.notebook = notebook
        self.sources = sources
        self.notes = notes
        self.chats = chats
        self.podcasts = podcasts
    }

    init(from decoder: Decoder) throws {
        notebook = try Notebook(from: decoder)
        let c = try decoder.container(keyedBy: CodingKeys.self)
        sources = c.lenientList(NotebookSource.self, .sources)
        notes = c.lenientList(NotebookNote.self, .notes)
        chats = c.lenientList(NotebookChat.self, .chats)
        podcasts = c.lenientList(NotebookPodcast.self, .podcasts)
    }

    private enum CodingKeys: String, CodingKey { case sources, notes, chats, podcasts }
}

// MARK: - Sources

/// How much of a source the chat sees.
enum SourceContext: String, CaseIterable, Identifiable, Sendable {
    case full, summary, off
    var id: String { rawValue }

    var label: String {
        switch self {
        case .full: String(localized: "Full text")
        case .summary: String(localized: "Summary")
        case .off: String(localized: "Off")
        }
    }

    var systemImage: String {
        switch self {
        case .full: "doc.text.fill"
        case .summary: "text.alignleft"
        case .off: "eye.slash"
        }
    }
}

struct NotebookSource: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    /// `pdf`, `youtube`, `web` (`url` in older servers), `text`, `audio`, `file`, `note`, `sandbox`.
    var kind: String
    var title: String
    var origin: String
    /// `queued`, `extracting`, `embedding`, `ready` or `failed`.
    var status: String
    var error: String?
    var chars: Int?
    var chunks: Int?
    var pages: Int?
    var durationS: Double?
    var hasFile: Bool
    var context: String?
    var createdAt: String
    var updatedAt: String

    init(id: String, kind: String = "text", title: String = "", origin: String = "", status: String = "ready", error: String? = nil,
         chars: Int? = nil, chunks: Int? = nil, pages: Int? = nil, durationS: Double? = nil, hasFile: Bool = false,
         context: String? = nil, createdAt: String = "", updatedAt: String = "") {
        self.id = id
        self.kind = kind
        self.title = title
        self.origin = origin
        self.status = status
        self.error = error
        self.chars = chars
        self.chunks = chunks
        self.pages = pages
        self.durationS = durationS
        self.hasFile = hasFile
        self.context = context
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        kind = c.lenientOptional(String.self, .kind) ?? "text"
        title = c.lenientOptional(String.self, .title) ?? id
        origin = c.lenientOptional(String.self, .origin) ?? ""
        status = c.lenientOptional(String.self, .status) ?? "ready"
        error = c.lenientOptional(String.self, .error)
        chars = c.lenientOptional(Int.self, .chars)
        chunks = c.lenientOptional(Int.self, .chunks)
        pages = c.lenientOptional(Int.self, .pages)
        durationS = c.lenientOptional(Double.self, .durationS)
        hasFile = c.lenientOptional(Bool.self, .hasFile) ?? false
        context = c.lenientOptional(String.self, .context)
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
        updatedAt = c.lenientOptional(String.self, .updatedAt) ?? ""
    }

    private enum CodingKeys: String, CodingKey {
        case id, kind, title, origin, status, error, chars, chunks, pages, context
        case durationS = "duration_s"
        case hasFile = "has_file"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    var isReady: Bool { status == "ready" }
    var isFailed: Bool { status == "failed" }
    /// Still being read: worth polling.
    var isPending: Bool { !isReady && !isFailed }
    var sourceContext: SourceContext { SourceContext(rawValue: context ?? "") ?? .full }

    var statusLabel: String {
        switch status {
        case "queued": String(localized: "Waiting")
        case "extracting": String(localized: "Reading")
        case "embedding": String(localized: "Indexing")
        case "ready": String(localized: "Ready")
        case "failed": String(localized: "Failed")
        default: status
        }
    }

    var kindIcon: String {
        switch kind {
        case "pdf": "doc.richtext"
        case "youtube": "play.rectangle"
        case "web", "url": "globe"
        case "text": "text.alignleft"
        case "audio": "waveform"
        case "file", "sandbox": "doc"
        case "note": "note.text"
        default: "doc"
        }
    }

    /// "22 pages · 37 passages", "5:12 · 8 passages" or "12,400 characters".
    var summaryLine: String {
        var parts: [String] = []
        if let pages, pages > 0 { parts.append(String(localized: "\(pages) pages")) }
        if let durationS, durationS > 0 { parts.append(NotebookTime.clock(durationS)) }
        if let chunks, chunks > 0 { parts.append(String(localized: "\(chunks) passages")) }
        if parts.isEmpty, let chars, chars > 0 { parts.append(String(localized: "\(chars) characters")) }
        return parts.joined(separator: " · ")
    }
}

/// A passage of a source (`GET /sources/{src}/chunks`).
struct SourceChunk: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    var ordinal: Int
    var text: String
    var page: Int?
    var startS: Double?

    init(id: String, ordinal: Int, text: String, page: Int? = nil, startS: Double? = nil) {
        self.id = id
        self.ordinal = ordinal
        self.text = text
        self.page = page
        self.startS = startS
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ordinal = c.lenientOptional(Int.self, .ordinal) ?? 0
        id = c.lenientOptional(String.self, .id) ?? "\(ordinal)"
        text = c.lenientOptional(String.self, .text) ?? ""
        page = c.lenientOptional(Int.self, .page)
        startS = c.lenientOptional(Double.self, .startS)
    }

    private enum CodingKeys: String, CodingKey {
        case id, ordinal, text, page
        case startS = "start_s"
    }

    /// "p. 7" or "1:05", empty when the passage has no position.
    var position: String { NotebookTime.position(page: page, startS: startS) }
}

struct Transformation: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    var name: String
    var description: String
    var prompt: String
    var applyDefault: Bool
    var builtin: Bool

    init(id: String, name: String, description: String = "", prompt: String = "", applyDefault: Bool = false, builtin: Bool = true) {
        self.id = id
        self.name = name
        self.description = description
        self.prompt = prompt
        self.applyDefault = applyDefault
        self.builtin = builtin
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = c.lenientOptional(String.self, .name) ?? id
        description = c.lenientOptional(String.self, .description) ?? ""
        prompt = c.lenientOptional(String.self, .prompt) ?? ""
        applyDefault = c.lenientOptional(Bool.self, .applyDefault) ?? false
        builtin = c.lenientOptional(Bool.self, .builtin) ?? false
    }

    private enum CodingKeys: String, CodingKey {
        case id, name, description, prompt, builtin
        case applyDefault = "apply_default"
    }
}

struct SourceInsight: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    var sourceId: String
    var transformationId: String
    var title: String
    /// `queued`, `running`, `ready` or `failed`.
    var status: String
    var text: String
    var createdAt: String

    init(id: String, sourceId: String = "", transformationId: String = "", title: String = "", status: String = "ready",
         text: String = "", createdAt: String = "") {
        self.id = id
        self.sourceId = sourceId
        self.transformationId = transformationId
        self.title = title
        self.status = status
        self.text = text
        self.createdAt = createdAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        sourceId = c.lenientOptional(String.self, .sourceId) ?? ""
        transformationId = c.lenientOptional(String.self, .transformationId) ?? ""
        title = c.lenientOptional(String.self, .title) ?? ""
        status = c.lenientOptional(String.self, .status) ?? "ready"
        text = c.lenientOptional(String.self, .text) ?? ""
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
    }

    private enum CodingKeys: String, CodingKey {
        case id, title, status, text
        case sourceId = "source_id"
        case transformationId = "transformation_id"
        case createdAt = "created_at"
    }

    var isPending: Bool { status == "queued" || status == "running" }
}

// MARK: - Notes

/// A Keep note linked to a notebook. The note itself lives in Keep.
struct NotebookNote: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let slug: String
    var title: String
    var excerpt: String
    var updatedAt: String
    var indexed: Bool
    var id: String { slug }

    init(slug: String, title: String = "", excerpt: String = "", updatedAt: String = "", indexed: Bool = false) {
        self.slug = slug
        self.title = title.isEmpty ? slug : title
        self.excerpt = excerpt
        self.updatedAt = updatedAt
        self.indexed = indexed
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        slug = try c.decode(String.self, forKey: .slug)
        title = c.lenientOptional(String.self, .title) ?? slug
        excerpt = c.lenientOptional(String.self, .excerpt) ?? ""
        updatedAt = c.lenientOptional(String.self, .updatedAt) ?? ""
        indexed = c.lenientOptional(Bool.self, .indexed) ?? false
    }

    private enum CodingKeys: String, CodingKey {
        case slug, title, excerpt, indexed
        case updatedAt = "updated_at"
    }

    var updated: Date? { NoteDates.parse(updatedAt) }
}

// MARK: - Chat

struct NotebookChat: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    var notebookId: String
    var title: String
    var createdAt: String
    var updatedAt: String
    var messageCount: Int

    init(id: String, notebookId: String = "", title: String = "", createdAt: String = "", updatedAt: String = "", messageCount: Int = 0) {
        self.id = id
        self.notebookId = notebookId
        self.title = title
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.messageCount = messageCount
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        notebookId = c.lenientOptional(String.self, .notebookId) ?? ""
        title = c.lenientOptional(String.self, .title) ?? ""
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
        updatedAt = c.lenientOptional(String.self, .updatedAt) ?? ""
        messageCount = c.countOrArray(.messages) ?? 0
    }

    private enum CodingKeys: String, CodingKey {
        case id, title, messages
        case notebookId = "notebook_id"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    var displayTitle: String { title.isEmpty ? String(localized: "New chat") : title }
}

/// `GET /chats/{c}`: the chat with its messages.
struct NotebookChatDetail: Decodable, Equatable, Sendable {
    var chat: NotebookChat
    var messages: [NotebookMessage]

    init(chat: NotebookChat, messages: [NotebookMessage] = []) {
        self.chat = chat
        self.messages = messages
    }

    init(from decoder: Decoder) throws {
        chat = try NotebookChat(from: decoder)
        let c = try decoder.container(keyedBy: CodingKeys.self)
        messages = c.lenientList(NotebookMessage.self, .messages)
    }

    private enum CodingKeys: String, CodingKey { case messages }
}

/// A passage the server resolved for a citation in an answer.
struct NotebookCitation: Decodable, Identifiable, Equatable, Hashable, Sendable {
    /// `source:<chunk id>`, `note:<slug>` or `insight:<id>`.
    let ref: String
    /// `source`, `note` or `insight`.
    var kind: String
    var sourceId: String?
    var chunk: Int?
    var title: String
    var excerpt: String
    var page: Int?
    var startS: Double?
    var id: String { ref }

    init(ref: String, kind: String? = nil, sourceId: String? = nil, chunk: Int? = nil, title: String = "", excerpt: String = "",
         page: Int? = nil, startS: Double? = nil) {
        self.ref = ref
        self.kind = kind ?? ref.split(separator: ":", maxSplits: 1).first.map(String.init) ?? "source"
        self.sourceId = sourceId
        self.chunk = chunk
        self.title = title
        self.excerpt = excerpt
        self.page = page
        self.startS = startS
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let ref = try c.decode(String.self, forKey: .ref)
        self.init(ref: ref,
                  kind: c.lenientOptional(String.self, .kind),
                  sourceId: c.lenientOptional(String.self, .sourceId),
                  chunk: c.lenientOptional(Int.self, .chunk),
                  title: c.lenientOptional(String.self, .title) ?? "",
                  excerpt: c.lenientOptional(String.self, .excerpt) ?? "",
                  page: c.lenientOptional(Int.self, .page),
                  startS: c.lenientOptional(Double.self, .startS))
    }

    private enum CodingKeys: String, CodingKey {
        case ref, kind, chunk, title, excerpt, page
        case sourceId = "source_id"
        case startS = "start_s"
    }

    /// "p. 7" or "1:05", empty when unknown.
    var position: String { NotebookTime.position(page: page, startS: startS) }
}

struct NotebookMessage: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    /// `user` or `assistant`.
    var role: String
    var text: String
    var citations: [NotebookCitation]
    var model: String?
    var createdAt: String

    init(id: String, role: String, text: String, citations: [NotebookCitation] = [], model: String? = nil, createdAt: String = "") {
        self.id = id
        self.role = role
        self.text = text
        self.citations = citations
        self.model = model
        self.createdAt = createdAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        role = c.lenientOptional(String.self, .role) ?? "assistant"
        text = c.lenientOptional(String.self, .text) ?? ""
        citations = c.lenientList(NotebookCitation.self, .citations)
        model = c.lenientOptional(String.self, .model)
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
    }

    private enum CodingKeys: String, CodingKey {
        case id, role, text, citations, model
        case createdAt = "created_at"
    }

    var isUser: Bool { role == "user" }
}

/// `POST /chats/{c}/messages`.
struct NotebookExchange: Decodable, Equatable, Sendable {
    let user: NotebookMessage
    let reply: NotebookMessage
}

/// What answers: DeepSeek with Lucina's key, or the local Qwen.
enum NotebookModel: String, CaseIterable, Identifiable, Sendable {
    case deepseek, qwen
    var id: String { rawValue }

    var label: String {
        switch self {
        case .deepseek: "DeepSeek"
        case .qwen: String(localized: "Local")
        }
    }
}

// MARK: - Podcasts

struct PodcastOutlineItem: Decodable, Equatable, Hashable, Sendable {
    var name: String
    var description: String
    var size: String

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = c.lenientOptional(String.self, .name) ?? ""
        description = c.lenientOptional(String.self, .description) ?? ""
        size = c.lenientOptional(String.self, .size) ?? ""
    }

    private enum CodingKeys: String, CodingKey { case name, description, size }
}

struct PodcastLine: Decodable, Identifiable, Equatable, Hashable, Sendable {
    var speaker: String
    var text: String
    /// Position in the transcript; not part of the wire format.
    var index = 0
    var id: Int { index }

    init(speaker: String, text: String, index: Int = 0) {
        self.speaker = speaker
        self.text = text
        self.index = index
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        speaker = c.lenientOptional(String.self, .speaker) ?? ""
        text = c.lenientOptional(String.self, .text) ?? ""
    }

    private enum CodingKeys: String, CodingKey { case speaker, text }
}

struct NotebookPodcast: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    var notebookId: String
    var title: String
    var episodeProfile: String
    var speakerProfile: String
    var briefing: String
    /// `queued`, `outline`, `script`, `voicing`, `mixing`, `ready` or `failed`.
    var status: String
    var error: String?
    var outline: [PodcastOutlineItem]
    var transcript: [PodcastLine]
    var durationS: Double?
    var audio: Bool
    var createdAt: String

    init(id: String, notebookId: String = "", title: String = "", episodeProfile: String = "", speakerProfile: String = "",
         briefing: String = "", status: String = "queued", error: String? = nil, outline: [PodcastOutlineItem] = [],
         transcript: [PodcastLine] = [], durationS: Double? = nil, audio: Bool = false, createdAt: String = "") {
        self.id = id
        self.notebookId = notebookId
        self.title = title
        self.episodeProfile = episodeProfile
        self.speakerProfile = speakerProfile
        self.briefing = briefing
        self.status = status
        self.error = error
        self.outline = outline
        self.transcript = transcript
        self.durationS = durationS
        self.audio = audio
        self.createdAt = createdAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        notebookId = c.lenientOptional(String.self, .notebookId) ?? ""
        title = c.lenientOptional(String.self, .title) ?? ""
        episodeProfile = c.lenientOptional(String.self, .episodeProfile) ?? ""
        speakerProfile = c.lenientOptional(String.self, .speakerProfile) ?? ""
        briefing = c.lenientOptional(String.self, .briefing) ?? ""
        status = c.lenientOptional(String.self, .status) ?? "queued"
        error = c.lenientOptional(String.self, .error)
        outline = c.lenientList(PodcastOutlineItem.self, .outline)
        transcript = c.lenientList(PodcastLine.self, .transcript).enumerated().map { index, line in
            var line = line
            line.index = index
            return line
        }
        durationS = c.lenientOptional(Double.self, .durationS)
        audio = c.lenientOptional(Bool.self, .audio) ?? false
        createdAt = c.lenientOptional(String.self, .createdAt) ?? ""
    }

    private enum CodingKeys: String, CodingKey {
        case id, title, briefing, status, error, outline, transcript, audio
        case notebookId = "notebook_id"
        case episodeProfile = "episode_profile"
        case speakerProfile = "speaker_profile"
        case durationS = "duration_s"
        case createdAt = "created_at"
    }

    var isReady: Bool { status == "ready" }
    var isFailed: Bool { status == "failed" }
    var isPending: Bool { !isReady && !isFailed }

    var statusLabel: String {
        switch status {
        case "queued": String(localized: "Waiting")
        case "outline": String(localized: "Planning")
        case "script": String(localized: "Writing")
        case "voicing": String(localized: "Recording")
        case "mixing": String(localized: "Mixing")
        case "ready": String(localized: "Ready")
        case "failed": String(localized: "Failed")
        default: status
        }
    }

    var displayTitle: String { title.isEmpty ? String(localized: "Podcast") : title }
}

struct EpisodeProfile: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    var name: String
    var description: String
    var speakerProfile: String
    var briefing: String
    var segments: Int

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = c.lenientOptional(String.self, .name) ?? id
        description = c.lenientOptional(String.self, .description) ?? ""
        speakerProfile = c.lenientOptional(String.self, .speakerProfile) ?? ""
        briefing = c.lenientOptional(String.self, .briefing) ?? ""
        segments = c.lenientOptional(Int.self, .segments) ?? 0
    }

    private enum CodingKeys: String, CodingKey {
        case id, name, description, briefing, segments
        case speakerProfile = "speaker_profile"
    }
}

struct SpeakerProfile: Decodable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    var name: String
    var language: String
    var speakers: [String]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = c.lenientOptional(String.self, .name) ?? id
        language = c.lenientOptional(String.self, .language) ?? ""
        struct Named: Decodable { var name: String? }
        speakers = c.lenientList(Named.self, .speakers).compactMap(\.name)
    }

    private enum CodingKeys: String, CodingKey { case id, name, language, speakers }
}

struct PodcastProfiles: Decodable, Equatable, Sendable {
    var episodes: [EpisodeProfile]
    var speakers: [SpeakerProfile]

    init(episodes: [EpisodeProfile] = [], speakers: [SpeakerProfile] = []) {
        self.episodes = episodes
        self.speakers = speakers
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        episodes = c.lenientList(EpisodeProfile.self, .episodes)
        speakers = c.lenientList(SpeakerProfile.self, .speakers)
    }

    private enum CodingKeys: String, CodingKey {
        case episodes = "episode_profiles"
        case speakers = "speaker_profiles"
    }
}

// MARK: - Time

enum NotebookTime {
    /// 312.4 → "5:12", 3725 → "1:02:05".
    static func clock(_ seconds: Double) -> String {
        let total = max(0, Int(seconds.rounded()))
        let (h, m, s) = (total / 3600, total % 3600 / 60, total % 60)
        return h > 0 ? String(format: "%d:%02d:%02d", h, m, s) : String(format: "%d:%02d", m, s)
    }

    /// "p. 7" for a page, "1:05" for a time in audio or video, "" for neither.
    static func position(page: Int?, startS: Double?) -> String {
        if let page { return String(localized: "p. \(page)") }
        if let startS { return clock(startS) }
        return ""
    }
}
