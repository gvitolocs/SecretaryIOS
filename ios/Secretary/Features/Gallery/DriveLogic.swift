import Foundation
import SwiftUI

// The Drive browser's pure logic (tested in DriveTests): file types and their icons, the browse
// query, breadcrumbs, the navigation stack, `/drive/...` links, which preview opens a file, cache
// file names, syntax colours and the archive tree. The views in DriveViews/DriveBrowser/DrivePreviews
// only draw what these decide.

extension GalleryItem {
    /// The file's name in its folder (the title on servers before the Drive browser).
    var driveName: String {
        if let name = drive?.name, !name.isEmpty { return name }
        return title
    }
}

// MARK: - File types

/// `drive.type`: what a Drive file is, for its icon, colour and the type filter.
enum DriveFileType: String, CaseIterable, Hashable, Sendable {
    case pdf, document, spreadsheet, presentation, text, code, archive, image, video, audio, other

    /// The server's type, else (older servers) the kind and the extension.
    init(item: GalleryItem) {
        if let raw = item.drive?.type, let type = DriveFileType(rawValue: raw) {
            self = type
            return
        }
        switch item.kind {
        case "photo": self = .image
        case "video": self = .video
        case "audio": self = .audio
        default:
            let ext = item.drive?.ext ?? DriveFiles.ext(of: item.drive?.name ?? item.title)
            self = DriveFileType(ext: ext)
        }
    }

    /// A best guess from the extension (lower or upper case), for items without `drive.type`.
    init(ext raw: String) {
        let ext = raw.lowercased()
        if ext == "pdf" { self = .pdf; return }
        for (type, exts) in Self.extensions where exts.contains(ext) {
            self = type
            return
        }
        self = .other
    }

    private static let extensions: [(DriveFileType, Set<String>)] = [
        (.document, ["doc", "docx", "odt", "rtf", "pages", "dotx", "epub"]),
        (.spreadsheet, ["xls", "xlsx", "ods", "csv", "tsv", "numbers", "xlsm"]),
        (.presentation, ["ppt", "pptx", "odp", "key", "potx"]),
        (.text, ["txt", "md", "markdown", "log", "rst", "tex", "bib", "srt", "vtt", "nfo"]),
        (.code, ["swift", "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "java", "kt", "kts", "c", "h", "cpp", "hpp",
                 "cc", "cxx", "cs", "go", "rb", "php", "sh", "bash", "zsh", "fish", "ps1", "html", "htm", "css", "scss",
                 "less", "json", "jsonl", "yaml", "yml", "toml", "xml", "sql", "lua", "r", "m", "mm", "scala", "hs", "ex",
                 "exs", "erl", "clj", "dart", "vue", "svelte", "ini", "cfg", "conf", "gradle", "cmake", "proto",
                 "graphql", "ipynb", "pl", "zig", "nim", "jl", "asm", "s", "makefile", "dockerfile", "lock"]),
        (.archive, ["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst"]),
        (.image, ["jpg", "jpeg", "png", "gif", "heic", "heif", "webp", "tif", "tiff", "bmp", "svg", "avif", "jxl", "dng", "raw"]),
        (.video, ["mp4", "mov", "m4v", "mkv", "avi", "webm", "mpg", "mpeg", "wmv"]),
        (.audio, ["mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "aiff", "aif", "wma", "caf"]),
    ]

    /// The SF Symbol for a file of this type.
    var symbol: String {
        switch self {
        case .pdf: "doc.richtext"
        case .document: "doc.text"
        case .spreadsheet: "tablecells"
        case .presentation: "rectangle.on.rectangle.angled"
        case .text: "doc.plaintext"
        case .code: "chevron.left.forwardslash.chevron.right"
        case .archive: "archivebox"
        case .image: "photo"
        case .video: "film"
        case .audio: "waveform"
        case .other: "doc"
        }
    }

    /// The icon colour (0xRRGGBBAA, dark then light): readable on the cards in both modes.
    var tintHex: (dark: UInt32, light: UInt32) {
        switch self {
        case .pdf: (0xFF6961FF, 0xC62828FF)
        case .document: (0x64A8FFFF, 0x1E63D6FF)
        case .spreadsheet: (0x4CD07DFF, 0x1E8E3EFF)
        case .presentation: (0xFFB340FF, 0xC25E00FF)
        case .text: (0xAEAEB2FF, 0x636366FF)
        case .code: (0xBF8CFFFF, 0x7B3FD1FF)
        case .archive: (0xD4A373FF, 0x8B5E34FF)
        case .image: (0xFF7EB6FF, 0xC2185BFF)
        case .video: (0x7F8FE0FF, 0x4A5BB5FF)
        case .audio: (0x5AC8FAFF, 0x0A7EA4FF)
        case .other: (0x98989DFF, 0x6C6C70FF)
        }
    }

    var tint: Color { Color(lucinaDark: tintHex.dark, light: tintHex.light) }

    /// The type filter chip.
    var chipTitle: LocalizedStringKey {
        switch self {
        case .pdf: "PDF"
        case .document: "Documents"
        case .spreadsheet: "Spreadsheets"
        case .presentation: "Slides"
        case .text: "Text"
        case .code: "Code"
        case .archive: "Archives"
        case .image: "Images"
        case .video: "Video"
        case .audio: "Audio"
        case .other: "Other"
        }
    }
}

// MARK: - Browse query

enum DriveSort: String, CaseIterable, Hashable, Sendable {
    case name, modified, size, type

    /// The server's order when none is sent: A→Z for names and types, newest/biggest first otherwise.
    var defaultOrder: DriveOrder { self == .name || self == .type ? .asc : .desc }

    var title: LocalizedStringKey {
        switch self {
        case .name: "Name"
        case .modified: "Modified"
        case .size: "Size"
        case .type: "Type"
        }
    }
}

enum DriveOrder: String, CaseIterable, Hashable, Sendable {
    case asc, desc
}

/// `GET /v1/media/drive/browse`'s query. Only what is set is sent (the server has the defaults).
struct DriveQuery: Hashable, Sendable {
    var folder: String
    var path: String = ""
    var sort: DriveSort?
    var order: DriveOrder?
    var type: DriveFileType?
    var q: String = ""
    var recursive = false
    var offset = 0
    var limit = 200

    var trimmedQuery: String { q.trimmingCharacters(in: .whitespacesAndNewlines) }

    /// The sort the server applies: `modified` for a search, `name` otherwise.
    var effectiveSort: DriveSort { sort ?? (trimmedQuery.isEmpty ? .name : .modified) }
    var effectiveOrder: DriveOrder { order ?? effectiveSort.defaultOrder }

    /// The values go in `URLQueryItem`s: URLComponents percent-encodes them (spaces, `&`, `/`, accents).
    var queryItems: [URLQueryItem] {
        var items = [URLQueryItem(name: "folder", value: folder)]
        if !path.isEmpty { items.append(URLQueryItem(name: "path", value: path)) }
        if let sort { items.append(URLQueryItem(name: "sort", value: sort.rawValue)) }
        if let order { items.append(URLQueryItem(name: "order", value: order.rawValue)) }
        if let type { items.append(URLQueryItem(name: "type", value: type.rawValue)) }
        if !trimmedQuery.isEmpty { items.append(URLQueryItem(name: "q", value: trimmedQuery)) }
        if recursive { items.append(URLQueryItem(name: "recursive", value: "1")) }
        if offset > 0 { items.append(URLQueryItem(name: "offset", value: String(offset))) }
        items.append(URLQueryItem(name: "limit", value: String(limit)))
        return items
    }
}

// MARK: - Locations, breadcrumbs, navigation

/// A directory in a Drive root: `path` "" is the root itself.
struct DriveLocation: Hashable, Sendable {
    let folder: String
    let path: String

    init(folder: String, path: String = "") {
        self.folder = folder
        self.path = DriveBreadcrumbs.segments(path).joined(separator: "/")
    }

    /// The last path segment, or the root's label.
    var name: String { DriveBreadcrumbs.segments(path).last ?? folder }
}

enum DriveBreadcrumbs {
    /// The non-empty '/'-separated segments.
    static func segments(_ path: String) -> [String] {
        path.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
    }

    /// The root, then each directory down to `path`: Lezioni ("") › Digital Forensics › Lab.
    static func build(folder: String, path: String) -> [DriveCrumb] {
        var crumbs = [DriveCrumb(name: folder, path: "")]
        var current: [String] = []
        for segment in segments(path) {
            current.append(segment)
            crumbs.append(DriveCrumb(name: segment, path: current.joined(separator: "/")))
        }
        return crumbs
    }

    /// The server's breadcrumbs when it sent sensible ones (starting at the root), else built here.
    static func resolve(_ server: [DriveCrumb]?, folder: String, path: String) -> [DriveCrumb] {
        if let server, let first = server.first, first.path.isEmpty, server.last?.path == DriveLocation(folder: folder, path: path).path {
            return server
        }
        return build(folder: folder, path: path)
    }
}

/// What the Drive screen pushes: a folder level, or one of the old album lists (Voice memos).
enum DriveDestination: Hashable, Sendable {
    case folder(DriveLocation)
    case album(id: String, title: String)
}

/// The Drive's navigation stack (one pushed view per level, so the back gesture walks up).
enum DriveStack {
    /// Every level from the root down to `location`.
    static func chain(to location: DriveLocation) -> [DriveDestination] {
        DriveBreadcrumbs.build(folder: location.folder, path: location.path)
            .map { .folder(DriveLocation(folder: location.folder, path: $0.path)) }
    }

    /// A breadcrumb tap: back to that level when it is on the stack, else the stack is rebuilt to it.
    static func jump(_ stack: [DriveDestination], to location: DriveLocation) -> [DriveDestination] {
        if let index = stack.lastIndex(of: .folder(location)) { return Array(stack.prefix(index + 1)) }
        return chain(to: location)
    }

    /// The level at `index` was pushed (a value) or popped (nil, which pops everything above too).
    static func set(_ stack: [DriveDestination], at index: Int, to value: DriveDestination?) -> [DriveDestination] {
        guard index >= 0, index <= stack.count else { return stack }
        guard let value else { return Array(stack.prefix(index)) }
        // The same level again keeps what is above it.
        if index < stack.count, stack[index] == value { return stack }
        return Array(stack.prefix(index)) + [value]
    }
}

/// `/drive`, `/drive/folder/<folder>/<path…>`, `/drive/file/<item id>` (links in Lucina's chat).
enum DriveLink: Equatable, Sendable {
    case root
    case folder(DriveLocation)
    case file(String)

    /// Nil when the route is not a Drive route. Segments are percent-decoded; a malformed folder link
    /// (`..`, `.`) still opens the Drive.
    static func parse(_ route: String) -> DriveLink? {
        let bare = route.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? ""
        let segments = bare.split(separator: "/", omittingEmptySubsequences: true)
            .map { String($0).removingPercentEncoding ?? String($0) }
        guard segments.first == "drive" else { return nil }
        guard segments.count >= 3 else { return .root }
        switch segments[1] {
        case "folder":
            let rest = segments[3...].flatMap { DriveBreadcrumbs.segments($0) }
            let parts = [segments[2]] + rest
            guard !parts.contains(where: { $0 == "." || $0 == ".." || $0.contains("\\") || $0.contains("\0") }),
                  !segments[2].trimmingCharacters(in: .whitespaces).isEmpty else { return .root }
            return .folder(DriveLocation(folder: segments[2], path: rest.joined(separator: "/")))
        case "file":
            return .file(segments[2])
        default:
            return .root
        }
    }
}

// MARK: - Previews

/// What tapping a Drive file opens.
enum DriveOpen: Equatable, Sendable {
    /// The Gallery's viewer (photos and videos).
    case gallery
    /// The media player.
    case audio
    /// Quick Look on the downloaded file: the server's PDF (`urls.pdf`) or the original.
    case quickLook(pdf: Bool)
    /// `DriveTextViewer`.
    case text
    /// `DriveArchiveViewer`.
    case archive
    /// Name, type, size, folder, Download and Share.
    case info
    /// `SheetScreen`: an .xlsx to view and edit (docs/xlsx.md).
    case sheet

    static func decide(_ item: GalleryItem) -> DriveOpen {
        if DriveFiles.opensAsSheet(item) { return .sheet }
        let media: DriveOpen? = switch item.kind {
        case "photo", "video": .gallery
        case "audio": .audio
        default: nil
        }
        guard let preview = item.drive?.preview else {
            // Servers before the Drive browser: as the old Drive list did.
            return media ?? .quickLook(pdf: false)
        }
        switch preview {
        case "image", "video", "audio":
            return media ?? .quickLook(pdf: false)
        case "pdf":
            return .quickLook(pdf: item.urls?.pdf != nil)
        case "office":
            // Quick Look renders docx/xlsx/pptx natively: the original, unless there is only the PDF.
            return .quickLook(pdf: item.urls?.original == nil && item.urls?.download == nil && item.urls?.pdf != nil)
        case "text", "code":
            return .text
        case "archive":
            return .archive
        default:
            return media ?? .info
        }
    }
}

/// Downloaded files and server links.
enum DriveFiles {
    /// The workbooks the sheet editor opens (lucina-media `sheet_ext`); .xls, .ods and .numbers keep Quick Look.
    static let sheetExtensions: Set<String> = ["xlsx", "xlsm", "xltx", "xltm"]

    static func opensAsSheet(_ item: GalleryItem) -> Bool {
        item.kind == "file" && sheetExtensions.contains(ext(of: item.driveName))
    }

    /// A server link ("/v1/media/items/x/text?a=1") as the path and query `APIClient` takes.
    static func split(_ link: String) -> (path: String, query: [URLQueryItem]) {
        guard let components = URLComponents(string: link), !components.percentEncodedPath.isEmpty else { return (link, []) }
        return (components.percentEncodedPath, components.queryItems ?? [])
    }

    /// The lower-case extension of a file name ("" when none).
    static func ext(of name: String) -> String {
        guard let dot = name.lastIndex(of: "."), dot != name.startIndex, dot != name.index(before: name.endIndex) else { return "" }
        return name[name.index(after: dot)...].lowercased()
    }

    /// The file's real name, safe as one path component; the PDF of an Office file ends in .pdf.
    static func fileName(for item: GalleryItem, pdf: Bool = false) -> String {
        var name = item.driveName.replacingOccurrences(of: "/", with: "-")
            .replacingOccurrences(of: ":", with: "-")
            .replacingOccurrences(of: "\0", with: "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty || name == "." || name == ".." { name = safeComponent(item.id) }
        if pdf, ext(of: name) != "pdf" { name += ".pdf" }
        return name
    }

    static func safeComponent(_ raw: String) -> String {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_."))
        let cleaned = String(raw.unicodeScalars.map { allowed.contains($0) ? Character($0) : "_" })
        return cleaned.isEmpty || cleaned == "." || cleaned == ".." ? "item" : cleaned
    }

    /// Caches/drive-files/<item id>[-pdf]/<real name>.
    static func cacheURL(for item: GalleryItem, pdf: Bool = false,
                         base: URL = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]) -> URL {
        base.appendingPathComponent("drive-files", isDirectory: true)
            .appendingPathComponent(safeComponent(item.id) + (pdf ? "-pdf" : ""), isDirectory: true)
            .appendingPathComponent(fileName(for: item, pdf: pdf), isDirectory: false)
    }

    /// Downloaded before, completely (when the size is known).
    static func isCached(_ url: URL, expectedSize: Int?) -> Bool {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: url.path),
              let size = (attributes[.size] as? NSNumber)?.intValue else { return false }
        if let expectedSize { return size == expectedSize }
        return true
    }

    /// 415: an archive format the server cannot list.
    static func isUnsupported(_ error: Error) -> Bool { (error as? APIError)?.status == 415 }

    /// 404/405: an endpoint this server does not have yet (or a file without text).
    static func isMissing(_ error: Error) -> Bool { [404, 405].contains((error as? APIError)?.status ?? 0) }
}

// MARK: - Syntax colours

/// A highlighted token's class (`DriveToken.kind`) as a colour role.
enum DriveSyntaxRole: String, CaseIterable, Sendable {
    case plain, keyword, string, comment, number, function, type, constant, punctuation, tag, attribute, variable, heading, emphasis

    init(tokenClass: String) {
        switch tokenClass {
        case "kw": self = .keyword
        case "str": self = .string
        case "com": self = .comment
        case "num": self = .number
        case "fn": self = .function
        case "type": self = .type
        case "const": self = .constant
        case "punct": self = .punctuation
        case "tag": self = .tag
        case "attr": self = .attribute
        case "var": self = .variable
        case "head": self = .heading
        case "em": self = .emphasis
        default: self = .plain
        }
    }

    /// GitHub-like colours (0xRRGGBBAA, dark then light); nil: the text colour.
    var hex: (dark: UInt32, light: UInt32)? {
        switch self {
        case .plain: nil
        case .keyword: (0xFF7B72FF, 0xCF222EFF)
        case .string: (0xA5D6FFFF, 0x0A3069FF)
        case .comment: (0x8B949EFF, 0x6E7781FF)
        case .number, .constant: (0x79C0FFFF, 0x0550AEFF)
        case .function: (0xD2A8FFFF, 0x8250DFFF)
        case .type, .variable: (0xFFA657FF, 0x953800FF)
        case .punctuation: (0xB1BAC4FF, 0x57606AFF)
        case .tag: (0x7EE787FF, 0x116329FF)
        case .attribute: (0x79C0FFFF, 0x0550AEFF)
        case .heading: (0x79C0FFFF, 0x0550AEFF)
        case .emphasis: (0xD2A8FFFF, 0x8250DFFF)
        }
    }

    var color: Color { hex.map { Color(lucinaDark: $0.dark, light: $0.light) } ?? Theme.text }
    var isBold: Bool { self == .heading || self == .keyword }
    var isItalic: Bool { self == .comment || self == .emphasis }
}

// MARK: - Archive tree

/// An archive's entries as an indented tree: directories first, then files, each by name.
/// Directories the archive does not list on their own (most zips) are added.
enum DriveArchiveTree {
    struct Row: Hashable, Identifiable, Sendable {
        /// The full path within the archive.
        let id: String
        let name: String
        let depth: Int
        let isDir: Bool
        var size: Int64?
    }

    static func rows(_ entries: [DriveArchive.Entry]) -> [Row] {
        var dirs: Set<String> = []
        var sizes: [String: Int64] = [:]
        var children: [String: Set<String>] = [:]
        for entry in entries {
            let parts = DriveBreadcrumbs.segments(entry.path).filter { $0 != "." }
            guard !parts.isEmpty else { continue }
            let isDir = entry.dir == true || entry.path.hasSuffix("/")
            for i in 0..<parts.count {
                let path = parts[...i].joined(separator: "/")
                let parent = parts[..<i].joined(separator: "/")
                children[parent, default: []].insert(path)
                if i < parts.count - 1 || isDir {
                    dirs.insert(path)
                } else if let size = entry.size {
                    sizes[path] = size
                }
            }
        }
        // A name listed both as a file and as a parent directory is a directory.
        let files = sizes.filter { !dirs.contains($0.key) }

        var out: [Row] = []
        func walk(_ parent: String, depth: Int) {
            let kids = children[parent] ?? []
            let name = { (path: String) in String(path.split(separator: "/").last ?? Substring(path)) }
            let order = { (a: String, b: String) in
                name(a).compare(name(b), options: [.caseInsensitive, .numeric]) == .orderedAscending
            }
            for dir in kids.filter({ dirs.contains($0) }).sorted(by: order) {
                out.append(Row(id: dir, name: name(dir), depth: depth, isDir: true, size: nil))
                walk(dir, depth: depth + 1)
            }
            for file in kids.filter({ !dirs.contains($0) }).sorted(by: order) {
                out.append(Row(id: file, name: name(file), depth: depth, isDir: false, size: files[file]))
            }
        }
        walk("", depth: 0)
        return out
    }
}

// MARK: - Formatting

enum DriveFormat {
    static func bytes(_ count: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: count, countStyle: .file)
    }

    /// "1.2 MB · 4 Mar 2026" (plus the duration of audio and video).
    static func detail(_ item: GalleryItem) -> String {
        var parts: [String] = []
        if let ms = item.durationMs, item.isVideo || item.isAudio { parts.append(MediaFormat.time(TimeInterval(ms) / 1000)) }
        if let size = item.size { parts.append(bytes(Int64(size))) }
        if let date = item.drive?.modifiedDate ?? item.date { parts.append(date.formatted(date: .abbreviated, time: .omitted)) }
        return parts.joined(separator: " · ")
    }

    /// "12 files · 3.4 MB"
    static func folderDetail(files: Int?, bytes count: Int64?) -> String {
        var parts: [String] = []
        if let files { parts.append(String(localized: "\(files) files")) }
        if let count, count > 0 { parts.append(bytes(count)) }
        return parts.joined(separator: " · ")
    }

    /// Where a search result lives: "Lezioni › Digital Forensics".
    static func location(_ item: GalleryItem) -> String? {
        guard let folder = item.drive?.folder else { return nil }
        return ([folder] + DriveBreadcrumbs.segments(item.drive?.dir ?? "")).joined(separator: " › ")
    }
}
