import XCTest
@testable import Secretary

/// The Drive browser's pure logic (Features/Gallery/DriveLogic.swift) and wire types (DriveAPI.swift).
final class DriveTests: XCTestCase {
    private func item(_ json: String) throws -> GalleryItem {
        try JSONDecoder().decode(GalleryItem.self, from: Data(json.utf8))
    }

    private func file(kind: String = "file", title: String = "x", drive: String? = nil, urls: String = #"{"original":"/v1/media/items/itm_1/original"}"#) throws -> GalleryItem {
        var json = #"{"id":"itm_1","kind":""# + kind + #"","title":""# + title + #"","urls":"# + urls
        if let drive { json += #","drive":"# + drive }
        return try item(json + "}")
    }

    // MARK: Wire types

    func testItemDecodesDriveFieldsAndOlderServersStillDecode() throws {
        let pdf = try item("""
        {"id":"itm_1","kind":"file","title":"L05 slides.pdf","mime":"application/pdf","size":1234567,
         "created_at":"2026-03-04T10:00:00.000Z","thumb_state":"ready","thumbhash":null,
         "urls":{"original":"/v1/media/items/itm_1/original","download":"/v1/media/items/itm_1/original?download=1",
                 "thumb":null,"grid":"/v1/media/s/t/grid","pdf":"/v1/media/items/itm_1/pdf","text":"/v1/media/items/itm_1/text",
                 "archive":null,"future":"x"},
         "drive":{"folder":"Lezioni","path":"Digital Forensics/L05 slides.pdf","dir":"Digital Forensics","name":"L05 slides.pdf",
                  "ext":"pdf","type":"pdf","preview":"pdf","language":null,"modified":"2026-03-04T10:00:00.000Z","text":"ready",
                  "future":1},
         "snippet":"…the chain of custody…"}
        """)
        XCTAssertEqual(pdf.drive?.folder, "Lezioni")
        XCTAssertEqual(pdf.drive?.dir, "Digital Forensics")
        XCTAssertEqual(pdf.drive?.textState, "ready")
        XCTAssertNotNil(pdf.drive?.modifiedDate)
        XCTAssertEqual(pdf.urls?.download, "/v1/media/items/itm_1/original?download=1")
        XCTAssertEqual(pdf.urls?.pdf, "/v1/media/items/itm_1/pdf")
        XCTAssertEqual(pdf.urls?.text, "/v1/media/items/itm_1/text")
        XCTAssertNil(pdf.urls?.archive)
        XCTAssertEqual(pdf.snippet, "…the chain of custody…")

        let old = try item(#"{"id":"itm_2","kind":"photo","title":"IMG_1.HEIC","urls":{"original":"/o"}}"#)
        XCTAssertNil(old.drive)
        XCTAssertNil(old.snippet)
        XCTAssertNil(old.urls?.download)
        XCTAssertEqual(old.driveName, "IMG_1.HEIC")
        XCTAssertEqual(pdf.driveName, "L05 slides.pdf")
    }

    func testRootsAndListingDecodeWithMissingParts() throws {
        let roots = try JSONDecoder().decode(DriveRoots.self, from: Data("""
        {"folders":[{"folder":"Lezioni","files":87,"bytes":11811160064,"modified":"2026-10-20T10:00:00Z"},{"folder":"Tracer"}],
         "albums":[{"id":"sys-voice-memos","title":"Voice memos","count":12}],
         "converters":{"pdf_thumbs":true,"office_pdf":false}}
        """.utf8))
        XCTAssertEqual(roots.folders.map(\.folder), ["Lezioni", "Tracer"])
        XCTAssertEqual(roots.folders[0].bytes, 11_811_160_064)
        XCTAssertEqual(roots.albums.first?.title, "Voice memos")
        XCTAssertEqual(roots.converters?["office_pdf"], false)
        let bare = try JSONDecoder().decode(DriveRoots.self, from: Data(#"{"converters":"odd"}"#.utf8))
        XCTAssertTrue(bare.folders.isEmpty)
        XCTAssertTrue(bare.albums.isEmpty)
        XCTAssertNil(bare.converters)

        let listing = try JSONDecoder().decode(DriveListing.self, from: Data("""
        {"folder":"Lezioni","path":"Digital Forensics",
         "breadcrumbs":[{"name":"Lezioni","path":""},{"name":"Digital Forensics","path":"Digital Forensics"}],
         "folders":[{"name":"Lab","path":"Digital Forensics/Lab","files":12,"bytes":3456}],
         "files":[{"id":"itm_1","kind":"file","title":"a.pdf"}],"total":87,"next_offset":200}
        """.utf8))
        XCTAssertEqual(listing.folders.first?.path, "Digital Forensics/Lab")
        XCTAssertEqual(listing.files.count, 1)
        XCTAssertEqual(listing.total, 87)
        XCTAssertEqual(listing.nextOffset, 200)
        XCTAssertEqual(listing.breadcrumbs?.count, 2)
        let last = try JSONDecoder().decode(DriveListing.self, from: Data(#"{"files":[],"next_offset":null}"#.utf8))
        XCTAssertNil(last.nextOffset)
        XCTAssertTrue(last.folders.isEmpty)
    }

    // MARK: Token lines

    func testTokenLinesDecodeFromClassTextPairs() throws {
        let text = try JSONDecoder().decode(DriveText.self, from: Data("""
        {"language":"rust","source":"file","truncated":false,"bytes":30,"text":"fn main() {}\\n// hi\\n",
         "lines":[[["kw","fn"],[""," "],["fn","main"],["punct","()"],[""," "],["punct","{}"]],[["com","// hi"]],[]]}
        """.utf8))
        let lines = try XCTUnwrap(text.lines)
        XCTAssertEqual(lines.count, 3)
        XCTAssertEqual(lines[0].first, DriveToken(kind: "kw", text: "fn"))
        XCTAssertEqual(lines[0].map(\.text).joined(), "fn main() {}")
        XCTAssertEqual(lines[1], [DriveToken(kind: "com", text: "// hi")])
        XCTAssertTrue(lines[2].isEmpty)
        XCTAssertEqual(text.language, "rust")
        XCTAssertFalse(text.truncated)
    }

    func testOneElementTokenIsPlainAndMalformedLinesFallBackToText() throws {
        let one = try JSONDecoder().decode(DriveText.self, from: Data(#"{"text":"a","lines":[[["a"]]]}"#.utf8))
        XCTAssertEqual(one.lines?.first, [DriveToken(kind: "", text: "a")])

        let bad = try JSONDecoder().decode(DriveText.self, from: Data(#"{"text":"one\r\ntwo\n","truncated":true,"lines":"nope"}"#.utf8))
        XCTAssertNil(bad.lines)
        XCTAssertTrue(bad.truncated)
        XCTAssertEqual(bad.displayLines, [[DriveToken(kind: "", text: "one")], [DriveToken(kind: "", text: "two")]])

        let plain = try JSONDecoder().decode(DriveText.self, from: Data(#"{"text":"x\n\ny"}"#.utf8))
        XCTAssertEqual(plain.displayLines.map { $0.map(\.text).joined() }, ["x", "", "y"])
        XCTAssertTrue(DriveText(text: "").displayLines.isEmpty)
    }

    func testHighlightingThatStoppedEarlyContinuesPlainly() {
        let text = DriveText(text: "a\nb\nc", lines: [[DriveToken(kind: "kw", text: "a")]])
        XCTAssertEqual(text.displayLines, [[DriveToken(kind: "kw", text: "a")], [DriveToken(kind: "", text: "b")], [DriveToken(kind: "", text: "c")]])
    }

    func testTokenClassesMapToColourRoles() {
        XCTAssertEqual(DriveSyntaxRole(tokenClass: "kw"), .keyword)
        XCTAssertEqual(DriveSyntaxRole(tokenClass: "str"), .string)
        XCTAssertEqual(DriveSyntaxRole(tokenClass: "com"), .comment)
        XCTAssertEqual(DriveSyntaxRole(tokenClass: "head"), .heading)
        XCTAssertEqual(DriveSyntaxRole(tokenClass: ""), .plain)
        XCTAssertEqual(DriveSyntaxRole(tokenClass: "something-new"), .plain)
        XCTAssertNil(DriveSyntaxRole.plain.hex)
        for role in DriveSyntaxRole.allCases where role != .plain {
            guard let hex = role.hex else { return XCTFail("\(role) has no colour") }
            XCTAssertNotEqual(hex.dark, hex.light, "\(role) differs between dark and light")
        }
        XCTAssertTrue(DriveSyntaxRole.comment.isItalic)
    }

    // MARK: Types and icons

    func testTypeIconsAndFallbacks() throws {
        XCTAssertEqual(DriveFileType.pdf.symbol, "doc.richtext")
        XCTAssertEqual(DriveFileType.document.symbol, "doc.text")
        XCTAssertEqual(DriveFileType.spreadsheet.symbol, "tablecells")
        XCTAssertEqual(DriveFileType.presentation.symbol, "rectangle.on.rectangle.angled")
        XCTAssertEqual(DriveFileType.text.symbol, "doc.plaintext")
        XCTAssertEqual(DriveFileType.code.symbol, "chevron.left.forwardslash.chevron.right")
        XCTAssertEqual(DriveFileType.archive.symbol, "archivebox")
        XCTAssertEqual(DriveFileType.image.symbol, "photo")
        XCTAssertEqual(DriveFileType.video.symbol, "film")
        XCTAssertEqual(DriveFileType.audio.symbol, "waveform")
        XCTAssertEqual(DriveFileType.other.symbol, "doc")
        XCTAssertEqual(Set(DriveFileType.allCases.map { $0.tintHex.light }).count, DriveFileType.allCases.count, "one colour per type")

        XCTAssertEqual(DriveFileType(item: try file(title: "a.bin", drive: #"{"type":"spreadsheet"}"#)), .spreadsheet)
        XCTAssertEqual(DriveFileType(item: try file(title: "main.rs", drive: #"{"type":"future-type","ext":"rs"}"#)), .code)
        XCTAssertEqual(DriveFileType(item: try file(kind: "audio", title: "memo.m4a")), .audio)
        XCTAssertEqual(DriveFileType(item: try file(kind: "photo", title: "x")), .image)
        XCTAssertEqual(DriveFileType(item: try file(title: "Slides.PPTX")), .presentation)
        XCTAssertEqual(DriveFileType(item: try file(title: "README")), .other)
        XCTAssertEqual(DriveFileType(ext: "tgz"), .archive)
        XCTAssertEqual(DriveFileType(ext: "PDF"), .pdf)
    }

    // MARK: Query

    func testQuerySendsOnlyWhatIsSetAndEncodesValues() throws {
        let plain = DriveQuery(folder: "Lezioni")
        XCTAssertEqual(plain.queryItems.map(\.name), ["folder", "limit"])
        XCTAssertEqual(plain.effectiveSort, .name)
        XCTAssertEqual(plain.effectiveOrder, .asc)

        var full = DriveQuery(folder: "Lezioni & co", path: "Digital Forensics/Lab #2", sort: .size, order: nil, type: .presentation,
                              q: "  chain of custody ", recursive: true, offset: 200, limit: 100)
        XCTAssertEqual(full.queryItems.map(\.name), ["folder", "path", "sort", "type", "q", "recursive", "offset", "limit"])
        XCTAssertEqual(full.queryItems.first { $0.name == "q" }?.value, "chain of custody")
        XCTAssertEqual(full.effectiveOrder, .desc, "size defaults to biggest first")
        full.order = .asc
        XCTAssertEqual(full.queryItems.first { $0.name == "order" }?.value, "asc")

        var components = try XCTUnwrap(URLComponents(string: "https://example.test/v1/media/drive/browse"))
        components.queryItems = full.queryItems
        let query = try XCTUnwrap(components.percentEncodedQuery)
        XCTAssertTrue(query.contains("folder=Lezioni%20%26%20co"), query)
        XCTAssertTrue(query.contains("path=Digital%20Forensics/Lab%20%232"), query)
        XCTAssertEqual(components.queryItems?.first { $0.name == "path" }?.value, "Digital Forensics/Lab #2")

        let search = DriveQuery(folder: "Tracer", q: "parser")
        XCTAssertEqual(search.effectiveSort, .modified)
        XCTAssertEqual(search.effectiveOrder, .desc)
        XCTAssertEqual(DriveSort.type.defaultOrder, .asc)
    }

    // MARK: Breadcrumbs and navigation

    func testBreadcrumbsFromAPath() {
        XCTAssertEqual(DriveBreadcrumbs.build(folder: "Lezioni", path: ""), [DriveCrumb(name: "Lezioni", path: "")])
        XCTAssertEqual(DriveBreadcrumbs.build(folder: "Lezioni", path: "Digital Forensics/Lab/"), [
            DriveCrumb(name: "Lezioni", path: ""),
            DriveCrumb(name: "Digital Forensics", path: "Digital Forensics"),
            DriveCrumb(name: "Lab", path: "Digital Forensics/Lab"),
        ])
        XCTAssertEqual(DriveBreadcrumbs.build(folder: "T", path: "//a//b").map(\.path), ["", "a", "a/b"])
        let server = [DriveCrumb(name: "Lezioni", path: ""), DriveCrumb(name: "DF", path: "DF")]
        XCTAssertEqual(DriveBreadcrumbs.resolve(server, folder: "Lezioni", path: "DF"), server)
        XCTAssertEqual(DriveBreadcrumbs.resolve(nil, folder: "Lezioni", path: "DF").map(\.name), ["Lezioni", "DF"])
        XCTAssertEqual(DriveBreadcrumbs.resolve(server, folder: "Lezioni", path: "Other").map(\.path), ["", "Other"])
        XCTAssertEqual(DriveLocation(folder: "Lezioni", path: "/a/b/").path, "a/b")
        XCTAssertEqual(DriveLocation(folder: "Lezioni", path: "a/b").name, "b")
        XCTAssertEqual(DriveLocation(folder: "Lezioni").name, "Lezioni")
    }

    func testStackPushesPopsAndJumps() {
        let root = DriveLocation(folder: "L")
        let a = DriveLocation(folder: "L", path: "a")
        let ab = DriveLocation(folder: "L", path: "a/b")
        XCTAssertEqual(DriveStack.chain(to: ab), [.folder(root), .folder(a), .folder(ab)])

        var stack: [DriveDestination] = []
        stack = DriveStack.set(stack, at: 0, to: .folder(root))
        stack = DriveStack.set(stack, at: 1, to: .folder(a))
        stack = DriveStack.set(stack, at: 2, to: .folder(ab))
        XCTAssertEqual(stack.count, 3)
        XCTAssertEqual(DriveStack.set(stack, at: 1, to: .folder(a)), stack, "the same level keeps what is above")
        XCTAssertEqual(DriveStack.set(stack, at: 2, to: nil), [.folder(root), .folder(a)], "back pops one level")
        XCTAssertEqual(DriveStack.set(stack, at: 1, to: nil), [.folder(root)])
        XCTAssertEqual(DriveStack.set(stack, at: 5, to: .folder(a)), stack, "no gaps")
        XCTAssertEqual(DriveStack.set(stack, at: 1, to: .album(id: "x", title: "X")), [.folder(root), .album(id: "x", title: "X")])

        XCTAssertEqual(DriveStack.jump(stack, to: root), [.folder(root)], "a breadcrumb jumps several levels")
        XCTAssertEqual(DriveStack.jump(stack, to: a), [.folder(root), .folder(a)])
        XCTAssertEqual(DriveStack.jump([], to: ab), DriveStack.chain(to: ab), "not on the stack: rebuilt")
    }

    func testDriveRoutes() {
        XCTAssertEqual(DriveLink.parse("/drive"), .root)
        XCTAssertEqual(DriveLink.parse("/drive/"), .root)
        XCTAssertEqual(DriveLink.parse("/drive/folder/Lezioni"), .folder(DriveLocation(folder: "Lezioni")))
        XCTAssertEqual(DriveLink.parse("/drive/folder/Lezioni/Digital%20Forensics/Lab"),
                       .folder(DriveLocation(folder: "Lezioni", path: "Digital Forensics/Lab")))
        XCTAssertEqual(DriveLink.parse("/drive/folder/Tracer/src%2Fparser?from=chat"),
                       .folder(DriveLocation(folder: "Tracer", path: "src/parser")))
        XCTAssertEqual(DriveLink.parse("/drive/file/itm_01K%2B"), .file("itm_01K+"))
        XCTAssertEqual(DriveLink.parse("/drive/folder/Lezioni/../secret"), .root)
        XCTAssertEqual(DriveLink.parse("/drive/folder/Lezioni/a%2F..%2Fb"), .root)
        XCTAssertEqual(DriveLink.parse("/drive/unknown/x"), .root)
        XCTAssertEqual(DriveLink.parse("/drive/file"), .root)
        XCTAssertNil(DriveLink.parse("/gallery/album/x"))
        XCTAssertNil(DriveLink.parse("/drives"))
        XCTAssertEqual(NotificationRoute.tab(for: "/drive/file/itm_1"), .plugins)
    }

    // MARK: Previews and files

    func testWhichPreviewOpens() throws {
        XCTAssertEqual(DriveOpen.decide(try file(kind: "photo")), .gallery, "older server: by kind")
        XCTAssertEqual(DriveOpen.decide(try file(kind: "audio")), .audio)
        XCTAssertEqual(DriveOpen.decide(try file()), .quickLook(pdf: false))
        let withPDF = #"{"original":"/o","pdf":"/p"}"#
        XCTAssertEqual(DriveOpen.decide(try file(drive: #"{"preview":"pdf"}"#, urls: withPDF)), .quickLook(pdf: true))
        XCTAssertEqual(DriveOpen.decide(try file(drive: #"{"preview":"pdf"}"#)), .quickLook(pdf: false))
        XCTAssertEqual(DriveOpen.decide(try file(drive: #"{"preview":"office"}"#, urls: withPDF)), .quickLook(pdf: false),
                       "Quick Look renders Office files: the original")
        XCTAssertEqual(DriveOpen.decide(try file(drive: #"{"preview":"code"}"#)), .text)
        XCTAssertEqual(DriveOpen.decide(try file(drive: #"{"preview":"text"}"#)), .text)
        XCTAssertEqual(DriveOpen.decide(try file(drive: #"{"preview":"archive"}"#)), .archive)
        XCTAssertEqual(DriveOpen.decide(try file(drive: #"{"preview":"none"}"#)), .info)
        XCTAssertEqual(DriveOpen.decide(try file(kind: "video", drive: #"{"preview":"video"}"#)), .gallery)
        XCTAssertEqual(DriveOpen.decide(try file(drive: #"{"preview":"image"}"#)), .quickLook(pdf: false), "an image without renditions")
    }

    func testDownloadNamesAndServerLinks() throws {
        let slides = try file(title: "title", drive: #"{"name":"L05: slides/v2.pptx"}"#)
        XCTAssertEqual(DriveFiles.fileName(for: slides), "L05- slides-v2.pptx")
        XCTAssertEqual(DriveFiles.fileName(for: slides, pdf: true), "L05- slides-v2.pptx.pdf")
        XCTAssertEqual(DriveFiles.fileName(for: try file(title: "a.pdf"), pdf: true), "a.pdf")
        XCTAssertEqual(DriveFiles.fileName(for: try file(title: "  ")), "itm_1")
        let base = URL(fileURLWithPath: "/tmp/caches")
        XCTAssertEqual(DriveFiles.cacheURL(for: slides, base: base).path, "/tmp/caches/drive-files/itm_1/L05- slides-v2.pptx")
        XCTAssertEqual(DriveFiles.cacheURL(for: slides, pdf: true, base: base).deletingLastPathComponent().lastPathComponent, "itm_1-pdf")
        XCTAssertEqual(DriveFiles.safeComponent("../x y"), ".._x_y")
        XCTAssertEqual(DriveFiles.ext(of: "a.tar.GZ"), "gz")
        XCTAssertEqual(DriveFiles.ext(of: ".env"), "")
        XCTAssertEqual(DriveFiles.ext(of: "README"), "")

        let link = DriveFiles.split("/v1/media/items/itm%201/text?max_bytes=10")
        XCTAssertEqual(link.path, "/v1/media/items/itm%201/text")
        XCTAssertEqual(link.query, [URLQueryItem(name: "max_bytes", value: "10")])
        XCTAssertTrue(DriveFiles.split("/v1/media/items/x/archive").query.isEmpty)

        XCTAssertTrue(DriveFiles.isUnsupported(APIError.server(status: 415, code: "http_415", message: "")))
        XCTAssertFalse(DriveFiles.isUnsupported(APIError.timeout))
        XCTAssertTrue(DriveFiles.isMissing(APIError.server(status: 404, code: "http_404", message: "")))
    }

    func testCachedFileIsReusedOnlyWhenComplete() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("drive-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let url = dir.appendingPathComponent("a.pdf")
        XCTAssertFalse(DriveFiles.isCached(url, expectedSize: nil))
        try Data(repeating: 1, count: 10).write(to: url)
        XCTAssertTrue(DriveFiles.isCached(url, expectedSize: 10))
        XCTAssertFalse(DriveFiles.isCached(url, expectedSize: 11))
        XCTAssertTrue(DriveFiles.isCached(url, expectedSize: nil))
    }

    // MARK: Archives

    func testArchiveTreeAddsDirectoriesAndSortsDirectoriesFirst() {
        let entries = [
            DriveArchive.Entry(path: "src/main.rs", size: 1234),
            DriveArchive.Entry(path: "README.md", size: 10),
            DriveArchive.Entry(path: "src/lib/b.rs", size: 2),
            DriveArchive.Entry(path: "src/lib/a10.rs", size: 3),
            DriveArchive.Entry(path: "src/lib/a2.rs", size: 4),
            DriveArchive.Entry(path: "docs/", size: nil, dir: true),
            DriveArchive.Entry(path: "./Cargo.toml", size: 5),
        ]
        let rows = DriveArchiveTree.rows(entries)
        XCTAssertEqual(rows.map(\.id), ["docs", "src", "src/lib", "src/lib/a2.rs", "src/lib/a10.rs", "src/lib/b.rs", "src/main.rs",
                                        "Cargo.toml", "README.md"])
        XCTAssertEqual(rows.map(\.depth), [0, 0, 1, 2, 2, 2, 1, 0, 0])
        XCTAssertEqual(rows.first { $0.id == "src/main.rs" }?.size, 1234)
        XCTAssertEqual(rows.first { $0.id == "src" }?.isDir, true)
        XCTAssertNil(rows.first { $0.id == "src" }?.size)

        let archive = try? JSONDecoder().decode(DriveArchive.self, from: Data("""
        {"format":"zip","total":2,"truncated":true,"entries":[{"path":"a","size":1,"dir":false,"modified":null},{"path":"b/","dir":true}]}
        """.utf8))
        XCTAssertEqual(archive?.entries.count, 2)
        XCTAssertEqual(archive?.truncated, true)
        XCTAssertEqual(archive.map { DriveArchiveTree.rows($0.entries).map(\.id) }, ["b", "a"])
    }

    // MARK: Formatting

    func testRowDetailAndLocation() throws {
        let entry = try item("""
        {"id":"itm_1","kind":"file","title":"x","size":2048,
         "drive":{"folder":"Lezioni","dir":"Digital Forensics/Lab","modified":"2026-03-04T10:00:00.000Z"}}
        """)
        XCTAssertTrue(DriveFormat.detail(entry).contains(" · "))
        XCTAssertEqual(DriveFormat.location(entry), "Lezioni › Digital Forensics › Lab")
        XCTAssertNil(DriveFormat.location(try file()))
        XCTAssertEqual(DriveFormat.folderDetail(files: nil, bytes: nil), "")
    }
}
