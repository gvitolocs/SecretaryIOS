import XCTest

/// The Gallery and photo backup against the live server, on the owner's iPhone (the app must
/// already be signed in; nothing is reset). Run only on purpose:
/// `xcodebuild test -only-testing:SecretaryUITests/GalleryLiveTests -destination id=<iPhone>`.
/// Turning backup on starts uploading the iPhone's library to the owner's server (Wi-Fi only).
@MainActor
final class GalleryLiveTests: XCTestCase {
    override func setUp() {
        continueAfterFailure = false
    }

    private func launchGallery() throws -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments = ["-SecretaryInitialTab", "gallery", "-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
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

    func testBackupFillsTheGallery() throws {
        let app = try launchGallery()
        addUIInterruptionMonitor(withDescription: "Photos access") { alert in
            for label in ["Allow Full Access", "Allow Access to All Photos", "Allow"] where alert.buttons[label].exists {
                alert.buttons[label].tap()
                return true
            }
            return false
        }
        XCTAssertTrue(app.buttons["backupBanner"].waitForExistence(timeout: 20), "gallery loaded")
        attach(app, "1-gallery")
        app.buttons["galleryBackup"].tap()
        let toggle = app.switches["backupToggle"]
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "backup settings")
        if (toggle.value as? String) != "1" {
            toggle.switches.firstMatch.exists ? toggle.switches.firstMatch.tap() : toggle.tap()
        }
        sleep(2)
        app.tap() // lets the interruption monitor answer the Photos prompt
        attach(app, "2-backup-on")
        app.buttons["Done"].tap()
        // Wait until the banner says a photo is up (or done), refreshing the timeline.
        var seen = false
        for _ in 0..<40 {
            sleep(6)
            app.swipeDown()
            let banner = app.buttons["backupBanner"].label
            if banner.contains("backed up") || banner.contains("up to date") {
                seen = true
                break
            }
        }
        attach(app, "3-after-backup")
        XCTAssertTrue(seen, "the banner reports backed-up photos")
        let first = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'gallery-'")).firstMatch
        XCTAssertTrue(first.waitForExistence(timeout: 20), "an item in the timeline")
        first.tap()
        sleep(4)
        attach(app, "4-viewer")
        app.buttons["Close"].tap()
    }

    func testServerVideoPlaysFromTheGallery() throws {
        let app = try launchGallery()
        let search = app.searchFields.firstMatch
        if !search.exists { app.swipeDown() }
        XCTAssertTrue(search.waitForExistence(timeout: 10), "search field")
        search.tap()
        search.typeText("Sword\n")
        let item = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'gallery-'")).firstMatch
        XCTAssertTrue(item.waitForExistence(timeout: 20), "search found the film")
        attach(app, "1-search")
        item.tap()
        XCTAssertTrue(app.buttons["galleryPlay"].waitForExistence(timeout: 10), "viewer with play")
        app.buttons["galleryPlay"].tap()
        XCTAssertTrue(app.buttons["closePlayer"].waitForExistence(timeout: 15), "player opened")
        sleep(12)
        attach(app, "2-playing")
        app.buttons["closePlayer"].tap()
    }
}
