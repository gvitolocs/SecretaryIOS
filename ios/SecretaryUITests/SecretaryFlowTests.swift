import XCTest

/// End-to-end flows against the local mock (`ios/scripts/run-mock.sh start`).
/// Mock sign-in: owner@example.test / dev-password-123 (test credentials of the mock).
@MainActor
final class SecretaryFlowTests: XCTestCase {
    static let baseURL = ProcessInfo.processInfo.environment["SECRETARY_MOCK_URL"] ?? "http://localhost:8791"

    override func setUp() {
        continueAfterFailure = false
    }

    private func launch(reset: Bool = true, tab: String? = nil) -> XCUIApplication {
        let app = XCUIApplication()
        // The assertions are in English; the simulator may be set to Danish or Italian.
        app.launchArguments = ["-SecretaryBaseURL", Self.baseURL, "-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
        if reset { app.launchArguments += ["-SecretaryResetState", "YES"] }
        if let tab { app.launchArguments += ["-SecretaryInitialTab", tab] }
        app.launch()
        return app
    }

    private func signIn(_ app: XCUIApplication) {
        let email = app.textFields["loginEmail"]
        XCTAssertTrue(email.waitForExistence(timeout: 15), "login screen")
        email.tap()
        email.typeText("owner@example.test")
        let password = app.secureTextFields["loginPassword"]
        password.tap()
        password.typeText("dev-password-123")
        app.buttons["signIn"].tap()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 15), "signed in")
    }

    private func attach(_ app: XCUIApplication, _ name: String) {
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = name
        shot.lifetime = .keepAlways
        add(shot)
    }

    // MARK: - Login

    func testLoginRejectsWrongPasswordThenSignsIn() {
        let app = launch()
        let email = app.textFields["loginEmail"]
        XCTAssertTrue(email.waitForExistence(timeout: 15))
        email.tap()
        email.typeText("owner@example.test")
        let password = app.secureTextFields["loginPassword"]
        password.tap()
        password.typeText("wrong-password")
        app.buttons["signIn"].tap()
        let error = app.staticTexts["loginError"]
        XCTAssertTrue(error.waitForExistence(timeout: 10))
        XCTAssertEqual(error.label, "That email and password don't match.")

        password.tap()
        password.press(forDuration: 1.0)
        if app.menuItems["Select All"].waitForExistence(timeout: 2) { app.menuItems["Select All"].tap() }
        password.typeText(XCUIKeyboardKey.delete.rawValue)
        password.typeText("dev-password-123")
        app.buttons["signIn"].tap()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.otherElements["lucinaStatus"].firstMatch.waitForExistence(timeout: 5) || app.buttons["lucinaStatus"].exists
            || app.descendants(matching: .any)["lucinaStatus"].exists)
    }

    func testAccessKeyFallbackSignsIn() {
        let app = launch()
        XCTAssertTrue(app.segmentedControls["loginMode"].waitForExistence(timeout: 15))
        app.segmentedControls["loginMode"].buttons["Access key"].tap()
        let key = app.secureTextFields["accessKey"]
        XCTAssertTrue(key.waitForExistence(timeout: 5))
        key.tap()
        key.typeText("dev")
        app.buttons["signIn"].tap()
        XCTAssertTrue(app.tabBars.buttons["Chat"].waitForExistence(timeout: 15))
    }

    // MARK: - Chat

    func testChatSendsAndShowsLucinasReply() {
        let app = launch()
        signIn(app)
        let composer = app.textFields["chatComposer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 5))
        composer.tap()
        composer.typeText("hello Lucina\n")
        let mine = app.descendants(matching: .any).matching(NSPredicate(format: "identifier == %@ AND label CONTAINS %@", "userMessage", "hello Lucina")).firstMatch
        XCTAssertTrue(mine.waitForExistence(timeout: 5), "optimistic user bubble")
        let reply = app.descendants(matching: .any).matching(NSPredicate(format: "identifier == %@ AND label CONTAINS %@", "lucinaMessage", "Good day")).firstMatch
        XCTAssertTrue(reply.waitForExistence(timeout: 20), "Lucina's reply")
        attach(app, "Chat")
    }

    func testTimelineShowsOtherChannelsLive() async throws {
        let app = launch()
        signIn(app)
        // History from the timeline shows with source badges.
        let badge = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH %@", "From ")).firstMatch
        XCTAssertTrue(badge.waitForExistence(timeout: 15), "messages with their source badge")
        let live = app.descendants(matching: .any)["liveIndicator"]
        XCTAssertTrue(live.waitForExistence(timeout: 15))
        let connected = NSPredicate(format: "label BEGINSWITH %@", "Live")
        expectation(for: connected, evaluatedWith: live)
        waitForExpectations(timeout: 15)

        // A Telegram message arrives through the stream while the app is open.
        let text = "From Telegram \(Int.random(in: 1000...9999))"
        var request = URLRequest(url: URL(string: "\(Self.baseURL)/v1/_mock/timeline")!)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("1", forHTTPHeaderField: "X-Secretary-CSRF")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["channel": "telegram", "role": "user", "text": text])
        _ = try await URLSession.shared.data(for: request)

        let message = app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", "via Telegram: \(text)")).firstMatch
        XCTAssertTrue(message.waitForExistence(timeout: 10), "live Telegram message")
        attach(app, "Unified chat")
    }

    // MARK: - Shopping

    func testShoppingAddAndToggle() {
        let app = launch(tab: "shopping")
        signIn(app)
        // Shopping v2: tiles, search to add, tap to buy.
        XCTAssertTrue(app.descendants(matching: .any).matching(NSPredicate(format: "identifier BEGINSWITH %@", "tile-")).firstMatch
            .waitForExistence(timeout: 15), "seeded tiles")
        // Bring!-style search sheet.
        app.buttons["shoppingSearch"].tap()
        let field = app.textFields["shoppingSearchField"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        let name = "UITest \(Int.random(in: 1000...9999))"
        field.typeText(name)
        let free = app.descendants(matching: .any)["suggestion-free"]
        XCTAssertTrue(free.waitForExistence(timeout: 5))
        free.tap()
        app.buttons["searchDone"].tap()
        let tile = app.descendants(matching: .any)["tile-\(name)"]
        XCTAssertTrue(tile.waitForExistence(timeout: 10), "added tile")
        XCTAssertEqual(tile.value as? String, "To buy")
        tile.tap()
        expectation(for: NSPredicate(format: "value == %@", "Bought"), evaluatedWith: app.descendants(matching: .any)["tile-\(name)"])
        waitForExpectations(timeout: 10)
        XCTAssertTrue(app.buttons["undo"].waitForExistence(timeout: 3), "undo offered")
        attach(app, "Shopping")
    }

    // MARK: - Gym (on-device engine, like the original LucinaGym tests)

    /// Fills the first weight field, completes the set and finishes the workout.
    private func logBenchSet(_ app: XCUIApplication, weight value: String) {
        let add = app.buttons["addExercise"]
        XCTAssertTrue(add.waitForExistence(timeout: 10))
        add.tap()
        let bench = app.buttons["exercise-bench-press"]
        XCTAssertTrue(bench.waitForExistence(timeout: 10))
        bench.tap()
        let weight = app.textFields.matching(NSPredicate(format: "identifier BEGINSWITH %@", "weight-")).firstMatch
        XCTAssertTrue(weight.waitForExistence(timeout: 10))
        weight.tap()
        let previous = weight.value as? String ?? ""
        weight.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: previous.count) + value)
        if app.toolbars.buttons["Done"].exists { app.toolbars.buttons["Done"].tap() }
        let complete = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "complete-set-")).firstMatch
        XCTAssertTrue(complete.waitForExistence(timeout: 5))
        complete.tap()
        XCTAssertTrue(app.descendants(matching: .any)["restTimer"].waitForExistence(timeout: 10), "rest timer started")
    }

    func testGymWorkoutPersistsAcrossRelaunch() {
        let app = launch(tab: "gym")
        signIn(app)
        let start = app.buttons["startWorkout"]
        XCTAssertTrue(start.waitForExistence(timeout: 10))
        XCTAssertTrue(app.staticTexts["Make time\nfor strength."].exists || app.staticTexts.containing(NSPredicate(format: "label CONTAINS %@", "for strength")).firstMatch.exists)
        start.tap()
        logBenchSet(app, weight: "62.5")
        attach(app, "Workout")
        app.buttons["finishWorkout"].tap()
        XCTAssertTrue(start.waitForExistence(timeout: 10), "back on Training")
        app.buttons["gymSection-Journal"].tap()
        XCTAssertTrue(app.buttons["history-Workout"].firstMatch.waitForExistence(timeout: 8), "finished workout in the journal")

        // The on-device engine file survives a relaunch.
        app.terminate()
        let relaunched = launch(reset: false, tab: "gym")
        XCTAssertTrue(relaunched.buttons["gymSection-Progress"].waitForExistence(timeout: 15))
        relaunched.buttons["gymSection-Progress"].tap()
        XCTAssertTrue(relaunched.staticTexts["Bench Press"].firstMatch.waitForExistence(timeout: 8))
        XCTAssertTrue(relaunched.staticTexts["62.5 kg"].firstMatch.exists)
        attach(relaunched, "Progress after relaunch")
    }

    func testGymRoutineCanBeDiscardedWithoutCreatingHistory() {
        let app = launch(tab: "gym")
        signIn(app)
        XCTAssertTrue(app.buttons["Start Full Body"].waitForExistence(timeout: 12))
        app.buttons["Start Full Body"].tap()
        XCTAssertTrue(app.buttons["Workout options"].waitForExistence(timeout: 8))
        app.buttons["Workout options"].tap()
        app.buttons["Discard workout"].tap()
        app.buttons["Discard workout"].firstMatch.tap()
        XCTAssertTrue(app.buttons["startWorkout"].waitForExistence(timeout: 8))
        app.buttons["gymSection-Journal"].tap()
        XCTAssertTrue(app.staticTexts["Your story starts here"].waitForExistence(timeout: 8))
    }

    func testGymRepeatOpensTheWorkout() {
        let app = launch(tab: "gym")
        signIn(app)
        XCTAssertTrue(app.buttons["startWorkout"].waitForExistence(timeout: 10))
        app.buttons["startWorkout"].tap()
        logBenchSet(app, weight: "50")
        app.buttons["finishWorkout"].tap()
        app.buttons["gymSection-Journal"].tap()
        let entry = app.buttons["history-Workout"].firstMatch
        XCTAssertTrue(entry.waitForExistence(timeout: 8))
        entry.tap()
        let repeatButton = app.buttons["repeatWorkout"]
        XCTAssertTrue(repeatButton.waitForExistence(timeout: 8))
        repeatButton.tap()
        XCTAssertTrue(app.buttons["finishWorkout"].waitForExistence(timeout: 8), "the repeated workout opens")
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "complete-set-")).firstMatch.exists)
    }

    // MARK: - Lucina stage

    func testLucinaBubbleExpandsMutedByDefault() {
        let app = launch()
        signIn(app)
        let bubble = app.descendants(matching: .any)["lucinaBubble"]
        XCTAssertTrue(bubble.waitForExistence(timeout: 10), "Lucina floats over the tabs")
        sleep(4) // let WebGL draw for the screenshot
        attach(app, "Lucina bubble")
        bubble.tap()
        let mute = app.buttons["lucinaMute"]
        XCTAssertTrue(mute.waitForExistence(timeout: 5), "expanded stage")
        XCTAssertEqual(mute.value as? String, "Muted", "no sound until the owner turns it on")
        XCTAssertTrue(app.buttons["lucinaMic"].exists)
        sleep(3)
        attach(app, "Lucina full body")
        app.buttons["lucinaClose"].tap()
        XCTAssertTrue(bubble.waitForExistence(timeout: 5))
    }

    // MARK: - Weight

    func testWeightLogEntry() {
        let app = launch()
        signIn(app)
        app.tabBars.buttons["Weight"].tap()
        let field = app.textFields["weightQuickField"]
        XCTAssertTrue(field.waitForExistence(timeout: 10))
        let value = "7\(Int.random(in: 1...8)).\(Int.random(in: 1...9))"
        field.tap()
        field.typeText(value)
        app.buttons["weightLogButton"].tap()
        let entry = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "\(value) kg")).firstMatch
        XCTAssertTrue(entry.waitForExistence(timeout: 10), "logged entry \(value) kg")
        attach(app, "Weight")
    }
}
