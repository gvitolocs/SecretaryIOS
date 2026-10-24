import XCTest

/// Runs on the real iPhone against the live server, signed in (no reset):
///   xcodebuild test ... -only-testing:SecretaryUITests/MemoDeviceTests
/// 1. Opens Memos, records three seconds, pauses, resumes and DISCARDS it (nothing of the
///    room's audio is kept or uploaded).
/// 2. Queues a synthetic memo (the iPhone's speech synthesizer, DEBUG launch argument) and
///    waits until the server says it is ready. Screenshots are attached to the result.
final class MemoDeviceTests: XCTestCase {
    private func shot(_ app: XCUIApplication, _ name: String) {
        let a = XCTAttachment(screenshot: app.screenshot())
        a.name = name
        a.lifetime = .keepAlways
        add(a)
    }

    func testRecordUIAndSyntheticMemoPipeline() throws {
        let app = XCUIApplication()
        app.launchArguments += [
            "-SecretaryMemoSynthetic",
            "Questo è un memo di prova registrato dall'iPhone. Domani alle dieci devo mandare a Laura il resoconto della riunione e prenotare la sala per giovedì.",
            "-SecretaryMemoSyntheticLang", "it-IT",
            "-SecretaryMemoSyntheticTitle", "TEST iPhone memo (voce sintetica)",
        ]
        addUIInterruptionMonitor(withDescription: "Microphone") { alert in
            for label in ["Allow", "OK", "Tillad", "Consenti"] where alert.buttons[label].exists {
                alert.buttons[label].tap()
                return true
            }
            return false
        }
        app.launch()
        let record = app.buttons["memoRecordButton"]
        let stop = app.buttons["memoStopButton"]
        // Never touch a recording the owner is making.
        if stop.waitForExistence(timeout: 3) { throw XCTSkip("a recording is in progress on the phone") }
        if !record.exists {
            let memos = app.buttons["memosButton"]
            XCTAssertTrue(memos.waitForExistence(timeout: 20))
            memos.tap()
        }
        XCTAssertTrue(record.waitForExistence(timeout: 10))
        shot(app, "1-list")
        record.tap()
        app.tap() // lets the interruption monitor handle a permission alert
        XCTAssertTrue(stop.waitForExistence(timeout: 10))
        sleep(3)
        shot(app, "2-recording")
        app.buttons["memoPauseButton"].tap()
        sleep(1)
        shot(app, "3-paused")
        app.buttons["memoPauseButton"].tap()
        sleep(1)
        app.buttons["Discard"].tap()
        let confirm = app.buttons.matching(identifier: "Discard").element(boundBy: 1)
        if confirm.waitForExistence(timeout: 3) { confirm.tap() } else { app.sheets.buttons["Discard"].firstMatch.tap() }
        XCTAssertTrue(record.waitForExistence(timeout: 5))

        // The synthetic memo: uploaded, transcribed, summarised (the list polls every few seconds).
        let cell = app.cells.containing(NSPredicate(format: "label CONTAINS[c] 'TEST iPhone memo'")).firstMatch
        XCTAssertTrue(cell.waitForExistence(timeout: 60), "the synthetic memo never showed up")
        shot(app, "4-uploading")
        let deadline = Date().addingTimeInterval(600)
        var ready = false
        while Date() < deadline {
            if cell.images["Ready"].exists { ready = true; break }
            sleep(5)
        }
        shot(app, "5-after-pipeline")
        XCTAssertTrue(ready, "the synthetic memo did not become ready")
        if ready {
            cell.staticTexts.firstMatch.tap()
            let details = app.buttons["memoDetailsButton"]
            if details.waitForExistence(timeout: 5) {
                details.tap()
                sleep(6)
                shot(app, "6-detail")
                app.swipeUp()
                sleep(1)
                shot(app, "7-detail-scrolled")
            }
        }
    }
}
