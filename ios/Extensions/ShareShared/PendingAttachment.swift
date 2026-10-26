import Foundation
import UIKit
import UniformTypeIdentifiers
import ImageIO
import AVFoundation
import QuickLookThumbnailing

/// What a file is, for the icon, the bubble and the type label. Same words as the share contract.
enum AttachmentKind: String, Codable, Sendable, CaseIterable {
    case image, video, audio, pdf, document, spreadsheet, presentation, text, code, archive, contact, calendar, file

    static func classify(mime: String, filename: String) -> AttachmentKind {
        let ext = (filename as NSString).pathExtension.lowercased()
        let type = UTType(filenameExtension: ext) ?? UTType(mimeType: mime)
        let mime = mime.lowercased()
        if let type {
            if type.conforms(to: .vCard) { return .contact }
            if type.conforms(to: .calendarEvent) || ext == "ics" { return .calendar }
            if type.conforms(to: .pdf) { return .pdf }
            if type.conforms(to: .image) { return .image }
            if type.conforms(to: .movie) || type.conforms(to: .video) { return .video }
            if type.conforms(to: .audio) { return .audio }
            if type.conforms(to: .spreadsheet) || ["csv", "tsv", "xlsx", "xls", "ods", "numbers"].contains(ext) { return .spreadsheet }
            if type.conforms(to: .presentation) || ["pptx", "ppt", "odp", "key"].contains(ext) { return .presentation }
            // Office files are zip packages underneath: decide them before archives.
            if ["doc", "docx", "odt", "rtf", "pages"].contains(ext) || type.conforms(to: .rtf) { return .document }
            if type.conforms(to: .archive) || ["zip", "tar", "gz", "tgz", "7z", "rar", "bz2", "xz"].contains(ext) { return .archive }
            if type.conforms(to: .sourceCode) || Self.codeExtensions.contains(ext) { return .code }
            if type.conforms(to: .plainText) || ["md", "txt", "markdown"].contains(ext) { return .text }
        }
        if mime.hasPrefix("image/") { return .image }
        if mime.hasPrefix("video/") { return .video }
        if mime.hasPrefix("audio/") { return .audio }
        if mime == "application/pdf" { return .pdf }
        if Self.codeExtensions.contains(ext) { return .code }
        return .file
    }

    static let codeExtensions: Set<String> = ["rs", "swift", "kt", "kts", "py", "js", "mjs", "ts", "tsx", "jsx", "go", "c", "h", "cpp",
                                              "hpp", "cc", "java", "sh", "zsh", "bash", "sql", "json", "yaml", "yml", "toml", "html",
                                              "css", "scss", "xml", "rb", "php", "lua", "m", "mm", "cs", "dart", "vue", "svelte"]

    var symbol: String {
        switch self {
        case .image: return "photo"
        case .video: return "video"
        case .audio: return "waveform"
        case .pdf: return "doc.richtext"
        case .document: return "doc.text"
        case .spreadsheet: return "tablecells"
        case .presentation: return "rectangle.on.rectangle.angled"
        case .text: return "text.alignleft"
        case .code: return "chevron.left.forwardslash.chevron.right"
        case .archive: return "doc.zipper"
        case .contact: return "person.crop.circle"
        case .calendar: return "calendar"
        case .file: return "doc"
        }
    }

    var isVisual: Bool { self == .image || self == .video }
}

/// Where attachments wait before they are sent: the App Group's `Shares/Pending`, or this process's caches
/// while the group is not provisioned (ShareRequestBuilder.directory()).
enum AttachmentStore {
    static func pendingDirectory() throws -> URL {
        let url = try ShareRequestBuilder.directory().appendingPathComponent("Pending", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    /// Leftovers of trays that were never sent (the app was killed with a full tray).
    static func purgePending(olderThan age: TimeInterval = 24 * 60 * 60) {
        guard let directory = try? pendingDirectory(),
              let entries = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.contentModificationDateKey]) else { return }
        for entry in entries {
            let date = (try? entry.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate ?? .distantPast
            if Date().timeIntervalSince(date) > age { try? FileManager.default.removeItem(at: entry) }
        }
    }
}

/// One file in the tray (composer or share sheet), copied into our own container.
struct PendingAttachment: Identifiable, Equatable, @unchecked Sendable {
    let id: UUID
    var url: URL
    var name: String
    var mime: String
    var kind: AttachmentKind
    var size: Int64
    /// A local preview (up to ~900 px for photos and videos, a Quick Look icon for documents).
    var thumbnail: UIImage?
    /// The on-device transcript of an audio file (sent as `meta.files[i].transcript`).
    var transcript: String?
    var isTranscribing = false
    var width: Int?
    var height: Int?
    var durationMs: Int?
    var takenAt: Date?
    /// A voice note recorded with the chat's mic button (`meta.files[i].voice`).
    var voice = false
    /// The voice note's waveform: base64 of up to 64 bytes (`meta.files[i].waveform`).
    var waveform: String?

    init(id: UUID = UUID(), url: URL, name: String, mime: String, kind: AttachmentKind? = nil, size: Int64 = 0,
         thumbnail: UIImage? = nil, transcript: String? = nil, width: Int? = nil, height: Int? = nil,
         durationMs: Int? = nil, takenAt: Date? = nil, voice: Bool = false, waveform: String? = nil) {
        self.id = id
        self.url = url
        self.name = name
        self.mime = mime
        self.kind = kind ?? AttachmentKind.classify(mime: mime, filename: name)
        self.size = size
        self.thumbnail = thumbnail
        self.transcript = transcript
        self.width = width
        self.height = height
        self.durationMs = durationMs
        self.takenAt = takenAt
        self.voice = voice
        self.waveform = waveform
    }

    static func == (a: PendingAttachment, b: PendingAttachment) -> Bool {
        a.id == b.id && a.url == b.url && a.transcript == b.transcript && a.isTranscribing == b.isTranscribing
            && a.thumbnail === b.thumbnail && a.width == b.width && a.height == b.height && a.durationMs == b.durationMs
            && a.voice == b.voice && a.waveform == b.waveform
    }

    var shareFile: ShareFile { ShareFile(url: url, name: name, mime: mime) }

    /// "12.4 MB"
    var sizeText: String { ByteCountFormatter.string(fromByteCount: size, countStyle: .file) }

    /// "PDF", "MP4", "ZIP".
    var typeLabel: String {
        let ext = (name as NSString).pathExtension
        return ext.isEmpty ? kind.rawValue.capitalized : ext.uppercased()
    }

    /// `meta.files[index]` for this file.
    func metaFile(index: Int) -> ShareMeta.File {
        ShareMeta.File(index: index, transcript: transcript.flatMap { $0.isEmpty ? nil : $0 }, width: width, height: height,
                       durationMs: durationMs, takenAt: takenAt.map { ISO8601DateFormatter().string(from: $0) },
                       voice: voice ? true : nil, waveform: voice ? waveform : nil)
    }

    static func meta(for attachments: [PendingAttachment], location: ShareMeta.Location? = nil) -> ShareMeta {
        ShareMeta(files: attachments.enumerated().map { $0.element.metaFile(index: $0.offset) }, location: location)
    }

    // MARK: - Reading what the file is

    /// Size, dimensions, duration, date taken and a preview, read on a background task.
    static func make(from file: ShareFile) async -> PendingAttachment {
        var attachment = PendingAttachment(url: file.url, name: file.name, mime: file.mime)
        attachment.size = Int64((try? file.url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0)
        switch attachment.kind {
        case .image:
            let facts = await Task.detached(priority: .userInitiated) { imageFacts(file.url) }.value
            attachment.width = facts.width
            attachment.height = facts.height
            attachment.takenAt = facts.takenAt
            attachment.thumbnail = facts.preview
        case .video, .audio:
            let facts = await mediaFacts(file.url, video: attachment.kind == .video)
            attachment.durationMs = facts.durationMs
            attachment.width = facts.width
            attachment.height = facts.height
            attachment.thumbnail = facts.poster
        default:
            break
        }
        if attachment.thumbnail == nil, attachment.kind != .audio {
            attachment.thumbnail = await quickLookThumbnail(file.url, size: CGSize(width: 120, height: 120))
        }
        return attachment
    }

    struct ImageFacts {
        var width: Int?
        var height: Int?
        var takenAt: Date?
        var preview: UIImage?
    }

    static func imageFacts(_ url: URL, maxPixels: Int = 900) -> ImageFacts {
        guard let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary) else { return ImageFacts() }
        var facts = ImageFacts()
        if let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] {
            let width = properties[kCGImagePropertyPixelWidth] as? Int
            let height = properties[kCGImagePropertyPixelHeight] as? Int
            let orientation = properties[kCGImagePropertyOrientation] as? Int ?? 1
            // EXIF orientations 5-8 are rotated a quarter turn: width and height swap on screen.
            if orientation >= 5 { facts.width = height; facts.height = width } else { facts.width = width; facts.height = height }
            if let exif = properties[kCGImagePropertyExifDictionary] as? [CFString: Any],
               let original = exif[kCGImagePropertyExifDateTimeOriginal] as? String {
                let formatter = DateFormatter()
                formatter.locale = Locale(identifier: "en_US_POSIX")
                formatter.dateFormat = "yyyy:MM:dd HH:mm:ss"
                facts.takenAt = formatter.date(from: original)
            }
        }
        let options: [CFString: Any] = [kCGImageSourceCreateThumbnailFromImageAlways: true,
                                        kCGImageSourceCreateThumbnailWithTransform: true,
                                        kCGImageSourceThumbnailMaxPixelSize: maxPixels]
        if let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) {
            facts.preview = UIImage(cgImage: image)
        }
        return facts
    }

    struct MediaFacts {
        var durationMs: Int?
        var width: Int?
        var height: Int?
        var poster: UIImage?
    }

    static func mediaFacts(_ url: URL, video: Bool) async -> MediaFacts {
        let asset = AVURLAsset(url: url)
        var facts = MediaFacts()
        if let duration = try? await asset.load(.duration), duration.isNumeric {
            facts.durationMs = Int((duration.seconds * 1000).rounded())
        }
        guard video else { return facts }
        if let track = try? await asset.loadTracks(withMediaType: .video).first,
           let (size, transform) = try? await track.load(.naturalSize, .preferredTransform) {
            let shown = size.applying(transform)
            facts.width = Int(abs(shown.width).rounded())
            facts.height = Int(abs(shown.height).rounded())
        }
        let generator = AVAssetImageGenerator(asset: asset)
        generator.appliesPreferredTrackTransform = true
        generator.maximumSize = CGSize(width: 900, height: 900)
        if let (image, _) = try? await generator.image(at: CMTime(seconds: 0.1, preferredTimescale: 600)) {
            facts.poster = UIImage(cgImage: image)
        }
        return facts
    }

    static func quickLookThumbnail(_ url: URL, size: CGSize) async -> UIImage? {
        let request = QLThumbnailGenerator.Request(fileAt: url, size: size, scale: 2, representationTypes: .thumbnail)
        return try? await QLThumbnailGenerator.shared.generateBestRepresentation(for: request).uiImage
    }
}
