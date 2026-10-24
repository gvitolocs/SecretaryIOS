import XCTest

/// Screenshots of every tab and the main secondary screens, on the real iPhone, signed in
/// (no reset). Read-only: it only opens screens and sheets and closes them again.
///   xcodebuild test ... -only-testing:SecretaryUITests/DesignShotsTests
final class DesignShotsTests: XCTestCase {
    private var app: XCUIApplication!

    override func setUp() {
        continueAfterFailure = true
        app = XCUIApplication()
        app.launch()
    }

    private func shot(_ name: String, settle: UInt32 = 2) {
        sleep(settle)
        let a = XCTAttachment(screenshot: app.screenshot())
        a.name = name
        a.lifetime = .keepAlways
        add(a)
    }

    private func tab(_ index: Int) {
        let button = app.tabBars.buttons.element(boundBy: index)
        if button.waitForExistence(timeout: 20) { button.tap() }
    }

    private func back() {
        let b = app.navigationBars.buttons.element(boundBy: 0)
        if b.exists { b.tap() }
        sleep(1)
    }

    private func open(_ id: String) -> Bool {
        let b = app.buttons[id].firstMatch
        guard b.waitForExistence(timeout: 5) else { return false }
        b.tap()
        return true
    }

    func testAllScreens() {
        tab(0); shot("01-chat", settle: 4)
        if open("notesButton") { shot("02-notes"); back() }
        if open("tasksButton") { shot("03-tasks"); back() }
        if open("notificationBell") { shot("04-notifications"); back(); app.swipeDown() }

        tab(1); shot("05-shopping", settle: 3)
        app.swipeUp(); shot("06-shopping-scrolled"); app.swipeDown()
        if open("dealsMapButton") { shot("07-deals-map", settle: 4); back() }

        tab(2); shot("08-gym", settle: 3)
        app.swipeUp(); shot("09-gym-scrolled"); app.swipeDown()

        tab(3); shot("10-weight", settle: 3)
        app.swipeUp(); shot("11-weight-scrolled"); app.swipeDown()
        if open("weightQuickAdd") { shot("12-weight-sheet"); app.swipeDown(velocity: .fast); sleep(1) }

        tab(4); shot("13-settings", settle: 2)
        if open("openLinks") { shot("14-links"); back() }
        app.swipeUp(); shot("15-settings-scrolled")
        if open("notificationSettings") { shot("16-notification-settings"); back() }
    }
}
