import XCTest
@testable import Secretary

@MainActor
final class PixivBrowseTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    func testListingDefaultsAndLossyFields() throws {
        let listing = try Fixtures.decode(PixivListing.self, #"{"items":[{"id":"1","pages":0,"popularTier":1000,"tags":["sky",3]},{"nope":1},{"id":12,"pages":-5,"popularTier":"1000"},null],"page":2,"hasMore":true,"notice":"r18_needs_session","sort":{"requested":"popular","used":"users_tags"}}"#)
        XCTAssertEqual(listing.items.map(\.id), ["1", "12"])
        XCTAssertEqual(listing.items.map(\.pages), [1, 1])
        XCTAssertEqual(listing.items[0].popularTier, 1000)
        XCTAssertNil(listing.items[1].popularTier)
        XCTAssertEqual(listing.items[0].tags, ["sky"])
        XCTAssertEqual(listing.notice, "r18_needs_session")
        XCTAssertEqual(listing.sort, .init(requested: "popular", used: "users_tags"))
        XCTAssertEqual(listing.page, 2)
        XCTAssertTrue(listing.hasMore)
        XCTAssertNil(listing.total)
        XCTAssertNil(listing.numPages)
        let empty = try Fixtures.decode(PixivListing.self, "null")
        XCTAssertTrue(empty.items.isEmpty)
        XCTAssertEqual(empty.page, 1)
        XCTAssertFalse(empty.hasMore)
        for json in [#"{"notice":3,"sort":"popular"}"#, #"{"notice":"","sort":{"requested":"popular"}}"#, #"{"items":"bad","sort":[]}"#] {
            let junk = try Fixtures.decode(PixivListing.self, json)
            XCTAssertNil(junk.notice)
            XCTAssertNil(junk.sort)
        }
        XCTAssertEqual(try Fixtures.decode(PixivListing.self, #"{"sort":{"used":"date_d"}}"#).sort, .init(requested: "", used: "date_d"))
        for tier in ["0", "-5", "null", "{}"] {
            XCTAssertNil(try Fixtures.decode(PixivItem.self, "{\"id\":\"1\",\"popularTier\":\(tier)}").popularTier)
        }
    }

    func testIllustKindsStatsAndThinPages() throws {
        let work = try Fixtures.decode(PixivIllust.self, #"{"id":"11","title":"Work 11","author":"Ann","authorId":7,"created":"2026-10-01T00:00:00+00:00","kind":"manga","pageCount":3,"description":"A quiet street.","stats":{"views":1200,"likes":30,"bookmarks":5},"tags":[{"tag":"風景","en":"landscape"},{"en":"x"}],"pages":[{"index":0,"thumbUrl":"t0","url":"u0","originalUrl":"o0"},{"thumbUrl":"t1","url":"u1"},null],"bookmarked":true}"#)
        XCTAssertEqual(work.kind, .manga)
        XCTAssertEqual(work.stats, .init(views: 1200, likes: 30, bookmarks: 5, comments: 0))
        XCTAssertEqual(work.tags, [.init(tag: "風景", en: "landscape")])
        XCTAssertEqual(work.pages[1], .init(index: 1, thumbUrl: "t1", url: "u1", originalUrl: ""))
        XCTAssertEqual(work.pages[2], .init(index: 2, thumbUrl: "", url: "", originalUrl: ""))
        XCTAssertEqual(work.pages[2].id, 2)
        XCTAssertEqual(work.authorId, "7")
        XCTAssertTrue(work.bookmarked)
        let unknown = try Fixtures.decode(PixivIllust.self, #"{"id":9,"kind":"future","pageCount":-1,"stats":{"views":"bad"}}"#)
        XCTAssertEqual(unknown.id, "9")
        XCTAssertEqual(unknown.kind, .illust)
        XCTAssertEqual(unknown.pageCount, 1)
        XCTAssertEqual(unknown.stats.views, 0)
        XCTAssertEqual(try Fixtures.decode(PixivIllust.self, #"{"kind":"ugoira","ugoira":true}"#).kind, .ugoira)
    }

    func testSaveResultsAndLocalizedSummary() throws {
        let result = try Fixtures.decode(PixivSaveResult.self, #"{"saved":[{"page":0,"itemId":"itm_1","duplicate":false},{"page":1,"itemId":"itm_2","duplicate":true},{}],"failed":[{"page":2,"error":"ingest_failed"}]}"#)
        XCTAssertEqual(result.saved[1], .init(page: 1, itemId: "itm_2", duplicate: true))
        XCTAssertEqual(result.saved[2], .init(page: 0, itemId: "", duplicate: false))
        XCTAssertEqual(result.failed, [.init(page: 2, error: "ingest_failed")])
        let saved = 2, duplicates = 1, failed = 1
        XCTAssertEqual(PixivBrowseChoices.saveText(result), String(localized: "Saved: \(saved). Duplicates: \(duplicates). Failed: \(failed)."))
        XCTAssertTrue(try Fixtures.decode(PixivSaveResult.self, "{}").saved.isEmpty)
    }

    func testQueriesAndPublicLinks() {
        XCTAssertEqual(PixivBrowseQuery.adult(false), [])
        XCTAssertEqual(PixivBrowseQuery.adult(true), [URLQueryItem(name: "adult", value: "1")])
        XCTAssertEqual(PixivBrowseQuery.ranking(mode: "week_r18", page: 2, adult: false), [URLQueryItem(name: "mode", value: "week_r18"), URLQueryItem(name: "page", value: "2")])
        let q = PixivBrowseQuery.search(q: "blue sky", order: "popular", type: "manga", mode: "all", page: 3, adult: true)
        XCTAssertEqual(Dictionary(uniqueKeysWithValues: q.map { ($0.name, $0.value ?? "") }), ["q": "blue sky", "order": "popular", "type": "manga", "mode": "all", "page": "3", "adult": "1"])
        XCTAssertEqual(PixivBrowseQuery.pathID("a/b ?"), "a%2Fb%20%3F")
        XCTAssertEqual(PixivBrowseQuery.publicURL("12").absoluteString, "https://www.pixiv.net/artworks/12")
    }

    func testModesAndLabelsRespectAdultGate() {
        for content in PixivSettings.Content.allCases {
            XCTAssertEqual(PixivBrowseChoices.modes(content: content, adult: false), PixivBrowseChoices.safeModes)
        }
        XCTAssertEqual(PixivBrowseChoices.modes(content: .safe, adult: true), PixivBrowseChoices.safeModes)
        XCTAssertEqual(PixivBrowseChoices.modes(content: .r18, adult: true), PixivBrowseChoices.adultModes)
        XCTAssertEqual(PixivBrowseChoices.modes(content: .all, adult: true), PixivBrowseChoices.safeModes + PixivBrowseChoices.adultModes)
        XCTAssertEqual(PixivBrowseChoices.label("day"), String(localized: "Daily"))
        for mode in PixivBrowseChoices.safeModes + PixivBrowseChoices.adultModes {
            XCTAssertFalse(PixivBrowseChoices.label(mode).isEmpty)
        }
        XCTAssertEqual(PixivBrowseChoices.label("future"), "future")
    }

    func testNoticesSortNotesAndAdultRefusal() {
        XCTAssertEqual(PixivBrowseChoices.notice("r18_needs_session"), String(localized: "Connect your Pixiv account in Settings to see R-18 works."))
        XCTAssertEqual(PixivBrowseChoices.notice("r18_disabled_in_account"), String(localized: "Enable R-18 viewing in your account on pixiv.net."))
        XCTAssertEqual(PixivBrowseChoices.notice("all_ages_only"), String(localized: "Without a connected Pixiv account, only all-ages works are shown."))
        XCTAssertNil(PixivBrowseChoices.notice("future"))
        XCTAssertNil(PixivBrowseChoices.notice(nil))
        for sort in ["pixiv_popular", "pixiv_popular_preview", "users_tags", "date_d_fallback"] { XCTAssertNotNil(PixivBrowseChoices.sortNote(sort)) }
        XCTAssertNil(PixivBrowseChoices.sortNote("date_d"))
        let expected = String(localized: "Turn on Show adult plugins in Settings to see this work.")
        XCTAssertEqual(PixivBrowseChoices.errorText(.server(status: 403, code: "adult_off", message: "x")), expected)
        XCTAssertEqual(PixivBrowseChoices.errorText(.server(status: 403, code: "", message: "adult_off")), expected)
        XCTAssertEqual(PixivBrowseChoices.errorText(.timeout), APIError.timeout.userMessage)
    }

    func testDateParsingIsRegionIndependent() {
        let reference = Date(timeIntervalSince1970: 1_790_812_800)
        let formatter = ISO8601DateFormatter()
        XCTAssertEqual(PixivBrowseChoices.createdDate(formatter.string(from: reference)), reference)
        formatter.formatOptions.insert(.withFractionalSeconds)
        XCTAssertEqual(PixivBrowseChoices.createdDate(formatter.string(from: reference)), reference)
        XCTAssertNil(PixivBrowseChoices.createdDate("bad"))
    }

    func testBrowseRequestsUseContractAndInjectedAdultGate() async throws {
        let api = PixivAPI(client: StubURLProtocol.makeClient(), adultShown: { false })
        StubURLProtocol.handler = { _ in .init(body: #"{"ok":true,"data":{"items":[{"id":"1"}],"hasMore":true}}"#) }
        let listing = try await api.ranking(mode: "week", page: 2)
        XCTAssertEqual(listing.items.map(\.id), ["1"])
        _ = try await api.search(q: "sky", order: "date", type: "ugoira", mode: "safe", page: 3)
        _ = try await api.illust(id: "11")
        _ = try await api.bookmarks(content: .all)
        for request in StubURLProtocol.requests {
            let items = URLComponents(url: try XCTUnwrap(request.url), resolvingAgainstBaseURL: false)?.queryItems ?? []
            XCTAssertFalse(items.contains(where: { $0.name == "adult" }))
        }
        XCTAssertEqual(StubURLProtocol.requests.map { $0.url?.path }, ["/v1/pixiv/ranking", "/v1/pixiv/search", "/v1/pixiv/illust/11", "/v1/pixiv/bookmarks"])
        XCTAssertEqual(StubURLProtocol.requests[0].url?.query, "mode=week&page=2")
        XCTAssertEqual(StubURLProtocol.requests[1].url?.query, "q=sky&order=date&type=ugoira&mode=safe&page=3")
        XCTAssertEqual(StubURLProtocol.requests[3].url?.query, "content=all")
        XCTAssertEqual(StubURLProtocol.requests[0].value(forHTTPHeaderField: "Authorization"), "Bearer sec_test")
        let adult = PixivAPI(client: StubURLProtocol.makeClient(), adultShown: { true })
        try await adult.addBookmark(id: "12")
        try await adult.removeBookmark(id: "12")
        _ = try await adult.save(id: "12", pages: [1, 2])
        let requests = Array(StubURLProtocol.requests.suffix(3))
        XCTAssertEqual(requests.map(\.httpMethod), ["PUT", "DELETE", "POST"])
        XCTAssertEqual(requests[0].url?.query, "adult=1")
        XCTAssertNil(requests[1].url?.query)
        XCTAssertEqual(requests[2].url?.query, "adult=1")
        XCTAssertEqual(requests[2].timeoutInterval, 180)
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: try XCTUnwrap(requests[2].httpBody)) as? [String: Any])
        XCTAssertEqual(body["id"] as? String, "12")
        XCTAssertEqual(body["pages"] as? [Int], [1, 2])
        _ = try await adult.save(id: "12", pages: [])
        let whole = try XCTUnwrap(JSONSerialization.jsonObject(with: try XCTUnwrap(StubURLProtocol.requests.last?.httpBody)) as? [String: Any])
        XCTAssertNil(whole["pages"])
    }

    func testStoreDefaultsPagingDedupeAndBookmarkRollback() async throws {
        let api = PixivAPI(client: StubURLProtocol.makeClient(), adultShown: { false })
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/pixiv/status" {
                return .init(body: #"{"ok":true,"data":{"connection":{"state":"disconnected"},"settings":{"defaultContent":"r18","defaultSort":"popular"}}}"#)
            }
            if request.httpMethod == "PUT" { return .init(status: 500, body: "{}") }
            let page = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "page" }?.value
            return page == "2" ? .init(body: #"{"ok":true,"data":{"items":[{"id":"1"},{"id":"2"}],"page":2,"hasMore":false}}"#)
                : .init(body: #"{"ok":true,"data":{"items":[{"id":"1"}],"page":1,"hasMore":true}}"#)
        }
        let store = PixivBrowseStore(api: { api }, adultShown: { false })
        await store.loadStatus()
        XCTAssertEqual(store.content, .safe)
        XCTAssertEqual(store.order, "popular")
        XCTAssertEqual(store.connection?.state, .disconnected)
        await store.start()
        await store.loadMore()
        XCTAssertEqual(store.feed.items.map(\.id), ["1", "2"])
        XCTAssertFalse(store.feed.hasMore)
        await store.toggleBookmark(id: "1", fallback: false)
        XCTAssertFalse(store.isBookmarked("1", fallback: true))
        XCTAssertNotNil(store.actionError)
        XCTAssertFalse(store.feed.items[0].bookmarked)
        XCTAssertTrue(store.isBookmarked("absent", fallback: true))
        store.searchFor("  sky  ")
        XCTAssertEqual(store.query, "sky")
        XCTAssertEqual(store.tab, .search)
        store.content = .r18; store.rankingMode = "day_r18"
        store.normalizeContent()
        XCTAssertEqual(store.content, .safe)
        XCTAssertEqual(store.rankingMode, "day")
    }
}
