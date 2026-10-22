import XCTest
@testable import Secretary

final class NotesTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    private let note = """
    {"id": "n_00000000000000000001", "slug": "craftit-meeting", "title": "CRAFT-IT4SD — meeting with Marianne & Jonas",
     "content": "# CRAFT-IT4SD — meeting with Marianne & Jonas\\n\\nMarianne offers **paid hours**.", "tags": ["work"],
     "source": "chatgpt", "pinned": true, "createdAt": "2026-10-22T11:30:00.123Z", "updatedAt": "2026-10-22T11:31:00.456Z"}
    """

    func testDecodesListsLenientlyAndNotes() async throws {
        StubURLProtocol.handler = { _ in
            .init(body: #"{"notes": [{"id": "n_1", "slug": "a", "title": "A", "excerpt": "# A *Hello*", "size": 9, "pinned": true, "updatedAt": "2026-10-22T11:31:00.456Z"}, {"broken": true}, {"id": "n_2", "slug": "b"}]}"#)
        }
        let api = NotesAPI(client: StubURLProtocol.makeClient())
        let list = try await api.list(query: " aarhus ", tag: "work")
        XCTAssertEqual(list.map(\.slug), ["a", "b"], "a bad entry does not hide the others")
        XCTAssertEqual(list[1].title, "b")
        XCTAssertEqual(list[1].source, "owner")
        XCTAssertNotNil(list[0].updated)
        XCTAssertEqual(list[0].plainExcerpt, "Hello")
        let url = try XCTUnwrap(StubURLProtocol.requests.last?.url)
        XCTAssertEqual(url.path, "/v1/notes")
        XCTAssertEqual(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems,
                       [URLQueryItem(name: "q", value: "aarhus"), URLQueryItem(name: "tag", value: "work")])

        let n = try Fixtures.decode(Note.self, note)
        XCTAssertEqual(n.slug, "craftit-meeting")
        XCTAssertTrue(n.pinned)
        XCTAssertEqual(NoteSource.label(n.source), "ChatGPT")
        XCTAssertNotNil(n.updated)
    }

    func testWritesCarryTheClientAndTheVersion() async throws {
        StubURLProtocol.handler = { [note] _ in .init(status: 200, body: note) }
        let api = NotesAPI(client: StubURLProtocol.makeClient())
        _ = try await api.create(NoteDraft(title: "Trip", content: "Rome", tags: parseNoteTags("Travel, #rome , travel")))
        var request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/v1/notes")
        XCTAssertEqual(request.value(forHTTPHeaderField: "X-Secretary-Client"), "ios")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: Any])
        XCTAssertEqual(body["title"] as? String, "Trip")
        XCTAssertEqual(body["tags"] as? [String], ["travel", "rome"])

        _ = try await api.update("n_00000000000000000001", .init(content: "x", baseUpdatedAt: "2026-10-22T11:31:00.456Z"))
        request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "PATCH")
        let patch = try XCTUnwrap(JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: Any])
        XCTAssertEqual(patch["baseUpdatedAt"] as? String, "2026-10-22T11:31:00.456Z")
        XCTAssertNil(patch["title"], "absent fields stay absent")

        StubURLProtocol.handler = { _ in .init(status: 409, body: #"{"error":{"code":"conflict","message":"changed"}}"#) }
        do {
            _ = try await api.update("a b/c", .init(title: "x"))
            XCTFail("expected a conflict")
        } catch let e as APIError {
            XCTAssertEqual(e.status, 409)
            XCTAssertEqual(e.code, "conflict")
        }
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.absoluteString, "https://example.test/v1/notes/a%20b%2Fc")
    }

    func testMarkdownBlocks() {
        let blocks = NoteText.parse("""
        # Title
        Intro **bold**
        line two

        ## Questions
        1. Excel
        2. Resources
          - CSV
        - [x] done
        - [ ] todo

        > quote

        ```
        let a = 1
        ```
        ---
        | Stage | Range |
        |---|--:|
        | Early | 0-30 |
        """)
        XCTAssertEqual(blocks.count, 8)
        XCTAssertEqual(blocks[0], .heading(level: 1, text: "Title"))
        XCTAssertEqual(blocks[1], .paragraph("Intro **bold**\nline two"))
        guard case let .list(items) = blocks[3] else { return XCTFail("list") }
        XCTAssertEqual(items.map(\.ordinal), [1, 2, nil, nil, nil])
        XCTAssertEqual(items[2].depth, 1)
        XCTAssertEqual(items.map(\.checked), [nil, nil, nil, true, false])
        XCTAssertEqual(blocks[4], .quote("quote"))
        XCTAssertEqual(blocks[5], .code("let a = 1"))
        XCTAssertEqual(blocks[6], .rule)
        XCTAssertEqual(blocks[7], .table(header: ["Stage", "Range"], rows: [["Early", "0-30"]]))
        XCTAssertEqual(NoteText.dropTitleHeading(blocks, title: " title ").first, .paragraph("Intro **bold**\nline two"))
        XCTAssertEqual(NoteText.dropTitleHeading(blocks, title: "Other").count, 8)
    }

    func testInlineLinksAreOnlyWebOrMail() {
        func links(_ s: String) -> [String] {
            NoteText.inline(s).runs.compactMap { $0.link?.absoluteString }
        }
        XCTAssertEqual(links("[site](https://craft-it4sd.eu/)"), ["https://craft-it4sd.eu/"])
        XCTAssertEqual(links("[x](javascript:alert(1))"), [])
        XCTAssertEqual(links("[x](file:///etc/passwd)"), [])
        XCTAssertEqual(links("[me](mailto:owner@example.com)"), ["mailto:owner@example.com"])
        XCTAssertEqual(links("see https://example.com/a now"), ["https://example.com/a"])
        XCTAssertEqual(String(NoteText.inline("<script>alert(1)</script>").characters), "<script>alert(1)</script>")
        XCTAssertEqual(String(NoteText.inline("**bold** and *it*").characters), "bold and it")
    }

    func testPlainExcerpt() {
        XCTAssertEqual(NoteText.plainExcerpt("# CRAFT — meeting *Briefing di ChatGPT* per Giuseppe", title: "CRAFT — meeting"),
                       "Briefing di ChatGPT per Giuseppe")
        XCTAssertEqual(NoteText.plainExcerpt("Saved. - **Mamma**: a book (*Il cucchiaio*) - [ ] Check", title: ""),
                       "Saved. Mamma: a book (Il cucchiaio) Check")
        XCTAssertEqual(NoteText.plainExcerpt("snake_case_name stays", title: ""), "snake_case_name stays")
        XCTAssertEqual(NoteText.plainExcerpt("a cookbook (*Il cucchiaio d'argento*) Marco", title: ""),
                       "a cookbook (Il cucchiaio d'argento) Marco")
    }

    @MainActor
    func testLiveTimelineItemsRefreshTheList() async throws {
        var lists = 0
        StubURLProtocol.handler = { _ in
            lists += 1
            return .init(body: #"{"notes": []}"#)
        }
        let client = StubURLProtocol.makeClient()
        let store = NotesStore(api: { NotesAPI(client: client) })
        let item = { (id: String, kind: String?, age: TimeInterval, role: TimelineItem.Role) in
            TimelineItem(id: id, ts: Date().addingTimeInterval(-age), channel: .web, role: role, text: "x",
                         meta: kind.map { TimelineMeta(kind: $0) })
        }
        store.consider([item("1", "reminder", 0, .lucina), item("2", "note", 0, .user), item("3", "note", 3600, .lucina)])
        XCTAssertEqual(store.version, 0, "not a fresh note item from Lucina")
        store.consider([item("4", "note", 1, .lucina)])
        XCTAssertEqual(store.version, 1)
        store.consider([item("4", "note", 1, .lucina)])
        XCTAssertEqual(store.version, 1, "each item counts once")
        for _ in 0..<50 where lists == 0 { try await Task.sleep(for: .milliseconds(20)) }
        XCTAssertEqual(lists, 1)
    }
}
