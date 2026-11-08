import XCTest
@testable import Secretary

final class NClientTests: XCTestCase {
    func testSearchResultsDecodeWebFixture() throws {
        let envelope = try Fixtures.decode(NClientEnvelope<NClientResults>.self, #"""
        {"ok":true,"data":{"items":[{"id":7,"title":"Placeholder volume","pages":6,"language":"english","tags":[],"thumbUrl":"/v1/nclient/s/7/thumb/0/123/abc123","favorite":false,"progress":null}],"page":2,"numPages":4}}
        """#)
        XCTAssertEqual(envelope.data.page, 2)
        XCTAssertEqual(envelope.data.numPages, 4)
        XCTAssertEqual(envelope.data.items.first?.id, 7)
        XCTAssertEqual(envelope.data.items.first?.pages, 6)
        XCTAssertEqual(envelope.data.items.first?.language, "english")
        XCTAssertNil(envelope.data.items.first?.progress)
    }

    func testDetailPagesRelatedAndOptionalTitles() throws {
        let detail = try Fixtures.decode(NClientDetail.self, #"""
        {"id":"7","title":"Placeholder volume","coverUrl":"/v1/nclient/s/7/cover/0/123/abc123",
         "titles":{"pretty":"Placeholder","english":"English title","japanese":"日本語"},"uploaded":123,"favorites":19,
         "tags":[{"id":"11","name":"color","type":"tag","count":1}],
         "pages":[{"index":0,"width":800,"height":1200,"url":"/v1/nclient/s/7/page/0/123/abc123","thumbUrl":"/v1/nclient/s/7/pthumb/0/123/abc123"}],
         "related":[{"id":8,"title":"Related volume","pages":6}],"progress":{"page":1,"pages":6,"updated_at":0}}
        """#)
        XCTAssertEqual(detail.id, 7)
        XCTAssertEqual(detail.pages.first?.index, 0)
        XCTAssertEqual(detail.pages.first?.width, 800)
        XCTAssertEqual(detail.pages.first?.height, 1200)
        XCTAssertNotNil(NClientImagePath.validated(detail.pages.first?.url))
        XCTAssertEqual(detail.related.first?.id, 8)
        XCTAssertEqual(detail.tags.first?.id, 11)
        XCTAssertEqual(detail.titles?.japanese, "日本語")
        XCTAssertEqual(detail.progress?.page, 1)
        XCTAssertEqual(detail.uploaded, 123)
        XCTAssertEqual(detail.favorites, 19)
        let sparse = try Fixtures.decode(NClientDetail.self, #"{"id":8,"titles":{"pretty":"Only pretty"},"language":null}"#)
        XCTAssertEqual(sparse.titles?.english, "")
        XCTAssertTrue(sparse.pages.isEmpty)
        XCTAssertNil(try Fixtures.decode(NClientDetail.self, "{}").titles)
    }

    func testMissingAndUnknownFieldsFallBack() throws {
        let result = try Fixtures.decode(NClientResults.self, #"{"items":null,"page":"2","numPages":"bad","extra":true}"#)
        XCTAssertEqual(result.items, [])
        XCTAssertEqual(result.page, 2)
        XCTAssertEqual(result.numPages, 1)
        let summary = try Fixtures.decode(NClientSummary.self, #"{"favorite":"yes","tags":{},"progress":"bad","language":6}"#)
        XCTAssertFalse(summary.favorite)
        XCTAssertTrue(summary.tags.isEmpty)
        XCTAssertNil(summary.progress)
        XCTAssertNil(summary.language)
    }

    func testStatusCapabilitiesAndDefaults() throws {
        let status = try Fixtures.decode(NClientStatus.self, #"{"enabled":true,"save":{"gallery":true,"library":false}}"#)
        XCTAssertTrue(status.enabled)
        XCTAssertEqual(status.save?.gallery, true)
        XCTAssertEqual(status.save?.library, false)
        XCTAssertFalse(try Fixtures.decode(NClientStatus.self, "{}").enabled)
        XCTAssertFalse(try Fixtures.decode(NClientStatus.self, #"{"enabled":"yes"}"#).enabled)
    }

    func testEverySaveStateAndCounts() throws {
        for state in ["queued", "running", "done", "failed", "cancelled", "future"] {
            let status = try Fixtures.decode(NClientSaveStatus.self, """
            {"job":"job1","state":"\(state)","total":6,"done":2,
             "saved":[{"index":0,"itemId":"a","duplicate":false},{"index":1,"itemId":42,"duplicate":true}],
             "failed":[{"index":2,"error":"unavailable"}],"error":"media_unavailable"}
            """)
            XCTAssertEqual(status.finished, !["queued", "running"].contains(state))
            XCTAssertEqual(status.duplicates, 1)
            XCTAssertEqual(status.failed.count, 1)
            XCTAssertEqual(status.saved[1].itemId, "42")
            XCTAssertEqual(status.error, "media_unavailable")
        }
        XCTAssertTrue(try Fixtures.decode(NClientSaveStatus.self, "{}").finished)
    }

    func testOnlyExactSignedPathsAreAccepted() {
        for kind in ["thumb", "cover", "page", "pthumb"] {
            let path = "/v1/nclient/s/7/\(kind)/0/123/abc_DEF-123"
            XCTAssertEqual(NClientImagePath.validated(path), path)
        }
        for path in ["https://example.org/image.jpg", "//example.org/image.jpg", "/v1/media/image",
                     "/v1/nclient/s/7/page/0/123/sig?redirect=x", "data:image/png;base64,x",
                     "/v1/nclient/s/7/page/0/123/sig#x", "/v1/nclient/s/7/page/0/123/sig\n", "/v1/nclient/s/7/page/0/123/sig\u{2028}",
                     "/v1/nclient/s/7/unknown/0/123/sig", "/v1/nclient/s/7/page/-1/123/sig", ""] {
            XCTAssertNil(NClientImagePath.validated(path), path)
        }
        XCTAssertNil(NClientImagePath.validated(nil))
    }

    func testQueryBuildingAllSortsAndOptionalFilters() {
        for sort in NClientSort.allCases {
            let query = NClientSearch(q: " a & b ", sort: sort, tags: [1, 2], exclude: [3]).query(page: 2)
            let values = Dictionary(uniqueKeysWithValues: query.map { ($0.name, $0.value ?? "") })
            XCTAssertEqual(values, ["q":"a & b", "page":"2", "sort":sort.rawValue, "tags":"1,2", "exclude":"3"])
        }
        XCTAssertEqual(Set(NClientSort.allCases.map(\.rawValue)), Set(["recent", "popular", "popular-week", "popular-today", "popular-month"]))
        XCTAssertEqual(NClientSearch().query(page: 1).count, 3)
        XCTAssertEqual(NClientAPI.jobPath("job/a?b#c"), "/saves/job%2Fa%3Fb%23c")
    }

    func testAbsoluteImagesStayOnPluginHost() {
        let client = APIClient(endpoints: [URL(string: "https://example.test")!], defaults: nil)
        let api = NClientAPI(client: client)
        XCTAssertEqual(api.imageURL("/v1/nclient/s/7/page/0/123/abc_DEF-123")?.absoluteString,
                       "https://example.test/v1/nclient/s/7/page/0/123/abc_DEF-123")
        XCTAssertNil(api.imageURL("https://upstream.test/image.jpg"))
        XCTAssertNil(api.imageURL(nil))
    }

    func testAPIUnwrapsAndUsesAuthenticatedPluginRoutes() async throws {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubURLProtocol.self]
        let session = URLSession(configuration: config)
        defer { session.invalidateAndCancel(); StubURLProtocol.handler = nil }
        StubURLProtocol.requests = []
        StubURLProtocol.handler = { request in
            let body: String
            switch request.url!.path {
            case "/v1/nclient/status": body = #"{"ok":true,"data":{"enabled":true}}"#
            case "/v1/nclient/search": body = #"{"ok":true,"data":{"items":[],"page":2,"numPages":3}}"#
            case "/v1/nclient/favorites/7": body = #"{"ok":true,"data":{"favorite":true}}"#
            case "/v1/nclient/galleries/7/save": body = #"{"ok":true,"data":{"job":"job1","state":"queued","total":6}}"#
            default: body = #"{"ok":true,"data":{}}"#
            }
            return StubURLProtocol.Reply(body: body)
        }
        let api = NClientAPI(client: APIClient(endpoints: [URL(string: "https://example.test")!], accessKey: "test", session: session, defaults: nil))
        let status = try await api.status()
        XCTAssertTrue(status.enabled)
        let result = try await api.search(NClientSearch(q: "a & b", sort: .week, tags: [1, 2], exclude: [3]), page: 2)
        XCTAssertEqual(result.numPages, 3)
        let favorite = try await api.favorite(7, value: true)
        XCTAssertTrue(favorite)
        try await api.progress(7, page: 2, pages: 6)
        let save = try await api.save(7)
        XCTAssertEqual(save.job, "job1")
        _ = try await api.saveStatus("job1")
        try await api.cancelSave("job1")
        let requests = StubURLProtocol.requests
        XCTAssertEqual(requests.map { $0.httpMethod ?? "" }, ["GET", "GET", "PUT", "PUT", "POST", "GET", "DELETE"])
        XCTAssertTrue(requests.allSatisfy { $0.value(forHTTPHeaderField: "Authorization") == "Bearer test" })
        let query = URLComponents(url: requests[1].url!, resolvingAgainstBaseURL: false)!.queryItems!
        XCTAssertEqual(query.first { $0.name == "tags" }?.value, "1,2")
        let progress = try JSONSerialization.jsonObject(with: XCTUnwrap(requests[3].httpBody)) as? [String: Int]
        XCTAssertEqual(progress, ["page":2, "pages":6])
        XCTAssertEqual(requests.last?.url?.path, "/v1/nclient/saves/job1")
    }

    func testProgressFractionUsesZeroBasedPageAndClamps() {
        XCTAssertEqual(NClientProgress(page: 0).fraction(total: 10), 0.1, accuracy: 0.0001)
        XCTAssertEqual(NClientProgress(page: 5).fraction(total: 6), 1)
        XCTAssertEqual(NClientProgress(page: 50).fraction(total: 6), 1)
        XCTAssertEqual(NClientProgress(page: -2).fraction(total: 6), 0)
        XCTAssertEqual(NClientProgress(page: 1).fraction(total: 0), 0)
        XCTAssertEqual(NClientProgress(page: Int.max).fraction(total: 1), 1)
    }

    func testProgressWritesWaitAndContinueAfterFailure() async throws {
        let writer = NClientProgressWriter()
        let sender = NClientTestSender()
        let first = Task { try await writer.write(id: 7) { try await sender.send(1) } }
        await sender.waitForFirst()
        let second = Task { try await writer.write(id: 7) { try await sender.send(5) } }
        // A different gallery is independent, even while the first request waits.
        try await writer.write(id: 8) { try await sender.send(8) }
        let blocked = await sender.recorded
        XCTAssertEqual(blocked, [1, 8])
        await sender.releaseFirst()
        do { try await first.value; XCTFail("The fake first request must fail") } catch { }
        try await second.value
        let finished = await sender.recorded
        XCTAssertEqual(finished, [1, 8, 5])
    }
}

/// A controlled fake sender: no network, clock, defaults or keychain.
private actor NClientTestSender {
    var recorded: [Int] = []
    private var waiting: CheckedContinuation<Void, Never>?
    private var started: CheckedContinuation<Void, Never>?
    private var didStart = false
    func send(_ page: Int) async throws {
        recorded.append(page)
        if page == 1 {
            await withCheckedContinuation { continuation in
                waiting = continuation; didStart = true
                started?.resume(); started = nil
            }
            throw URLError(.networkConnectionLost)
        }
    }
    func waitForFirst() async {
        if didStart { return }
        await withCheckedContinuation { started = $0 }
    }
    func releaseFirst() { waiting?.resume(); waiting = nil }
}
