import Foundation

// The Drive browser's wire types and calls (lucina-media, branch `cloud/drive-all-files`): every file
// in the owner's Drive folders, browsed as folders (`GET /v1/media/drive`, `/drive/browse`), with
// previews (`/items/{id}/text`, `/archive`, `/pdf`) and downloads (`/original?download=1`).
// Every field may be missing on an older server: the Drive screen then keeps the album list.

/// `item.drive`: where a Drive folder file lives and how to preview it.
struct DriveInfo: Codable, Hashable, Sendable {
    /// The Drive root (a configured folder label, "Lezioni").
    var folder: String?
    /// Within the folder, '/'-separated, never starting with '/'.
    var path: String?
    /// The parent directory within the folder; "" is the folder itself.
    var dir: String?
    var name: String?
    /// Lower case, "" when none.
    var ext: String?
    /// `DriveFileType` raw value.
    var type: String?
    /// pdf | office | text | code | archive | image | video | audio | none
    var preview: String?
    /// The syntax name of a code/text file.
    var language: String?
    var modified: String?
    /// Full-text extraction state: pending | ready | none | failed.
    var textState: String?

    private enum CodingKeys: String, CodingKey {
        case folder, path, dir, name, ext, type, preview, language, modified
        case textState = "text"
    }

    var modifiedDate: Date? { modified.flatMap(GalleryFormat.parse) }
}

/// One Drive root (`GET /v1/media/drive` → `folders`).
struct DriveRoot: Codable, Hashable, Identifiable, Sendable {
    let folder: String
    var files: Int?
    var bytes: Int64?
    var modified: String?

    var id: String { folder }
}

/// A Drive collection that is not a folder on disk (voice memos, imports, shares): opened with the
/// old album list (`GET /items?scope=drive&album=<id>`).
struct DriveRootAlbum: Codable, Hashable, Identifiable, Sendable {
    let id: String
    let title: String
    var count: Int?
}

/// `GET /v1/media/drive`.
struct DriveRoots: Codable, Hashable, Sendable {
    var folders: [DriveRoot]
    var albums: [DriveRootAlbum]
    /// pdf_thumbs, office_pdf, pdf_text.
    var converters: [String: Bool]?

    init(folders: [DriveRoot] = [], albums: [DriveRootAlbum] = [], converters: [String: Bool]? = nil) {
        self.folders = folders
        self.albums = albums
        self.converters = converters
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        folders = try c.decodeIfPresent([DriveRoot].self, forKey: .folders) ?? []
        albums = try c.decodeIfPresent([DriveRootAlbum].self, forKey: .albums) ?? []
        converters = try? c.decodeIfPresent([String: Bool].self, forKey: .converters)
    }

    private enum CodingKeys: String, CodingKey { case folders, albums, converters }
}

/// A subfolder in a listing.
struct DriveFolder: Codable, Hashable, Identifiable, Sendable {
    let name: String
    /// Within the Drive root.
    let path: String
    var files: Int?
    var bytes: Int64?
    var modified: String?

    var id: String { path }
}

/// One breadcrumb: a name and its path within the Drive root ("" is the root).
struct DriveCrumb: Codable, Hashable, Sendable {
    let name: String
    let path: String
}

/// `GET /v1/media/drive/browse`.
struct DriveListing: Decodable, Sendable {
    var folder: String?
    var path: String?
    var breadcrumbs: [DriveCrumb]?
    var folders: [DriveFolder]
    var files: [GalleryItem]
    /// Files matching (all pages).
    var total: Int?
    var nextOffset: Int?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        folder = try c.decodeIfPresent(String.self, forKey: .folder)
        path = try c.decodeIfPresent(String.self, forKey: .path)
        breadcrumbs = try? c.decodeIfPresent([DriveCrumb].self, forKey: .breadcrumbs)
        folders = try c.decodeIfPresent([DriveFolder].self, forKey: .folders) ?? []
        files = try c.decodeIfPresent([GalleryItem].self, forKey: .files) ?? []
        total = try c.decodeIfPresent(Int.self, forKey: .total)
        nextOffset = try c.decodeIfPresent(Int.self, forKey: .nextOffset)
    }

    private enum CodingKeys: String, CodingKey {
        case folder, path, breadcrumbs, folders, files, total
        case nextOffset = "next_offset"
    }
}

/// One highlighted token: `[class, text]` on the wire.
struct DriveToken: Decodable, Hashable, Sendable {
    /// "" plain, kw, str, com, num, fn, type, const, punct, tag, attr, var, head, em.
    let kind: String
    let text: String

    init(kind: String, text: String) {
        self.kind = kind
        self.text = text
    }

    /// `["kw", "fn"]`; a one-element array is plain text.
    init(from decoder: Decoder) throws {
        var c = try decoder.unkeyedContainer()
        let first = try c.decodeIfPresent(String.self) ?? ""
        if c.isAtEnd {
            kind = ""
            text = first
        } else {
            kind = first
            text = try c.decodeIfPresent(String.self) ?? ""
        }
    }
}

/// `GET /v1/media/items/{id}/text?highlight=1`.
struct DriveText: Decodable, Sendable {
    var language: String?
    /// file | extracted
    var source: String?
    var truncated: Bool
    var bytes: Int?
    var text: String
    /// Highlighted lines (code and text files only, the first 5 000 lines). Nil when missing or not
    /// in the expected shape: the viewer shows `text` plainly.
    var lines: [[DriveToken]]?

    init(language: String? = nil, source: String? = nil, truncated: Bool = false, bytes: Int? = nil, text: String,
         lines: [[DriveToken]]? = nil) {
        self.language = language
        self.source = source
        self.truncated = truncated
        self.bytes = bytes
        self.text = text
        self.lines = lines
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        language = try? c.decodeIfPresent(String.self, forKey: .language)
        source = try? c.decodeIfPresent(String.self, forKey: .source)
        truncated = (try? c.decodeIfPresent(Bool.self, forKey: .truncated)) ?? false
        bytes = try? c.decodeIfPresent(Int.self, forKey: .bytes)
        text = (try? c.decodeIfPresent(String.self, forKey: .text)) ?? ""
        lines = try? c.decodeIfPresent([[DriveToken]].self, forKey: .lines)
    }

    private enum CodingKeys: String, CodingKey { case language, source, truncated, bytes, text, lines }

    /// What the viewer draws: the highlighted lines, or the text split into plain lines. When the
    /// highlighting stopped early (5 000 lines) the rest of the text follows plainly.
    var displayLines: [[DriveToken]] {
        let plain = Self.plainLines(text)
        guard let lines, !lines.isEmpty else { return plain }
        if plain.count > lines.count { return lines + plain[lines.count...] }
        return lines
    }

    static func plainLines(_ text: String) -> [[DriveToken]] {
        guard !text.isEmpty else { return [] }
        // "\r\n" is one Character (a newline), so CRLF files split like LF ones.
        var parts = text.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline).map(String.init)
        if parts.count > 1, parts.last == "" { parts.removeLast() }
        return parts.map { [DriveToken(kind: "", text: $0)] }
    }
}

/// `GET /items/{id}/text` answers 202 `{"state":"pending"}` until the server has read the file.
enum DriveTextResult: Sendable {
    case ready(DriveText)
    case pending
}

/// `GET /v1/media/items/{id}/archive`.
struct DriveArchive: Decodable, Sendable {
    struct Entry: Decodable, Hashable, Sendable {
        let path: String
        var size: Int64?
        var dir: Bool?
        var modified: String?
    }

    var format: String?
    var total: Int?
    var truncated: Bool
    var entries: [Entry]

    init(format: String? = nil, total: Int? = nil, truncated: Bool = false, entries: [Entry]) {
        self.format = format
        self.total = total
        self.truncated = truncated
        self.entries = entries
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        format = try? c.decodeIfPresent(String.self, forKey: .format)
        total = try? c.decodeIfPresent(Int.self, forKey: .total)
        truncated = (try? c.decodeIfPresent(Bool.self, forKey: .truncated)) ?? false
        entries = try c.decodeIfPresent([Entry].self, forKey: .entries) ?? []
    }

    private enum CodingKeys: String, CodingKey { case format, total, truncated, entries }
}

extension GalleryAPI {
    /// The Drive roots. A server before the Drive browser answers 404: the screen keeps the album list.
    func driveRoots() async throws -> DriveRoots {
        try await client.get("/v1/media/drive")
    }

    func browse(_ query: DriveQuery) async throws -> DriveListing {
        try await client.get("/v1/media/drive/browse", query: query.queryItems, timeout: 30)
    }

    func browse(folder: String, path: String = "", sort: DriveSort? = nil, order: DriveOrder? = nil,
                type: DriveFileType? = nil, q: String = "", recursive: Bool = false, offset: Int = 0,
                limit: Int = 200) async throws -> DriveListing {
        try await browse(DriveQuery(folder: folder, path: path, sort: sort, order: order, type: type, q: q,
                                    recursive: recursive, offset: offset, limit: limit))
    }

    /// One item (a `/drive/file/<id>` link).
    func item(_ id: String) async throws -> GalleryItem {
        try await client.get("/v1/media/items/\(MediaAPI.encode(id))")
    }

    /// The file's text, highlighted (`urls.text?highlight=1`).
    func text(item: GalleryItem, highlight: Bool = true) async throws -> DriveTextResult {
        let link = DriveFiles.split(item.urls?.text ?? "/v1/media/items/\(MediaAPI.encode(item.id))/text")
        var query = link.query.filter { $0.name != "highlight" }
        if highlight { query.append(URLQueryItem(name: "highlight", value: "1")) }
        let response = try await client.send("GET", link.path, query: query, body: nil, timeout: 60)
        if response.status == 202 { return .pending }
        return .ready(try client.decode(DriveText.self, from: response))
    }

    /// The archive's listing. A format the server cannot list answers 415 (`DriveFiles.isUnsupported`).
    func archive(item: GalleryItem) async throws -> DriveArchive {
        let link = DriveFiles.split(item.urls?.archive ?? "/v1/media/items/\(MediaAPI.encode(item.id))/archive")
        return try await client.get(link.path, query: link.query, timeout: 60)
    }

    /// The file, downloaded once into the caches under its real name (Quick Look picks the type by the
    /// extension; Share and Save to Files keep the name). `pdf`: the server's PDF (`urls.pdf`) instead
    /// of the original.
    func download(item: GalleryItem, pdf: Bool = false) async throws -> URL {
        let link = pdf ? item.urls?.pdf : (item.urls?.download ?? item.urls?.original)
        guard let url = absolute(link) else { throw URLError(.badURL) }
        let target = DriveFiles.cacheURL(for: item, pdf: pdf)
        if DriveFiles.isCached(target, expectedSize: pdf ? nil : item.size) { return target }
        var request = URLRequest(url: url)
        if let key = client.accessKey { request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization") }
        let result: (URL, URLResponse)
        do {
            result = try await URLSession.shared.download(for: request)
        } catch let error as URLError where error.code == .cancelled {
            throw CancellationError()
        } catch let error as URLError {
            throw APIClient.map(error, host: url.host)
        }
        let (file, response) = result
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            try? FileManager.default.removeItem(at: file)
            throw APIError.server(status: status, code: "http_\(status)", message: "")
        }
        let dir = target.deletingLastPathComponent()
        try? FileManager.default.removeItem(at: dir)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        try FileManager.default.moveItem(at: file, to: target)
        return target
    }
}
