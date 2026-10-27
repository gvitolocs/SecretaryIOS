import CoreGraphics
import Foundation
import UniformTypeIdentifiers

// What a message in Lucina's chat can do (Telegram style): swipe left to reply, swipe right to hear Lucina read
// it, long-press for the full menu, share the real content to other apps, select several and share them as a
// transcript. This file holds the pure parts (the swipe's thresholds, the quote and plain-text summaries, which
// share items a message makes, the transcript, the TTS cache) so they can be unit-tested; the views are in
// MessageActionViews.swift, the player in ChatReadAloud.swift and the share sheet in ChatShareSheet.swift.

// MARK: - Swipe

/// A horizontal drag on a bubble: left past the threshold replies, right reads it aloud. The drag only counts
/// once it moved `activation` points and more sideways than up or down (vertical drags stay the scroll view's).
enum BubbleSwipe {
    /// The finger must move this far before the drag picks an axis.
    static let activation: CGFloat = 12
    /// Letting go past this triggers the action (the haptic fires when it is crossed).
    static let threshold: CGFloat = 60
    /// The bubble follows the finger up to here.
    static let maxOffset: CGFloat = 70

    enum Action: Equatable { case none, reply, readAloud }

    /// The drag's axis, decided once per touch: undecided until it moved `activation` points.
    enum Track: Equatable {
        case undecided
        case horizontal(CGFloat)
        case vertical
    }

    /// The next state for the finger's translation. Once vertical, the touch stays vertical (the scroll view's).
    static func track(_ state: Track, translation: CGSize) -> Track {
        switch state {
        case .vertical:
            return .vertical
        case .horizontal:
            return .horizontal(translation.width)
        case .undecided:
            let dx = abs(translation.width)
            let dy = abs(translation.height)
            guard max(dx, dy) >= activation else { return .undecided }
            return dx > dy ? .horizontal(translation.width) : .vertical
        }
    }

    /// What letting go now would do.
    static func action(_ track: Track, canReply: Bool, canReadAloud: Bool) -> Action {
        guard case let .horizontal(dx) = track else { return .none }
        if dx <= -threshold, canReply { return .reply }
        if dx >= threshold, canReadAloud { return .readAloud }
        return .none
    }

    /// How far the bubble moves: with the finger, up to `maxOffset`, only in a direction that does something.
    static func offset(_ track: Track, canReply: Bool, canReadAloud: Bool) -> CGFloat {
        guard case let .horizontal(dx) = track else { return 0 }
        if dx < 0, canReply { return max(dx, -maxOffset) }
        if dx > 0, canReadAloud { return min(dx, maxOffset) }
        return 0
    }

    /// 0 at rest, 1 at the threshold: the reply arrow / speaker fades in with it.
    static func progress(_ offset: CGFloat) -> Double {
        Double(min(1, abs(offset) / threshold))
    }
}

// MARK: - Plain text

enum ChatPlainText {
    /// Markdown to plain text for sharing and copying: headings, quotes and rules lose their marks, bullets
    /// become "•", checkboxes "☐"/"☑", links keep their address ("text (https://…)"), code fences go.
    static func from(_ markdown: String) -> String {
        let normalized = markdown.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        let lines = normalized.components(separatedBy: "\n").compactMap { raw -> String? in
            var line = raw
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") { return nil }
            if line.range(of: #"^\s{0,3}([-*_]\s*){3,}$"#, options: .regularExpression) != nil { return "" }
            line = line.replacingOccurrences(of: #"^\s{0,3}#{1,6}\s+"#, with: "", options: .regularExpression)
            line = line.replacingOccurrences(of: #"^\s{0,3}>\s?"#, with: "", options: .regularExpression)
            line = line.replacingOccurrences(of: #"^(\s*)[-*+]\s+\[[ ]\]\s+"#, with: "$1☐ ", options: .regularExpression)
            line = line.replacingOccurrences(of: #"^(\s*)[-*+]\s+\[[xX]\]\s+"#, with: "$1☑ ", options: .regularExpression)
            line = line.replacingOccurrences(of: #"^(\s*)[-*+]\s+"#, with: "$1• ", options: .regularExpression)
            // A link keeps its address; an image keeps its alt text (stripMarkdown).
            line = line.replacingOccurrences(of: #"(?<!!)\[([^\]]+)\]\((https?://[^)\s]+)\)"#, with: "$1 ($2)", options: .regularExpression)
            return RichMessage.stripMarkdown(line)
        }
        let joined = lines.joined(separator: "\n").replacingOccurrences(of: #"\n{3,}"#, with: "\n\n", options: .regularExpression)
        return joined.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// One line: whitespace runs (newlines included) become single spaces.
    static func oneLine(_ text: String) -> String {
        text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    /// A rich reply (format v1) as plain text: the bubbles, then each widget. `includeLinks` writes link
    /// widgets' addresses into the text (a transcript); otherwise they travel as their own share items.
    static func summary(_ rich: RichMessage, includeLinks: Bool) -> String {
        var parts = rich.bubbles.map(from).filter { !$0.isEmpty }
        for block in rich.blocks {
            var lines: [String] = []
            switch block {
            case let .checklist(title, items):
                if let title { lines.append(title) }
                lines += items.map { ($0.done ? "☑ " : "☐ ") + $0.text }
            case let .card(emoji, title, subtitle, body, _):
                lines.append([emoji, title].compactMap { $0 }.joined(separator: " "))
                if let subtitle { lines.append(subtitle) }
                if let body { lines.append(from(body)) }
            case let .stat(items):
                lines += items.map { stat in "\(stat.label): \(stat.value)" + (stat.delta.map { " (\($0))" } ?? "") }
            case let .list(title, items):
                if let title { lines.append(title) }
                lines += items.map { item in
                    "• " + [item.emoji, item.text].compactMap { $0 }.joined(separator: " ") + (item.meta.map { " — \($0)" } ?? "")
                }
            case let .table(title, columns, rows):
                if let title { lines.append(title) }
                lines.append(columns.joined(separator: " | "))
                lines += rows.map { $0.joined(separator: " | ") }
            case let .steps(title, items):
                if let title { lines.append(title) }
                lines += items.enumerated().map { "\($0.offset + 1). \($0.element)" }
            case let .quote(text, cite):
                lines.append("“\(text)”" + (cite.map { " — \($0)" } ?? ""))
            case let .link(url, title):
                lines.append(includeLinks ? "\(title): \(url.absoluteString)" : title)
            }
            let block = lines.filter { !$0.isEmpty }.joined(separator: "\n")
            if !block.isEmpty { parts.append(block) }
        }
        return parts.joined(separator: "\n\n")
    }

    /// The message's words as plain text: a rich reply's summary, or its text without markdown.
    static func body(of item: TimelineItem, includeLinks: Bool) -> String {
        if let rich = item.rich { return summary(rich, includeLinks: includeLinks) }
        return from(item.displayText)
    }
}

// MARK: - Quote

/// The quoted message above a reply (the composer's bar and the bubble): who wrote it and one or two lines.
enum ChatQuote {
    static func sender(_ item: TimelineItem) -> String {
        if item.role == .lucina { return "Lucina" }
        return item.isMcp ? item.source.label : String(localized: "You")
    }

    /// "Text", "📷 caption", "📷 Photo", "📷 3 photos", "🎤 Voice note 0:07", "📄 name.pdf"…
    static func summary(_ item: TimelineItem) -> String {
        if let rich = item.rich {
            let speech = rich.speechText
            if !speech.isEmpty { return speech }
            let text = ChatPlainText.oneLine(ChatPlainText.summary(rich, includeLinks: false))
            if !text.isEmpty { return text }
        }
        let text = ChatPlainText.oneLine(ChatPlainText.from(item.displayText))
        let images = item.attachments.filter(\.isImage).count
        let videos = item.attachments.filter(\.isVideo).count
        if !text.isEmpty {
            if images > 0 { return "📷 " + text }
            if videos > 0 { return "🎥 " + text }
            return text
        }
        if let voice = item.attachments.first(where: \.isVoice) {
            return "🎤 " + String(localized: "Voice note \(duration(voice.durationMs))")
        }
        if images > 1 { return "📷 " + String(localized: "\(images) photos") }
        if images == 1 { return "📷 " + String(localized: "Photo") }
        if videos > 0 { return "🎥 " + String(localized: "Video") }
        if let first = item.attachments.first { return attachmentLabel(first) }
        return String(localized: "Message")
    }

    /// One attachment as a short label (transcripts list them under the message's words).
    static func attachmentLabel(_ attachment: TimelineAttachment) -> String {
        let name = attachment.name?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmpty
        switch attachment.normalizedKind {
        case "image": return "📷 " + (name ?? String(localized: "Photo"))
        case "video": return "🎥 " + (name ?? String(localized: "Video"))
        case "voice": return "🎤 " + String(localized: "Voice note \(duration(attachment.durationMs))")
        case "audio": return "🎵 " + (name ?? String(localized: "Audio"))
        case "location": return "📍 " + (name ?? String(localized: "Location"))
        case "link": return "🔗 " + (attachment.url ?? name ?? "")
        default: return "📄 " + (name ?? String(localized: "File"))
        }
    }

    /// "0:07".
    static func duration(_ milliseconds: Int?) -> String {
        MediaFormat.time(Double(max(0, milliseconds ?? 0)) / 1000)
    }
}

// MARK: - Speech

enum ChatSpeech {
    /// What "Read aloud" says: a rich reply's bubbles (or its widgets' text), the message's words without
    /// markdown, and a voice note's transcript. Nil when there is nothing to read (a photo without a caption).
    static func text(for item: TimelineItem, transcript: String?) -> String? {
        var parts: [String] = []
        if let rich = item.rich {
            let speech = rich.speechText
            parts.append(speech.isEmpty ? ChatPlainText.summary(rich, includeLinks: false) : speech)
        } else {
            parts.append(ChatPlainText.from(item.displayText))
        }
        if item.attachments.contains(where: \.isVoice), let transcript { parts.append(transcript) }
        let text = ChatPlainText.oneLine(parts.joined(separator: " "))
        return text.isEmpty ? nil : String(text.prefix(1200))
    }
}

// MARK: - Share items

/// One thing a share sheet receives: words, a link, or a file of the message (resolved later: downloaded
/// originals, the composer's own copy, or the local preview).
enum ChatSharePart: Equatable {
    case text(String)
    case link(URL)
    case media(ChatShareMedia)
}

struct ChatShareMedia: Equatable {
    enum Kind: String, Equatable { case image, video, audio, voice, file }

    let kind: Kind
    let attachment: TimelineAttachment
    /// The position among the message's file parts (the composer's local copies are keyed by it).
    let fileIndex: Int?
    let shareId: String?

    static func kind(of attachment: TimelineAttachment) -> Kind {
        if attachment.isImage { return .image }
        if attachment.isVideo { return .video }
        if attachment.isVoice { return .voice }
        if attachment.isAudio { return .audio }
        return .file
    }
}

enum ChatShareBuilder {
    /// The share items of one message, files first: a picture → the image; picture + caption → both; text →
    /// the text without markdown; a voice note → the audio and its transcript; files → the files; cards → a
    /// plain-text summary plus their links; locations → a line and a Maps link.
    static func parts(for item: TimelineItem, transcript: String?) -> [ChatSharePart] {
        var media: [ChatSharePart] = []
        var links: [URL] = []
        var texts: [String] = []
        let body = ChatPlainText.body(of: item, includeLinks: false)
        if !body.isEmpty { texts.append(body) }
        for entry in BubbleEntry.entries(item.attachments) {
            let attachment = entry.attachment
            switch attachment.normalizedKind {
            case "link":
                if let url = webURL(attachment.url) { links.append(url) }
            case "location":
                guard let coordinate = attachment.coordinate else { continue }
                texts.append("📍 " + (attachment.name?.nonEmpty ?? String(localized: "Location")))
                if let url = mapsURL(lat: coordinate.lat, lon: coordinate.lon, name: attachment.name) { links.append(url) }
            case "text":
                continue
            default:
                media.append(.media(ChatShareMedia(kind: ChatShareMedia.kind(of: attachment), attachment: attachment,
                                                   fileIndex: entry.fileIndex, shareId: item.meta?.shareId)))
            }
        }
        if item.attachments.contains(where: \.isVoice), let transcript = transcript?.trimmingCharacters(in: .whitespacesAndNewlines),
           !transcript.isEmpty {
            texts.append(transcript)
        }
        if let rich = item.rich {
            for case let .link(url, _) in rich.blocks { links.append(url) }
        }
        var out = media
        if !texts.isEmpty { out.append(.text(texts.joined(separator: "\n\n"))) }
        var seen = Set<URL>()
        for url in links where seen.insert(url).inserted { out.append(.link(url)) }
        return out
    }

    /// "Share conversation": the selected messages as one plain-text transcript, then their files and links.
    static func conversationParts(_ items: [TimelineItem], userName: String, timeZone: TimeZone = .current,
                                  transcript: (TimelineItem) -> String?) -> [ChatSharePart] {
        var out: [ChatSharePart] = [.text(self.transcript(items, userName: userName, timeZone: timeZone, transcript: transcript))]
        var seen = Set<URL>()
        for item in items {
            for part in parts(for: item, transcript: transcript(item)) {
                switch part {
                case .media: out.append(part)
                case let .link(url): if seen.insert(url).inserted { out.append(part) }
                case .text: continue
                }
            }
        }
        return out
    }

    /// "Giuseppe — 10:02\n…\n\nLucina — 10:02\n…". Messages from several days carry the date as well.
    static func transcript(_ items: [TimelineItem], userName: String, timeZone: TimeZone = .current,
                           transcript: (TimelineItem) -> String?) -> String {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = timeZone
        let days = Set(items.map { calendar.startOfDay(for: $0.ts) })
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = timeZone
        formatter.dateFormat = days.count > 1 ? "yyyy-MM-dd HH:mm" : "HH:mm"
        return items.map { item in
            let speaker = item.role == .lucina ? "Lucina" : (item.isMcp ? item.source.label : userName)
            return "\(speaker) — \(formatter.string(from: item.ts))\n" + transcriptBody(item, transcript: transcript(item))
        }.joined(separator: "\n\n")
    }

    /// The words, then one line per attachment (a voice note's line is its transcript).
    static func transcriptBody(_ item: TimelineItem, transcript: String?) -> String {
        var lines: [String] = []
        let body = ChatPlainText.body(of: item, includeLinks: true)
        if !body.isEmpty { lines.append(body) }
        for attachment in item.attachments where attachment.normalizedKind != "text" {
            if attachment.isVoice, let words = transcript?.trimmingCharacters(in: .whitespacesAndNewlines), !words.isEmpty {
                lines.append("🎤 " + words)
            } else {
                lines.append(ChatQuote.attachmentLabel(attachment))
            }
        }
        return lines.isEmpty ? "…" : lines.joined(separator: "\n")
    }

    static func webURL(_ raw: String?) -> URL? {
        guard let raw, let url = URL(string: raw), ["http", "https"].contains(url.scheme?.lowercased() ?? ""), url.host != nil else { return nil }
        return url
    }

    static func mapsURL(lat: Double, lon: Double, name: String?) -> URL? {
        var components = URLComponents(string: "https://maps.apple.com/")
        components?.queryItems = [URLQueryItem(name: "ll", value: "\(lat),\(lon)"),
                                  URLQueryItem(name: "q", value: name?.nonEmpty ?? "\(lat),\(lon)")]
        return components?.url
    }

    /// The speaker name for the owner's messages in a transcript: the first name in a device name like
    /// "Giuseppe's iPhone" / "iPhone di Giuseppe" / "Giuseppes iPhone", else "Me".
    static func ownerName(deviceName: String?) -> String {
        let name = deviceName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let patterns = [
            #"^(.+?)['’]s?\s+(iPhone|iPad)\b"#,
            #"^(?:iPhone|iPad)\s+(?:di|de|von|van|af|til|of)\s+(.+)$"#,
            #"^(.+?)s\s+(?:iPhone|iPad)\b"#,
        ]
        for pattern in patterns {
            guard let regex = try? NSRegularExpression(pattern: pattern, options: [.caseInsensitive]),
                  let match = regex.firstMatch(in: name, range: NSRange(name.startIndex..., in: name)),
                  let range = Range(match.range(at: 1), in: name) else { continue }
            let first = name[range].trimmingCharacters(in: .whitespaces)
            if !first.isEmpty, first.lowercased() != "iphone", first.lowercased() != "ipad" { return first }
        }
        return String(localized: "Me")
    }
}

/// File names for downloaded originals: the name the owner sent, with an extension the receiving app can read.
enum ChatShareNaming {
    static func fileName(for attachment: TimelineAttachment) -> String {
        let kind = ChatShareMedia.kind(of: attachment)
        var base = attachment.name?.trimmingCharacters(in: .whitespacesAndNewlines).nonEmpty ?? defaultBase(kind)
        base = base.replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: ":", with: "_")
        if base.hasPrefix(".") { base = "_" + base.dropFirst() }
        if (base as NSString).pathExtension.isEmpty, let ext = attachment.mime.flatMap({ UTType(mimeType: $0)?.preferredFilenameExtension })
            ?? defaultExtension(kind) {
            base += "." + ext
        }
        return base
    }

    static func defaultBase(_ kind: ChatShareMedia.Kind) -> String {
        switch kind {
        case .image: return "photo"
        case .video: return "video"
        case .voice: return "voice-note"
        case .audio: return "audio"
        case .file: return "file"
        }
    }

    static func defaultExtension(_ kind: ChatShareMedia.Kind) -> String? {
        switch kind {
        case .image: return "jpg"
        case .video: return "mp4"
        case .voice: return "aac"
        case .audio: return "m4a"
        case .file: return nil
        }
    }

    /// The type a share sheet announces for a file.
    static func type(of url: URL) -> UTType {
        UTType(filenameExtension: url.pathExtension) ?? .data
    }
}

// MARK: - Menu

/// The long-press menu's actions (the swipes and VoiceOver's custom actions use the same ones).
enum MessageAction: String, CaseIterable, Identifiable {
    case reply, readAloud, copy, share, saveToPhotos, saveToLibrary, saveAsNote, select, hide

    var id: String { rawValue }

    /// The actions this message offers. Reply and the Lucina intents need the server's copy (an optimistic
    /// bubble has no id the server knows yet); Read aloud needs words; Save to Photos a picture.
    static func available(for item: TimelineItem, speech: String?) -> [MessageAction] {
        var out: [MessageAction] = []
        if !item.isLocal { out.append(.reply) }
        if speech?.isEmpty == false { out.append(.readAloud) }
        if !ChatPlainText.body(of: item, includeLinks: true).isEmpty || speech?.isEmpty == false { out.append(.copy) }
        out.append(.share)
        if item.attachments.contains(where: \.isImage) { out.append(.saveToPhotos) }
        if !item.isLocal {
            out.append(.saveToLibrary)
            out.append(.saveAsNote)
        }
        out.append(.select)
        out.append(.hide)
        return out
    }

    var symbol: String {
        switch self {
        case .reply: return "arrowshape.turn.up.left"
        case .readAloud: return "speaker.wave.2"
        case .copy: return "doc.on.doc"
        case .share: return "square.and.arrow.up"
        case .saveToPhotos: return "square.and.arrow.down"
        case .saveToLibrary: return "books.vertical"
        case .saveAsNote: return "note.text.badge.plus"
        case .select: return "checkmark.circle"
        case .hide: return "eye.slash"
        }
    }

    /// The share-pipeline intent for the two "send to Lucina" actions.
    var intent: String? {
        switch self {
        case .saveToLibrary: return "library"
        case .saveAsNote: return "note"
        default: return nil
        }
    }
}

// MARK: - TTS cache

/// Lucina's voice for a message, kept per message id (`Caches/TTS/<itemId>.mp3`), least recently played
/// files dropped beyond ~100 MB.
struct TTSCache: Sendable {
    let directory: URL
    let limit: Int64

    static let defaultLimit: Int64 = 100 * 1024 * 1024

    static var shared: TTSCache {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first ?? FileManager.default.temporaryDirectory
        return TTSCache(directory: caches.appendingPathComponent("TTS", isDirectory: true), limit: defaultLimit)
    }

    /// `<itemId>.mp3` for ids that are already safe file names; anything else (slashes, dots up front, very
    /// long ids) is cleaned and gets a short hash so two ids never share a file.
    static func fileName(for itemId: String) -> String {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-.")
        var cleaned = String(String.UnicodeScalarView(itemId.unicodeScalars.map { allowed.contains($0) ? $0 : "_" }))
        while cleaned.hasPrefix(".") { cleaned = "_" + cleaned.dropFirst() }
        if cleaned.isEmpty { cleaned = "item" }
        if cleaned != itemId || cleaned.count > 120 {
            cleaned = String(cleaned.prefix(100)) + "-" + hash(itemId)
        }
        return cleaned + ".mp3"
    }

    /// FNV-1a, 64 bit, hex: stable across launches (unlike `hashValue`).
    static func hash(_ text: String) -> String {
        var value: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in text.utf8 {
            value ^= UInt64(byte)
            value = value &* 0x0000_0100_0000_01b3
        }
        return String(value, radix: 16)
    }

    func url(for itemId: String) -> URL {
        directory.appendingPathComponent(Self.fileName(for: itemId))
    }

    /// The cached voice, marked as just used (the LRU order is the files' modification dates).
    func read(_ itemId: String, now: Date = Date()) -> Data? {
        let url = url(for: itemId)
        guard let data = try? Data(contentsOf: url), !data.isEmpty else { return nil }
        try? FileManager.default.setAttributes([.modificationDate: now], ofItemAtPath: url.path)
        return data
    }

    func write(_ data: Data, for itemId: String, now: Date = Date()) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let url = url(for: itemId)
        try data.write(to: url, options: .atomic)
        try? FileManager.default.setAttributes([.modificationDate: now], ofItemAtPath: url.path)
        prune(keeping: Self.fileName(for: itemId))
    }

    func remove(_ itemId: String) {
        try? FileManager.default.removeItem(at: url(for: itemId))
    }

    /// Drops the least recently used files until the folder is within the limit (never `keeping`).
    func prune(keeping: String? = nil) {
        let keys: [URLResourceKey] = [.contentModificationDateKey, .fileSizeKey]
        guard let entries = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: keys) else { return }
        var files = entries.map { url -> (url: URL, date: Date, size: Int64) in
            let values = try? url.resourceValues(forKeys: Set(keys))
            return (url, values?.contentModificationDate ?? .distantPast, Int64(values?.fileSize ?? 0))
        }
        var total = files.reduce(Int64(0)) { $0 + $1.size }
        guard total > limit else { return }
        files.sort { $0.date < $1.date }
        for file in files where total > limit && file.url.lastPathComponent != keeping {
            try? FileManager.default.removeItem(at: file.url)
            total -= file.size
        }
    }
}

extension String {
    /// nil for an empty string.
    var nonEmpty: String? { isEmpty ? nil : self }
}
