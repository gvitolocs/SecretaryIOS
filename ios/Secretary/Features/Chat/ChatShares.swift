import Foundation
import UIKit

// Attachments sent from Lucina's chat composer, Telegram style: they go through `POST /v1/share`
// (source `ios-chat`) on the background upload session. This file holds the wire types and the pure
// logic (outbox records, local → server bubble reconciliation) so it can be unit-tested.

/// `POST /v1/share` → 202 `{shareId, userItemId, attachments[]}`.
struct ShareAccepted: Decodable, Equatable, Sendable {
    var shareId: String?
    var userItemId: String?
    var attachments: [ShareStatus.Attachment]

    init(shareId: String? = nil, userItemId: String? = nil, attachments: [ShareStatus.Attachment] = []) {
        self.shareId = shareId
        self.userItemId = userItemId
        self.attachments = attachments
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        shareId = c.lenientOptional(String.self, .shareId)
        userItemId = c.lenientOptional(FlexibleID.self, .userItemId)?.value
        attachments = (c.lenientOptional([Lossy<ShareStatus.Attachment>].self, .attachments) ?? []).compactMap(\.value)
    }

    private enum CodingKeys: String, CodingKey { case shareId, userItemId, attachments }
}

/// `GET /v1/share/{id}`: where Lucina is with it.
struct ShareStatus: Decodable, Equatable, Sendable {
    struct Attachment: Decodable, Equatable, Sendable {
        var index: Int?
        var kind: String?
        var mediaId: String?
        /// `pending|reading|read|failed`
        var state: String?
        var summary: String?
        var thumbhash: String?

        init(index: Int? = nil, kind: String? = nil, mediaId: String? = nil, state: String? = nil, summary: String? = nil, thumbhash: String? = nil) {
            self.index = index; self.kind = kind; self.mediaId = mediaId; self.state = state; self.summary = summary; self.thumbhash = thumbhash
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            index = c.lenientOptional(Int.self, .index)
            kind = c.lenientOptional(String.self, .kind)
            mediaId = c.lenientOptional(String.self, .mediaId)
            state = c.lenientOptional(String.self, .state)
            summary = c.lenientOptional(String.self, .summary)
            thumbhash = c.lenientOptional(String.self, .thumbhash)
        }

        private enum CodingKeys: String, CodingKey { case index, kind, mediaId, state, summary, thumbhash }
    }

    /// `received` → `reading` → `thinking` → `done` | `error`
    var stage: String
    var attachments: [Attachment]
    var lucinaItemId: String?
    var reply: String?

    init(stage: String, attachments: [Attachment] = [], lucinaItemId: String? = nil, reply: String? = nil) {
        self.stage = stage
        self.attachments = attachments
        self.lucinaItemId = lucinaItemId
        self.reply = reply
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        stage = c.lenient(String.self, .stage, "received")
        attachments = (c.lenientOptional([Lossy<Attachment>].self, .attachments) ?? []).compactMap(\.value)
        lucinaItemId = c.lenientOptional(FlexibleID.self, .lucinaItemId)?.value
        reply = c.lenientOptional(String.self, .reply)
    }

    private enum CodingKeys: String, CodingKey { case stage, attachments, lucinaItemId, reply }

    var isWorking: Bool { ["received", "reading", "thinking"].contains(stage) }
    var isFinished: Bool { stage == "done" || stage == "error" }
}

/// One share the composer sent, kept on disk (`Shares/Chat/<shareId>/outgoing.json`) with its files until
/// the server took it, so a failed upload can be retried with the same share id.
struct OutgoingShare: Codable, Equatable, Sendable {
    struct File: Codable, Equatable, Sendable {
        /// The copy inside the share's folder.
        var fileName: String
        var name: String
        var mime: String
        var kind: String
        var size: Int64
        var width: Int?
        var height: Int?
        var durationMs: Int?
        var transcript: String?
        var takenAt: String?
    }

    var shareId: String
    var caption: String
    var files: [File]
    var urls: [String] = []
    var location: ShareMeta.Location?
    var createdAt: Date
    var userItemId: String?
    var acceptedAt: Date?

    var meta: ShareMeta {
        ShareMeta(files: files.enumerated().map { index, file in
            ShareMeta.File(index: index, transcript: file.transcript, width: file.width, height: file.height,
                           durationMs: file.durationMs, takenAt: file.takenAt)
        }, location: location)
    }

    func payload(in directory: URL) -> SharePayload {
        SharePayload(urls: urls, files: files.map { ShareFile(url: directory.appendingPathComponent($0.fileName), name: $0.name, mime: $0.mime) })
    }

    /// The optimistic bubble: files first (in part order), then links and the location.
    func localItem(ts: Date = Date()) -> TimelineItem {
        var attachments = files.map { file in
            TimelineAttachment(kind: file.kind, name: file.name, mime: file.mime, bytes: file.size,
                               width: file.width, height: file.height, durationMs: file.durationMs)
        }
        attachments += urls.map { TimelineAttachment(kind: "link", name: URL(string: $0)?.host, url: $0) }
        if let location {
            attachments.append(TimelineAttachment(kind: "location", name: location.name, url: "geo:\(location.lat),\(location.lon)",
                                                  lat: location.lat, lon: location.lon))
        }
        var meta = TimelineMeta(kind: "share")
        meta.shareId = shareId
        return TimelineItem(id: TimelineItem.localPrefix + shareId, ts: ts, channel: .ios, role: .user, text: caption,
                            clientMessageId: shareId, attachments: attachments, meta: meta, localStatus: .sending)
    }

    /// The file a bubble's attachment came from (by its position among the file attachments).
    func localFile(at fileIndex: Int, in directory: URL) -> URL? {
        guard files.indices.contains(fileIndex) else { return nil }
        let url = directory.appendingPathComponent(files[fileIndex].fileName)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }
}

enum ShareReconcile {
    /// The 202 arrived: the optimistic bubble becomes the server's user item (`userItemId`), keeping its
    /// attachments (now with media ids) so the local previews stay on screen. If the timeline already
    /// delivered the server's copy, the local bubble just goes away.
    static func accept(_ items: [TimelineItem], shareId: String, accepted: ShareAccepted?) -> [TimelineItem] {
        let localId = TimelineItem.localPrefix + shareId
        guard let index = items.firstIndex(where: { $0.id == localId }) else { return items }
        var out = items
        if out.contains(where: { !$0.isLocal && $0.meta?.shareId == shareId && $0.role == .user })
            || (accepted?.userItemId).map({ id in out.contains { $0.id == id } }) == true {
            out.remove(at: index)
            return out
        }
        let local = out[index]
        let attachments = merged(local.attachments, accepted?.attachments ?? [])
        guard let userItemId = accepted?.userItemId, !userItemId.isEmpty else {
            out[index].localStatus = .sent
            out[index].localError = nil
            out[index].attachments = attachments
            return out
        }
        out[index] = TimelineItem(id: userItemId, ts: local.ts, channel: local.channel, role: .user, text: local.text,
                                  clientMessageId: local.clientMessageId, attachments: attachments, meta: local.meta)
        return out
    }

    /// Media ids (and thumbhashes) from the server's attachment list, by file index.
    static func merged(_ local: [TimelineAttachment], _ server: [ShareStatus.Attachment]) -> [TimelineAttachment] {
        var fileIndex = 0
        return local.map { attachment in
            guard attachment.isFilePart else { return attachment }
            defer { fileIndex += 1 }
            var copy = attachment
            if let match = server.first(where: { $0.index == fileIndex }) ?? (server.allSatisfy { $0.index == nil } && server.indices.contains(fileIndex) ? server[fileIndex] : nil) {
                if let id = match.mediaId, !id.isEmpty { copy.mediaId = id }
                if let hash = match.thumbhash, !hash.isEmpty { copy.thumbhash = hash }
            }
            return copy
        }
    }

    static func fail(_ items: [TimelineItem], shareId: String, message: String) -> [TimelineItem] {
        items.map { item in
            guard item.id == TimelineItem.localPrefix + shareId else { return item }
            var copy = item
            copy.localStatus = .error
            copy.localError = message
            return copy
        }
    }

    /// Lucina already answered this share in the timeline.
    static func replied(_ items: [TimelineItem], userItemId: String?, status: ShareStatus?) -> Bool {
        if let id = status?.lucinaItemId, items.contains(where: { $0.id == id }) { return true }
        guard let userItemId else { return false }
        return items.contains { $0.role == .lucina && $0.replyTo == userItemId }
    }
}

/// The share folders on disk: `Shares/Chat/<shareId>/` with `outgoing.json` and the files.
enum ChatShareOutbox {
    static func root() throws -> URL {
        let url = try ShareRequestBuilder.directory().appendingPathComponent("Chat", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    static func directory(_ shareId: String) throws -> URL {
        let url = try root().appendingPathComponent(shareId, isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    static func save(_ share: OutgoingShare) throws {
        try JSONEncoder().encode(share).write(to: try directory(share.shareId).appendingPathComponent("outgoing.json"),
                                              options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    static func loadAll() -> [String: OutgoingShare] {
        guard let root = try? root(),
              let folders = try? FileManager.default.contentsOfDirectory(at: root, includingPropertiesForKeys: nil) else { return [:] }
        var out: [String: OutgoingShare] = [:]
        for folder in folders {
            guard let data = try? Data(contentsOf: folder.appendingPathComponent("outgoing.json")),
                  let share = try? JSONDecoder().decode(OutgoingShare.self, from: data) else { continue }
            // Accepted a day ago: nothing left to show or retry.
            if let accepted = share.acceptedAt, Date().timeIntervalSince(accepted) > 24 * 60 * 60 {
                try? FileManager.default.removeItem(at: folder)
                continue
            }
            out[share.shareId] = share
        }
        return out
    }

    /// The server has the files: keep only the record (for polling) and drop the copies.
    static func dropFiles(_ share: OutgoingShare) {
        guard let folder = try? directory(share.shareId) else { return }
        for file in share.files { try? FileManager.default.removeItem(at: folder.appendingPathComponent(file.fileName)) }
    }

    static func remove(_ shareId: String) {
        if let folder = try? root().appendingPathComponent(shareId, isDirectory: true) { try? FileManager.default.removeItem(at: folder) }
    }

    static func removeAll() {
        if let root = try? root() { try? FileManager.default.removeItem(at: root) }
    }

    /// Moves a tray file into the share's folder as `<index>-<name>`.
    static func adopt(_ attachment: PendingAttachment, index: Int, into folder: URL) throws -> OutgoingShare.File {
        let safe = attachment.name.replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: ":", with: "_")
        let fileName = "\(index)-" + (safe.isEmpty ? "file" : safe)
        let target = folder.appendingPathComponent(fileName)
        try? FileManager.default.removeItem(at: target)
        do { try FileManager.default.moveItem(at: attachment.url, to: target) } catch { try FileManager.default.copyItem(at: attachment.url, to: target) }
        return OutgoingShare.File(fileName: fileName, name: attachment.name, mime: attachment.mime, kind: attachment.kind.rawValue,
                                  size: attachment.size, width: attachment.width, height: attachment.height,
                                  durationMs: attachment.durationMs, transcript: attachment.transcript,
                                  takenAt: attachment.takenAt.map { ISO8601DateFormatter().string(from: $0) })
    }
}

/// Local previews of what the owner sent, by share id and file index: in memory and in Caches, so the
/// bubble keeps showing them after the server's copy replaces the optimistic one (and after a relaunch).
enum ChatPreviews {
    private static let cache: NSCache<NSString, UIImage> = {
        let cache = NSCache<NSString, UIImage>()
        cache.countLimit = 200
        return cache
    }()

    private static func key(_ shareId: String, _ index: Int) -> NSString { "\(shareId)#\(index)" as NSString }

    private static func file(_ shareId: String, _ index: Int) -> URL? {
        guard let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first else { return nil }
        return caches.appendingPathComponent("ChatPreviews", isDirectory: true).appendingPathComponent(shareId, isDirectory: true)
            .appendingPathComponent("\(index).jpg")
    }

    static func store(_ image: UIImage, shareId: String, index: Int) {
        cache.setObject(image, forKey: key(shareId, index))
        guard let url = file(shareId, index), let data = image.jpegData(compressionQuality: 0.8) else { return }
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: url, options: .atomic)
    }

    static func image(shareId: String?, index: Int) -> UIImage? {
        guard let shareId else { return nil }
        if let image = cache.object(forKey: key(shareId, index)) { return image }
        guard let url = file(shareId, index), let data = try? Data(contentsOf: url), let image = UIImage(data: data) else { return nil }
        cache.setObject(image, forKey: key(shareId, index))
        return image
    }

    static func removeAll() {
        cache.removeAllObjects()
        if let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first {
            try? FileManager.default.removeItem(at: caches.appendingPathComponent("ChatPreviews", isDirectory: true))
        }
    }
}
