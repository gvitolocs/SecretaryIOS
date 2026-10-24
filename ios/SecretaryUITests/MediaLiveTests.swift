import XCTest

/// The media player against the live server, on the owner's iPhone (the app must already be
/// signed in; nothing is reset). Run only on purpose:
/// `xcodebuild test -only-testing:SecretaryUITests/MediaLiveTests -destination id=<iPhone>`
/// with `SECRETARY_MEDIA_VIDEO_ID` / `SECRETARY_MEDIA_AUDIO_ID` set to items in the library.
@MainActor
final class MediaLiveTests: XCTestCase {
    private var videoId: String? { ProcessInfo.processInfo.environment["SECRETARY_MEDIA_VIDEO_ID"] }
    private var audioId: String? { ProcessInfo.processInfo.environment["SECRETARY_MEDIA_AUDIO_ID"] }
    /// A video with a text subtitle track (embedded or a sidecar file).
    private var subtitledId: String? { ProcessInfo.processInfo.environment["SECRETARY_MEDIA_SUBTITLED_ID"] }

    override func setUp() {
        continueAfterFailure = false
    }

    private func launchLibrary() throws -> XCUIApplication {
        guard videoId != nil || audioId != nil || subtitledId != nil else { throw XCTSkip("set SECRETARY_MEDIA_VIDEO_ID / SECRETARY_MEDIA_AUDIO_ID") }
        let app = XCUIApplication()
        app.launchArguments = ["-SecretaryInitialTab", "media", "-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
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

    private func row(_ id: String, in app: XCUIApplication) -> XCUIElement {
        let row = app.buttons["media-\(id)"]
        // Clear of the mini player and tab bar at the bottom.
        let bottom = app.frame.maxY - 200
        for _ in 0..<12 where !(row.exists && row.isHittable && row.frame.maxY < bottom) {
            app.swipeUp()
        }
        return row
    }

    func testVideoPlaysSeeksAndResumes() throws {
        let app = try launchLibrary()
        guard let videoId else { throw XCTSkip("no video id") }
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'media-'")).firstMatch.waitForExistence(timeout: 30), "library loaded")
        attach(app, "1-library")
        let video = row(videoId, in: app)
        XCTAssertTrue(video.exists, "video row")
        video.tap()
        XCTAssertTrue(app.buttons["closePlayer"].waitForExistence(timeout: 10), "player opened")
        sleep(12)
        attach(app, "2-video-playing")
        // Scrub: tap the video to show the controls, then skip forward with AVKit's button.
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        let forward = app.buttons.matching(NSPredicate(format: "label CONTAINS[c] 'forward' OR label CONTAINS[c] 'Skip Forward'")).firstMatch
        if forward.waitForExistence(timeout: 3) {
            forward.tap()
            forward.tap()
        }
        sleep(4)
        attach(app, "3-video-after-skip")
        sleep(16)
        attach(app, "4-video-later")
        app.buttons["closePlayer"].tap()
        sleep(2)
        attach(app, "5-library-with-mini-player")
        // Reopen: playback continues from the saved position.
        row(videoId, in: app).tap()
        XCTAssertTrue(app.buttons["closePlayer"].waitForExistence(timeout: 10))
        sleep(8)
        app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        sleep(1)
        attach(app, "6-video-resumed")
        app.buttons["closePlayer"].tap()
    }

    func testAudioPlaysAndSkips() throws {
        let app = try launchLibrary()
        guard let audioId else { throw XCTSkip("no audio id") }
        app.buttons["Audio"].tap()
        XCTAssertTrue(app.buttons["media-\(audioId)"].waitForExistence(timeout: 20), "audio list loaded")
        attach(app, "1-audio-list")
        row(audioId, in: app).tap()
        let play = app.buttons["playPause"]
        XCTAssertTrue(play.waitForExistence(timeout: 10), "audio player opened")
        sleep(6)
        attach(app, "2-audio-playing")
        app.buttons["Forward 15 seconds"].tap()
        sleep(3)
        attach(app, "3-audio-after-skip")
        XCTAssertEqual(play.label, "Pause", "still playing")
        play.tap()
        sleep(1)
        attach(app, "4-audio-paused")
        app.buttons["closePlayer"].tap()
    }

    /// The owner's report: subtitles VLC finds were missing. `SECRETARY_MEDIA_SUBTITLED_ID` has
    /// only a loose `whisper-da.srt` in its folder; `SECRETARY_MEDIA_IMAGE_SUB_ID` has DVD picture
    /// subtitles (OCR'd on the server). Picking Danish on the first must turn it on for the second.
    func testSubtitlesFoundAndRemembered() throws {
        let app = try launchLibrary()
        guard let subtitledId else { throw XCTSkip("no subtitled video id") }
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'media-'")).firstMatch.waitForExistence(timeout: 30), "library loaded")
        let video = row(subtitledId, in: app)
        XCTAssertTrue(video.exists, "video row")
        video.tap()
        XCTAssertTrue(app.buttons["closePlayer"].waitForExistence(timeout: 10), "player opened")
        sleep(4)
        app.buttons["Subtitles and speed"].tap()
        sleep(1)
        attach(app, "1-subtitle-menu")
        let danish = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Danish'")).firstMatch
        XCTAssertTrue(danish.waitForExistence(timeout: 5), "the folder's whisper-da.srt is listed")
        danish.tap()
        sleep(20)
        attach(app, "2-sidecar-subtitles")
        app.buttons["closePlayer"].tap()
        guard let imageId = ProcessInfo.processInfo.environment["SECRETARY_MEDIA_IMAGE_SUB_ID"] else { return }
        let film = row(imageId, in: app)
        XCTAssertTrue(film.exists, "image-subtitle film row")
        film.tap()
        XCTAssertTrue(app.buttons["closePlayer"].waitForExistence(timeout: 10), "player opened")
        sleep(25)
        attach(app, "3-ocr-subtitles-remembered")
        sleep(10)
        attach(app, "4-ocr-subtitles-later")
        app.buttons["Subtitles and speed"].tap()
        sleep(1)
        attach(app, "5-picker-on-dvd-film")
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Danish'")).count >= 2, "both DVD tracks listed")
    }
}
