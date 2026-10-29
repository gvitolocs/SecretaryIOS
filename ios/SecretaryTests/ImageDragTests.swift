import CoreGraphics
@testable import Secretary
import SecretaryCore
import XCTest

final class ImageDragTests: XCTestCase {
    private func drag(_ json: String) throws -> [String: Any] {
        let raw = try XCTUnwrap(json.withCString { secretary_image_view($0) })
        defer { secretary_string_free(raw) }
        let data = try XCTUnwrap(String(cString: raw).data(using: .utf8))
        let answer = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(answer["ok"] as? Bool, true)
        return try XCTUnwrap(answer["drag"] as? [String: Any])
    }

    func testStraightDownIsOut() throws {
        let result = try drag(#"{"new":[1440,960,390,844],"drag":{"dx":0,"dy":40,"vx":0,"vy":0}}"#)
        XCTAssertEqual(result["axis"] as? String, "out")
    }

    func testThirtyFiveDegreesDownIsOut() throws {
        let result = try drag(#"{"new":[1440,960,390,844],"drag":{"dx":28,"dy":40,"vx":0,"vy":0}}"#)
        XCTAssertEqual(result["axis"] as? String, "out")
    }

    func testFortyFiveDegreesDownWithoutVelocityIsOut() throws {
        let result = try drag(#"{"new":[1440,960,390,844],"drag":{"dx":30,"dy":30,"vx":0,"vy":0}}"#)
        XCTAssertEqual(result["axis"] as? String, "out")
    }

    func testHorizontalSwipePages() throws {
        let result = try drag(#"{"new":[1440,960,390,844],"drag":{"dx":40,"dy":10,"vx":900,"vy":0}}"#)
        XCTAssertEqual(result["axis"] as? String, "pager")
    }

    func testFastShortFlickDismisses() throws {
        let result = try drag(#"{"new":[1440,960,390,844],"drag":{"dx":0,"dy":24,"vx":0,"vy":900,"predicted_dy":30,"ended":true}}"#)
        XCTAssertEqual(result["end"] as? String, "dismiss")
    }

    func testSlowShortDragSpringsBack() throws {
        let result = try drag(#"{"new":[1440,960,390,844],"drag":{"dx":0,"dy":60,"vx":0,"vy":40,"predicted_dy":70,"ended":true}}"#)
        XCTAssertEqual(result["end"] as? String, "back")
    }

    func testDragWhileOpeningIsOut() throws {
        let result = try drag(#"{"new":[1,1,390,844],"drag":{"dx":20,"dy":30,"vx":0,"vy":700}}"#)
        XCTAssertEqual(result["axis"] as? String, "out")
    }

    // The pan asks to start after a few points (iOS 27: dy 0-5), under the core's slop. These are the
    // owner's slow swipes from the phone's log (2026-10-29) that never started.
    private let screen = CGSize(width: 390, height: 844)

    func testSlowSwipeDownStartsOurPanWithoutTheZoomTransition() {
        let (begin, axis) = PhotoDragOut.admits(CGSize(width: 0, height: 2), CGSize(width: 0, height: 189), screen, upOnly: false)
        XCTAssertTrue(begin)
        XCTAssertEqual(axis, "out")
    }

    func testSwipeDownIsLeftToTheZoomTransitionFromTheGrid() {
        XCTAssertFalse(PhotoDragOut.admits(CGSize(width: 0, height: 2), CGSize(width: 0, height: 189), screen, upOnly: true).begin)
        XCTAssertFalse(PhotoDragOut.admits(CGSize(width: 0, height: 27), CGSize(width: -23, height: 1262), screen, upOnly: true).begin)
    }

    func testSlowSwipeUpStartsTheDetails() {
        XCTAssertTrue(PhotoDragOut.admits(CGSize(width: 0, height: -3), CGSize(width: 5, height: -187), screen, upOnly: true).begin)
    }

    func testSlowSidewaysSwipeStaysWithThePager() {
        XCTAssertFalse(PhotoDragOut.admits(CGSize(width: 3, height: 0), CGSize(width: 327, height: 55), screen, upOnly: false).begin)
    }

    func testStillFingerDoesNotStart() {
        XCTAssertFalse(PhotoDragOut.admits(.zero, .zero, screen, upOnly: false).begin)
    }
}
