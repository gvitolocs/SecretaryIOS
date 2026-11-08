import XCTest
@testable import Secretary

@MainActor
final class InstagramTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }
    override func tearDown() { StubURLProtocol.handler = nil }

    func testProfileBundleAndTolerantFields() throws {
        let answer = try Fixtures.decode(IgEnvelope<IgProfileBundle>.self, """
        {"ok":true,"data":{"profile":{"username":"natgeo","fullName":"National Geographic",
        "biography":"Explore","picUrl":"/v1/instagram/s/9/x/avatar","profileUrl":"https://www.instagram.com/natgeo/",
        "mediaCount":30000,"followerCount":268000000,"followingCount":100,"isPrivate":false,
        "hasStories":true,"isVerified":true,"userId":12345,"externalUrl":null},
        "posts":{"items":[{"id":123,"kind":"image","url":"/v1/instagram/s/9/x/p1","thumbUrl":null,
        "takenAt":1700000000,"caption":"caption p1","likeCount":12,"commentCount":3}],"nextCursor":"A+B/c="},
        "highlights":[{"id":"highlight:1","title":"Best of","coverUrl":null,"mediaCount":null}]}}
        """)
        XCTAssertEqual(answer.data.profile.userId, "12345")
        XCTAssertEqual(answer.data.profile.followerCount, 268_000_000)
        XCTAssertTrue(answer.data.profile.isVerified)
        XCTAssertEqual(answer.data.posts.items.first?.id, "123")
        XCTAssertEqual(answer.data.posts.nextCursor, "A+B/c=")
        XCTAssertNil(answer.data.highlights.first?.mediaCount)
        let sparse = try Fixtures.decode(IgProfileBundle.self, #"{"profile":{"username":"nasa","fullName":12,"isPrivate":"unknown"},"posts":null,"highlights":null}"#)
        XCTAssertEqual(sparse.profile.username, "nasa")
        XCTAssertEqual(sparse.profile.fullName, "")
        XCTAssertFalse(sparse.profile.isPrivate)
        XCTAssertEqual(sparse.posts, IgPage())
        XCTAssertTrue(sparse.highlights.isEmpty)
    }
    func testPagesCarouselsAndArchive() throws {
        let page = try Fixtures.decode(IgPage.self, """
        {"items":[{"id":"c1","kind":"carousel","url":"/v1/instagram/s/9/x/c0","thumbUrl":null,
        "takenAt":null,"children":[{"id":1,"kind":"image","url":"/v1/instagram/s/9/x/c0","thumbUrl":null},
        {"id":"c1_1","kind":"video","url":"/v1/instagram/s/9/x/c1"}]}],"nextCursor":null,"exhausted":true}
        """)
        XCTAssertTrue(page.exhausted)
        XCTAssertNil(page.nextCursor)
        XCTAssertEqual(page.items[0].children.map(\.id), ["1", "c1_1"])
        XCTAssertEqual(page.items[0].children[1].kind, "video")
        XCTAssertNil(page.items[0].takenAt)
        let archive = try Fixtures.decode(IgPage.self, #"{"items":[{"id":"a1","mediaId":"itm_1","kind":"image","url":"/v1/instagram/a/9/x/itm_1","thumbUrl":"/v1/media/items/itm_1/thumb?size=256","savedAt":1700000000,"auto":true}],"nextCursor":"30"}"#)
        XCTAssertEqual(archive.items[0].url, "/v1/instagram/a/9/x/itm_1")
        XCTAssertEqual(archive.nextCursor, "30")
        XCTAssertFalse(archive.exhausted)
    }
    func testWatchSettingsBadgesRecentAndSaveResults() throws {
        let watched = try Fixtures.decode([IgWatch].self, #"[{"username":"natgeo","autoSave":false,"lastCheck":null,"newStories":2,"newPosts":1,"private":null},{"username":"nasa"}]"#)
        XCTAssertFalse(watched[0].autoSave)
        XCTAssertNil(watched[0].isPrivate)
        XCTAssertNil(watched[1].nextCheck)
        XCTAssertTrue(watched[1].autoSave)
        XCTAssertFalse(watched[1].gone)
        let settings = try Fixtures.decode(IgWatchSettings.self, #"{"intervalHours":12,"defaultHours":23,"jitterMinutes":30,"choices":[6,12,23]}"#)
        XCTAssertEqual(settings.intervalHours, 12)
        XCTAssertEqual(settings.choices, [6, 12, 23])
        let badges = try Fixtures.decode(IgBadges.self, #"{"total":3,"accounts":{"natgeo":3}}"#)
        XCTAssertEqual(badges.accounts["natgeo"], 3)
        let recent = try Fixtures.decode(IgList<IgRecent>.self, #"{"items":[{"username":"nasa","fullName":"NASA","isVerified":true,"picUrl":null,"openedAt":1}]}"#)
        XCTAssertEqual(recent.items[0].openedAt, 1)
        let result = try Fixtures.decode(IgSaveResult.self, #"{"saved":[{"index":0,"itemId":123,"duplicate":true}],"failed":[{"index":1,"error":"expired"}]}"#)
        XCTAssertEqual(result.saved[0].itemId, "123")
        XCTAssertTrue(result.saved[0].duplicate)
        XCTAssertEqual(result.failed[0].error, "expired")
        XCTAssertEqual(try Fixtures.decode(IgSaveResult.self, "{}"), IgSaveResult())
        XCTAssertThrowsError(try Fixtures.decode(IgEnvelope<IgPage>.self, #"{"ok":false,"data":{}}"#))
    }
    func testSearchParsing() {
        for input in ["@natgeo", "natgeo", " NATGEO ", "https://www.instagram.com/natgeo/", "instagram.com/natgeo/"] {
            XCTAssertEqual(InstagramText.parse(input), .profile("natgeo"), input)
        }
        XCTAssertEqual(InstagramText.parse("instagram.com/p/abc/"), .resolve("https://instagram.com/p/abc/", username: nil))
        XCTAssertEqual(InstagramText.parse("https://www.instagram.com/natgeo/reel/abc/?x=1"), .resolve("https://www.instagram.com/natgeo/reel/abc/?x=1", username: "natgeo"))
        for input in ["", "not a username", "https://example.com/natgeo", "instagram.com/accounts/", "https://instagram.com.evil.test/p/abc/", "@bad-name", "instagram.com/p/", String(repeating: "a", count: 31)] {
            XCTAssertNil(InstagramText.parse(input), input)
        }
        XCTAssertTrue(InstagramText.validUsername("na.t_geo"))
        XCTAssertFalse(InstagramText.validUsername("a/b"))
        XCTAssertEqual(InstagramText.profileLink("natgeo")?.absoluteString, "https://www.instagram.com/natgeo/")
        XCTAssertNil(InstagramText.profileLink("a/b"))
    }
    func testMonthChipsUseViewersZoneAndSkipUndatedItems() throws {
        let utc = try XCTUnwrap(TimeZone(secondsFromGMT: 0))
        let west = try XCTUnwrap(TimeZone(secondsFromGMT: -3600))
        let locale = Locale(identifier: "en_US_POSIX")
        let date = try XCTUnwrap(ISO8601DateFormatter().date(from: "2026-11-01T00:30:00Z"))
        let items = [IgItem(id: "1", takenAt: date.timeIntervalSince1970), IgItem(id: "2"),
                     IgItem(id: "3", takenAt: date.timeIntervalSince1970 - 3600),
                     IgItem(id: "4", takenAt: date.timeIntervalSince1970 - 7200)]
        let formatter = DateFormatter()
        formatter.locale = locale; formatter.calendar = Calendar(identifier: .gregorian); formatter.timeZone = utc
        formatter.setLocalizedDateFormatFromTemplate("MMM yyyy")
        let chips = InstagramText.monthChips(items, timeZone: utc, locale: locale)
        XCTAssertEqual(chips, [formatter.string(from: date), nil, formatter.string(from: date.addingTimeInterval(-3600)), nil])
        formatter.timeZone = west
        XCTAssertEqual(InstagramText.monthChips(items, timeZone: west, locale: locale), [formatter.string(from: date), nil, nil, nil])
        XCTAssertEqual(items.map(\.id), ["1", "2", "3", "4"])
        XCTAssertEqual(InstagramText.monthChips([IgItem(takenAt: .infinity)], timeZone: utc, locale: locale), [nil])
    }
    func testCompactCounts() {
        let locale = Locale(identifier: "en_US_POSIX")
        for (count, expected) in [(0, "0"), (1234, "1234"), (12500, "12.5K"), (268000000, "268M"), (1500000000, "1.5B"), (-12500, "-12.5K")] {
            XCTAssertEqual(InstagramText.compact(count, locale: locale), expected)
        }
        XCTAssertEqual(InstagramText.compact(12500, locale: Locale(identifier: "da_DK")), "12,5K")
    }
    func testANextPageWhileTheViewerIsOpenKeepsTheSelectedSlide() {
        let first = (0..<12).map { IgItem(id: "p\($0)", url: "u\($0)") }
        let selected = IgSlide.startID(first, itemIndex: 4)
        XCTAssertEqual(IgSlide.index(of: selected, in: IgSlide.slides(first)), 4)
        // The next page lands (with one item again that the store drops as a duplicate).
        let more = first + (12..<24).map { IgItem(id: "p\($0)", url: "u\($0)") }
        XCTAssertEqual(IgSlide.index(of: selected, in: IgSlide.slides(more)), 4)
        XCTAssertEqual(IgSlide.slides(more)[4].item.id, "p4")
        // Even items before it (a refresh with a new story first) keep the viewer on the same item.
        let refreshed = [IgItem(id: "new", url: "n")] + more
        XCTAssertEqual(IgSlide.slides(refreshed)[IgSlide.index(of: selected, in: IgSlide.slides(refreshed))].item.id, "p4")
        // A carousel's frames keep their own ids, so the frame on screen stays too.
        let carousel = IgItem(id: "c", kind: "carousel", url: "c0", children: [IgChild(id: "a", kind: "image", url: "a"), IgChild(id: "b", kind: "image", url: "b")])
        let frame = IgSlide.slides([carousel])[1].id
        XCTAssertEqual(IgSlide.index(of: frame, in: IgSlide.slides([IgItem(id: "x", url: "x"), carousel])), 2)
    }
    func testCarouselSlidesAndSavePayloads() throws {
        let item = IgItem(id: "c", kind: "carousel", url: "first", takenAt: 5, caption: "caption", children: [
            IgChild(id: "a", kind: "image", url: "image"), IgChild(id: "b", kind: "video", url: "video")])
        let slides = IgSlide.slides([item, IgItem(id: "after", kind: "video", url: "end")])
        XCTAssertEqual(slides.map(\.child.url), ["image", "video", "end"])
        XCTAssertEqual(slides.map(\.frames), [2, 2, 1])
        XCTAssertEqual(slides.map(\.frame), [0, 1, 0])
        XCTAssertEqual(IgSlide.startIndex([item, IgItem(id: "after")], itemIndex: 1), 2)
        XCTAssertEqual(IgSlide.startIndex([item], itemIndex: -1), 0)
        XCTAssertEqual(InstagramText.saveItems(item, username: "natgeo").map(\.kind), ["image", "video"])
        let one = InstagramText.saveItems(item, username: "natgeo", child: item.children[1])
        XCTAssertEqual(one, [IgSaveItem(url: "video", kind: "video", username: "natgeo", takenAt: 5, caption: "caption")])
        XCTAssertEqual(InstagramText.saveItems(IgItem(id: "x", kind: "carousel", url: "first"), username: "nasa")[0].kind, "image")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(one)) as? [[String: Any]])
        XCTAssertEqual(body[0]["username"] as? String, "natgeo")
        XCTAssertEqual(body[0]["takenAt"] as? Double, 5)
    }
    func testPathAndQueryEncoding() {
        XCTAssertEqual(InstagramAPI.segment("a/b c"), "a%2Fb%20c")
        XCTAssertEqual(InstagramAPI.segment("na.t_geo"), "na.t_geo")
        XCTAssertTrue(InstagramAPI.cursorQuery(nil).isEmpty)
        XCTAssertTrue(InstagramAPI.cursorQuery("").isEmpty)
        XCTAssertEqual(InstagramAPI.cursorQuery("A+B/c="), [URLQueryItem(name: "cursor", value: "A+B/c=")])
    }
    func testAPIRoutesUnwrapAndEncodeQueries() async throws {
        StubURLProtocol.handler = { request in
            let path = request.url!.path
            if path.hasSuffix("/recent") || path.hasSuffix("/highlights/natgeo") { return .init(body: #"{"ok":true,"data":{"items":[]}}"#) }
            if path.hasSuffix("/save") { return .init(body: #"{"ok":true,"data":{"saved":[{"index":0,"itemId":"itm_1","duplicate":false}],"failed":[]}}"#) }
            if path.hasSuffix("/watch/natgeo") {
                if request.httpMethod == "DELETE" { return .init(body: #"{"ok":true,"data":{"removed":true}}"#) }
                return .init(body: #"{"ok":true,"data":{"username":"natgeo","autoSave":false}}"#)
            }
            if path.hasSuffix("/watch-settings") { return .init(body: #"{"ok":true,"data":{"intervalHours":12}}"#) }
            return .init(body: #"{"ok":true,"data":{"items":[],"nextCursor":null}}"#)
        }
        let api = InstagramAPI(client: StubURLProtocol.makeClient())
        _ = try await api.page(.posts, username: "na.t_geo", cursor: "A+B/c=")
        _ = try await api.page(.reels, username: "natgeo")
        _ = try await api.page(.stories, username: "natgeo")
        _ = try await api.page(.archive, username: "natgeo", cursor: "30")
        _ = try await api.highlights("natgeo")
        _ = try await api.highlightItems("highlight:1", cursor: "x y")
        _ = try await api.resolve("https://www.instagram.com/p/abc/?x=1&y=2")
        _ = try await api.recent()
        let watched = try await api.watch("natgeo", autoSave: false)
        XCTAssertFalse(watched.autoSave)
        try await api.unwatch("natgeo")
        let settings = try await api.setWatchSettings(12)
        XCTAssertEqual(settings.intervalHours, 12)
        let saved = try await api.save([IgSaveItem(url: "/v1/instagram/s/9/x/p1", kind: "image", username: "natgeo", takenAt: 5, caption: "caption")])
        XCTAssertEqual(saved.saved[0].itemId, "itm_1")
        let first = try XCTUnwrap(StubURLProtocol.requests.first?.url)
        XCTAssertEqual(first.path, "/v1/instagram/posts/na.t_geo")
        XCTAssertEqual(URLComponents(url: first, resolvingAgainstBaseURL: false)?.queryItems?.first?.value, "A+B/c=")
        let highlight = try XCTUnwrap(StubURLProtocol.requests.first { $0.url?.path == "/v1/instagram/highlights/items" }?.url)
        XCTAssertEqual(URLComponents(url: highlight, resolvingAgainstBaseURL: false)?.queryItems,
                       [URLQueryItem(name: "id", value: "highlight:1"), URLQueryItem(name: "cursor", value: "x y")])
        let put = try XCTUnwrap(StubURLProtocol.requests.first { $0.httpMethod == "PUT" && $0.url?.path == "/v1/instagram/watch/natgeo" })
        let putData = try XCTUnwrap(put.httpBody)
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: putData) as? [String: Any])
        XCTAssertEqual(body["autoSave"] as? Bool, false)
    }
    func testReadTimeoutAndRateLimitBody() async throws {
        StubURLProtocol.handler = { request in
            XCTAssertEqual(request.timeoutInterval, 60)
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer sec_test")
            return .init(status: 429, body: #"{"ok":false,"error":"rate_limited","retryAfter":12}"#)
        }
        let api = InstagramAPI(client: StubURLProtocol.makeClient())
        do { _ = try await api.profile("natgeo"); XCTFail("Expected 429") }
        catch let error as APIError {
            XCTAssertEqual(error, .rateLimited(code: "rate_limited", retryAfter: 12, message: ""))
            XCTAssertEqual(InstagramText.error(error), String(localized: "Instagram is busy, try again in \(12) s"))
        }
        XCTAssertEqual(InstagramText.error(.server(status: 404, code: "not_found", message: "")), String(localized: "No account with that name."))
        XCTAssertEqual(InstagramText.error(.timeout), APIError.timeout.userMessage)
    }
    func testStoreKeepsPagesOnFailureStopsExhaustedAndClearsBadge() async {
        var fail = false
        StubURLProtocol.handler = { request in
            let path = request.url!.path
            if path == "/v1/instagram/watch" { return .init(body: #"{"ok":true,"data":[{"username":"natgeo"}]}"#) }
            if path.hasSuffix("/opened") { return .init(body: #"{"ok":true,"data":{"total":0,"accounts":{}}}"#) }
            if path.hasSuffix("/badges") { return .init(body: #"{"ok":true,"data":{"total":3,"accounts":{"natgeo":3}}}"#) }
            if path.hasSuffix("/watch-settings") { return .init(body: #"{"ok":true,"data":{"intervalHours":12}}"#) }
            if path.hasSuffix("/profile/natgeo") { return .init(body: #"{"ok":true,"data":{"profile":{"username":"natgeo"},"posts":{"items":[{"id":"p1"}],"nextCursor":"next"}}}"#) }
            if fail { return .init(status: 502, body: #"{"ok":false,"error":"upstream_unavailable"}"#) }
            return .init(body: #"{"ok":true,"data":{"items":[{"id":"p1"}],"nextCursor":"ignored","exhausted":true}}"#)
        }
        let api = InstagramAPI(client: StubURLProtocol.makeClient())
        let store = InstagramStore(api: { api })
        await store.refreshWatching()
        XCTAssertEqual(store.badges.total, 3)
        await store.open("natgeo")
        XCTAssertEqual(store.badges.total, 0)
        XCTAssertTrue(StubURLProtocol.requests.contains { $0.url?.path.hasSuffix("/opened") == true })
        await store.load(.posts, username: "natgeo", more: true)
        XCTAssertEqual(store.pages["posts"]?.items.map(\.id), ["p1"])
        XCTAssertTrue(store.pages["posts"]?.exhausted == true)
        let count = StubURLProtocol.requests.count
        await store.load(.posts, username: "natgeo", more: true)
        XCTAssertEqual(StubURLProtocol.requests.count, count)
        fail = true
        await store.load(.posts, username: "natgeo", refresh: true)
        XCTAssertEqual(store.pages["posts"]?.items.map(\.id), ["p1"])
        XCTAssertNotNil(store.errors["posts"])
    }
}
