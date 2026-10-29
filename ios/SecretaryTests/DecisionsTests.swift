import UserNotifications
import XCTest
@testable import Secretary

/// Lucina's decision pushes: the route, the inbox data, the answers sent, the notification responses.
final class DecisionsTests: XCTestCase {
    func testRoutes() {
        XCTAssertEqual(NotificationRoute.decision("/decisions/dc-0123456789ab"), "dc-0123456789ab")
        XCTAssertEqual(NotificationRoute.decision("/decisions"), "")
        XCTAssertNil(NotificationRoute.decision("/decisions/../etc"))
        XCTAssertNil(NotificationRoute.decision("/shopping"))
    }

    func testInboxDecisionData() throws {
        let page = try Fixtures.decode(NotificationPage.self, """
        {"items":[{"id":"n_1","ts":1793213591822,"category":"lucina_message","title":"Decision needed: claude-code (maps)",
          "body":"Use Google Maps?","route":"/decisions/dc-0123456789ab",
          "data":{"kind":"decision","decisionId":"dc-0123456789ab","options":["Google","Keep"]}},
          {"id":"n_2","ts":1793213591823,"category":"lucina_message","body":"hi","route":"/"}],"unread":2}
        """)
        XCTAssertEqual(page.items[0].decisionId, "dc-0123456789ab")
        XCTAssertEqual(page.items[0].decisionOptions, ["Google", "Keep"])
        XCTAssertNil(page.items[1].decisionId)
    }

    func testAnswerTexts() {
        // The exact text Lucina's decide::parse_answer understands.
        XCTAssertEqual(DecisionAnswer.option("dc-0123456789ab", index: 1, option: "hold"), "Decision dc-0123456789ab: 2. hold")
        XCTAssertEqual(DecisionAnswer.words("dc-0123456789ab", "wait a day"), "Decision dc-0123456789ab: wait a day")
        XCTAssertTrue(DecisionAnswer.isId("dc-0123456789ab"))
        XCTAssertFalse(DecisionAnswer.isId("dc-0123"))
    }

    func testNotificationResponses() {
        XCTAssertEqual(DecisionNotifier.response(actionIdentifier: UNNotificationDefaultActionIdentifier, userText: nil), .open)
        XCTAssertEqual(DecisionNotifier.response(actionIdentifier: "DECISION_OPTION_2", userText: nil), .option(2))
        XCTAssertEqual(DecisionNotifier.response(actionIdentifier: DecisionNotifier.wordsAction, userText: "later"), .words("later"))
        XCTAssertNil(DecisionNotifier.response(actionIdentifier: UNNotificationDismissActionIdentifier, userText: nil))
        XCTAssertEqual(DecisionNotifier.category("dc-0123456789ab"), "DECISION.dc-0123456789ab")
    }
}
