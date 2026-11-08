import XCTest

/// The Calendar, Instagram, Pixiv and NClient screens against the live server, on the owner's iPhone (the app must
/// already be signed in; nothing is reset). Run only on purpose:
/// `xcodebuild test -only-testing:SecretaryUITests/PluginScreensLiveTests -destination id=<iPhone>`.
/// The calendar test creates one event in Lucina's calendar, edits it and deletes it again.
@MainActor
final class PluginScreensLiveTests: XCTestCase {
    override func setUp() {
        continueAfterFailure = false
    }

    private func launch(route: String) throws -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-SecretaryOpenRoute", route, "-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
        app.launch()
        if app.textFields["loginEmail"].waitForExistence(timeout: 5) { throw XCTSkip("the app is signed out") }
        return app
    }

    private func attach(_ app: XCUIApplication, _ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }

    private func element(_ app: XCUIApplication, _ id: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: id).firstMatch
    }

    // MARK: - Calendar

    func testCalendarCreateEditDelete() throws {
        let app = try launch(route: "/calendar")
        XCTAssertTrue(element(app, "calendar-new").waitForExistence(timeout: 20), "calendar shown")
        sleep(3)
        attach(app, "calendar-1-month")

        let title = "iOS test \(UUID().uuidString.prefix(6)) delete me"
        addTeardownBlock { @MainActor in self.deleteIfPresent(app, titlePrefix: String(title.prefix(15))) }

        element(app, "calendar-new").tap()
        let field = app.textFields["calendar-editor-title"]
        XCTAssertTrue(field.waitForExistence(timeout: 5), "editor")
        field.tap()
        field.typeText(title)
        attach(app, "calendar-2-editor")
        element(app, "calendar-editor-save").tap()

        let created = app.staticTexts[title]
        XCTAssertTrue(created.waitForExistence(timeout: 15), "the new event is listed")
        created.tap()
        XCTAssertTrue(element(app, "calendar-edit").waitForExistence(timeout: 5), "detail")
        attach(app, "calendar-3-detail")

        element(app, "calendar-edit").tap()
        XCTAssertTrue(field.waitForExistence(timeout: 5), "editor for the edit")
        field.tap()
        field.typeText(" edited")
        element(app, "calendar-editor-save").tap()
        let edited = app.staticTexts[title + " edited"]
        XCTAssertTrue(edited.waitForExistence(timeout: 15), "the edit is listed")

        element(app, "calendar-view").buttons["Agenda"].tap()
        sleep(2)
        attach(app, "calendar-4-agenda")

        edited.tap()
        XCTAssertTrue(element(app, "calendar-delete").waitForExistence(timeout: 5), "detail of the edit")
        element(app, "calendar-delete").tap()
        app.alerts.buttons["Delete"].tap()
        XCTAssertTrue(edited.waitForNonExistence(timeout: 15), "the event is gone")
    }

    /// Leaves no test event behind when the test stops halfway.
    private func deleteIfPresent(_ app: XCUIApplication, titlePrefix: String) {
        let left = app.staticTexts.containing(NSPredicate(format: "label BEGINSWITH %@", titlePrefix)).firstMatch
        guard app.state == .runningForeground, left.exists else { return }
        if app.sheets.count > 0 || app.navigationBars.buttons["Cancel"].exists { app.navigationBars.buttons["Cancel"].tap() }
        left.tap()
        guard element(app, "calendar-delete").waitForExistence(timeout: 5) else { return }
        element(app, "calendar-delete").tap()
        app.alerts.buttons["Delete"].tap()
        _ = left.waitForNonExistence(timeout: 10)
    }

    // MARK: - Instagram

    func testInstagramProfileHighlightsAndPicture() throws {
        let app = try launch(route: "/instagram")
        let search = app.textFields["instagram-search"]
        XCTAssertTrue(search.waitForExistence(timeout: 15), "instagram shown")
        attach(app, "instagram-1-start")
        search.tap()
        search.typeText("natgeo")
        element(app, "instagram-open").tap()
        let picture = element(app, "instagram-profile-picture")
        XCTAssertTrue(picture.waitForExistence(timeout: 60), "profile loaded")
        sleep(3)
        attach(app, "instagram-2-profile")

        app.buttons["Highlights"].tap()
        let highlight = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'instagram-highlight-'")).firstMatch
        if highlight.waitForExistence(timeout: 60) {
            sleep(2)
            attach(app, "instagram-3-highlights")
            highlight.tap()
            let tile = element(app, "instagram-tile-0")
            XCTAssertTrue(tile.waitForExistence(timeout: 60), "highlight items")
            sleep(4)
            attach(app, "instagram-4-highlight-grid")
            tile.tap()
            sleep(4)
            attach(app, "instagram-5-viewer")
            if element(app, "instagram-viewer-close").exists { element(app, "instagram-viewer-close").tap() }
            app.navigationBars.buttons.firstMatch.tap()
        }
        XCTAssertTrue(picture.waitForExistence(timeout: 10))
        picture.tap()
        sleep(3)
        attach(app, "instagram-6-picture")
    }

    /// "5 of 24" → (5, 24); the element's value is the id of the slide on screen.
    private func position(_ app: XCUIApplication) -> (index: Int, count: Int, id: String) {
        let counter = element(app, "instagram-viewer-position")
        let parts = counter.label.components(separatedBy: " of ").map { Int($0.trimmingCharacters(in: .whitespaces)) }
        guard parts.count == 2, let index = parts[0], let count = parts[1] else {
            XCTFail("viewer position \"\(counter.label)\"")
            return (0, 0, "")
        }
        return (index, count, (counter.value as? String) ?? "")
    }

    /// Waits until the slide count grows past `count` and returns the position then, or nil if no page arrived.
    private func waitForMore(_ app: XCUIApplication, than count: Int, timeout: TimeInterval) -> (index: Int, count: Int, id: String)? {
        let end = Date().addingTimeInterval(timeout)
        while Date() < end {
            let now = position(app)
            if now.count > count { return now }
            usleep(500_000)
        }
        return nil
    }

    /// The viewer stays on the slide on screen while the next page of posts arrives (2026-11-01: it slid to another
    /// item by itself): at the first slide, at the last loaded slide, and while swiping. natgeo's reels, live server.
    func testInstagramViewerKeepsItsSlideWhenAPageArrives() throws {
        let app = try launch(route: "/instagram")
        let search = app.textFields["instagram-search"]
        XCTAssertTrue(search.waitForExistence(timeout: 15), "instagram shown")
        search.tap()
        search.typeText("natgeo")
        element(app, "instagram-open").tap()
        XCTAssertTrue(element(app, "instagram-profile-picture").waitForExistence(timeout: 60), "profile loaded")
        // Reels, not posts: the anonymous source stops posts at the newest twelve, reels page on.
        app.buttons["Reels"].tap()
        let tile = element(app, "instagram-tile-0")
        XCTAssertTrue(tile.waitForExistence(timeout: 60), "reels loaded")
        sleep(3)
        tile.tap()
        XCTAssertTrue(element(app, "instagram-viewer-position").waitForExistence(timeout: 15), "viewer open")
        let pages = element(app, "instagram-viewer-pages")
        attach(app, "viewer-paging-1-open")

        // First slide: a swipe back stays on it.
        let first = position(app)
        XCTAssertEqual(first.index, 1, "opens on the first slide")
        XCTAssertFalse(first.id.isEmpty, "the slide id is exposed")
        pages.swipeRight()
        sleep(1)
        XCTAssertEqual(position(app).index, 1, "still the first slide after a swipe back")
        XCTAssertEqual(position(app).id, first.id, "same first slide")

        // Last loaded slide: walk to it, then let the next page land under it.
        var grew = false
        for _ in 0..<(first.count + 5) {
            let now = position(app)
            if now.count > first.count { grew = true; break }
            if now.index == now.count { break }
            pages.swipeLeft()
            usleep(400_000)
        }
        if !grew {
            let before = position(app)
            XCTAssertEqual(before.index, before.count, "on the last loaded slide")
            attach(app, "viewer-paging-2-last")
            guard let after = waitForMore(app, than: before.count, timeout: 45) else {
                XCTFail("no next page arrived at the last slide (\(before.index) of \(before.count))")
                return
            }
            XCTAssertEqual(after.index, before.index, "same place when the page arrived (\(before.count) → \(after.count))")
            XCTAssertEqual(after.id, before.id, "the viewer stayed on the slide on screen when the page arrived")
            attach(app, "viewer-paging-3-page-arrived")
        }
        XCTContext.runActivity(named: "page arrived before the last slide: \(grew)") { _ in }

        // While swiping: get within 3 of the end (that starts the next load) and keep swiping as it arrives.
        for _ in 0..<40 {
            let now = position(app)
            if now.index >= now.count - 3 { break }
            pages.swipeLeft()
            usleep(400_000)
        }
        let start = position(app)
        var seen = [start]
        var pageArrivedWhileSwiping = false
        for _ in 0..<4 {
            pages.swipeLeft()
            usleep(700_000)
            let now = position(app)
            if now.count > seen[seen.count - 1].count { pageArrivedWhileSwiping = true }
            seen.append(now)
        }
        sleep(2)
        let end = position(app)
        for (previous, next) in zip(seen, seen.dropFirst()) {
            XCTAssertGreaterThanOrEqual(next.index, previous.index, "never back while swiping: \(seen.map { "\($0.index)/\($0.count)" })")
        }
        XCTAssertGreaterThanOrEqual(end.index, start.index, "not behind where the swipes started")
        XCTAssertLessThanOrEqual(end.index, start.index + 4, "at most one slide per swipe")
        XCTAssertEqual(end.id, seen[seen.count - 1].id, "nothing moved after the last swipe")
        if !pageArrivedWhileSwiping, let after = waitForMore(app, than: end.count, timeout: 45) {
            XCTAssertEqual(after.index, end.index, "same place when the page arrived after the swipes")
            XCTAssertEqual(after.id, end.id, "same slide when the page arrived after the swipes")
        }
        XCTContext.runActivity(named: "page arrived while swiping: \(pageArrivedWhileSwiping)") { _ in }
        attach(app, "viewer-paging-4-after-swipes")
        if element(app, "instagram-viewer-close").exists { element(app, "instagram-viewer-close").tap() }
    }

    // MARK: - Pixiv

    func testPixivRankingAndReader() throws {
        let app = try launch(route: "/pixiv")
        let work = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'pixiv-work-'")).firstMatch
        XCTAssertTrue(work.waitForExistence(timeout: 30), "ranking loaded")
        sleep(4)
        attach(app, "pixiv-1-ranking")
        work.tap()
        XCTAssertTrue(element(app, "pixiv-page-0").waitForExistence(timeout: 30), "reader")
        sleep(4)
        attach(app, "pixiv-2-reader")
    }

    // MARK: - NClient

    func testNClientBrowseAndDetail() throws {
        let app = try launch(route: "/nclient")
        let item = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'nclient-gallery-'")).firstMatch
        XCTAssertTrue(item.waitForExistence(timeout: 30), "results loaded")
        sleep(3)
        attach(app, "nclient-1-browse")
        item.tap()
        XCTAssertTrue(element(app, "nclient-read").waitForExistence(timeout: 30), "detail")
        sleep(2)
        attach(app, "nclient-2-detail")
        // Not the reader: it saves the reading progress to the owner's account.
    }
}
