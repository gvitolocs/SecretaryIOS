import XCTest

/// How soon the chat scrolls after a cold launch and after a return from the background, on the
/// signed-in iPhone (ios/scripts/chat-scroll-perf.sh runs it and fetches the result).
/// The app runs with `-SecretaryPerfScroll`: it records when a drag first moved the list, and what the
/// list did until then, in `Documents/perf-scroll.txt`. This test only keeps dragging, down and up,
/// from the moment the app is up.
final class ChatScrollPerfTests: XCTestCase {
    override func setUp() {
        continueAfterFailure = true
    }

    func testChatScrollsSoonAfterLaunch() throws {
        let app = XCUIApplication()
        app.launchArguments = ["-SecretaryInitialTab", "chat", "-SecretaryPerfScroll"]
        app.launch()
        // What is on screen when the drags start (the test log keeps it).
        print("chat-scroll-perf: tree after launch\n\(app.debugDescription)")
        drag(app, for: 8)
        // Warm: back from the background.
        XCUIDevice.shared.press(.home)
        sleep(4)
        app.activate()
        drag(app, for: 8)
        // ScrollTrace writes its file 12 s after each start.
        sleep(6)
    }

    private func drag(_ app: XCUIApplication, for seconds: TimeInterval) {
        let window = app.windows.firstMatch
        let top = window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.3))
        let bottom = window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.6))
        let deadline = Date().addingTimeInterval(seconds)
        var down = true
        while Date() < deadline {
            let (from, to) = down ? (top, bottom) : (bottom, top)
            from.press(forDuration: 0.02, thenDragTo: to, withVelocity: .default, thenHoldForDuration: 0.05)
            down.toggle()
        }
    }
}
