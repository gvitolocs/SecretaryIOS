import XCTest
@testable import Secretary

/// The debug event recorder (docs/debug-events.md): the sanitizer drops forbidden fields and cleans values, the ring
/// buffer keeps its caps and the upload mark.
final class DebugEventsTests: XCTestCase {
    func testForbiddenFieldNamesAreDropped() {
        let fields: [String: Any] = [
            "token": "sec_abc", "authToken": "x", "device_token": "x", "unlock_key": "x", "privateKey": "x",
            "signature": "x", "password": "x", "Cookie": "x", "message": "hello", "message_text": "hi",
            "note_text": "x", "command": "rm -rf /", "plan": "x", "body": "{}", "response_body": "x", "url": "x",
            "requestURL": "x", "query": "x", "title": "x", "otp_code": "123456", "apiKey": "x",
            // kept
            "id": "ap_1", "action": "approve", "sensitive": true, "outcome": "approved", "status": 409,
            "duration_ms": 12, "keyboard_shown": false,
        ]
        let clean = DebugEvents.sanitize(fields)
        XCTAssertEqual(Set(clean.keys), ["id", "action", "sensitive", "outcome", "status", "duration_ms", "keyboard_shown", "dropped_fields"])
        XCTAssertEqual(clean["dropped_fields"], .int(21))
        XCTAssertEqual(clean["sensitive"], .bool(true))
        XCTAssertEqual(clean["status"], .int(409))
    }

    func testValuesAreCleanedAndSecretsRedacted() {
        let clean = DebugEvents.sanitize([
            "path": "/v1/approvals/ap_8f3a19c2d4e5/decide?pin=1234",
            "route": "/v1/notes/shopping-list",
            "endpoint": "https://user:pw@getsecretary.tech/v1/x?token=abc#frag",
            "error": "Bearer sec_abcdef",
            "raw": String(repeating: "a", count: 64),
            "long": String(repeating: "word ", count: 100),
            "failure": URLError(.notConnectedToInternet),
            "api": APIError.server(status: 409, code: "already_decided", message: "Already approved by the iPad"),
        ])
        XCTAssertEqual(clean["path"], .string("/v1/approvals/{id}/decide"))
        XCTAssertEqual(clean["route"], .string("/v1/notes/{id}"))
        XCTAssertEqual(clean["endpoint"], .string("https://getsecretary.tech/v1/x"))
        XCTAssertEqual(clean["error"], .string(DebugEvents.redacted))
        XCTAssertEqual(clean["raw"], .string(DebugEvents.redacted))
        guard case let .string(long)? = clean["long"] else { return XCTFail("long") }
        XCTAssertEqual(long.count, DebugEvents.maxValueChars)
        XCTAssertEqual(clean["failure"], .string("NSURLErrorDomain:-1009"))
        XCTAssertEqual(clean["api"], .string("APIError:409:already_decided"))
    }

    func testTypesAndPaths() {
        XCTAssertTrue(DebugEvents.isValidType("notification.action"))
        XCTAssertFalse(DebugEvents.isValidType("UI.Tap"))
        XCTAssertFalse(DebugEvents.isValidType("ui..tap"))
        XCTAssertEqual(DebugEvents.templatePath("/v1/devices/dev_3fa2b1c9/unlock"), "/v1/devices/{id}/unlock")
        XCTAssertEqual(DebugEvents.templatePath("/v1/x/3f2504e0-4f89-11d3-9a0c-0305e82c3301"), "/v1/x/{id}")
        XCTAssertEqual(DebugEvents.templatePath("/v1/approvals/{id}"), "/v1/approvals/{id}")
        XCTAssertEqual(DebugEvents.templatePath("/v1/shopping/lists"), "/v1/shopping/lists")
    }

    func testBufferAppendsCompactsAndTracksTheUploadMark() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: dir) }
        let buffer = DebugEvents.Buffer(directory: dir, process: "test", build: "abc1234")
        for i in 0..<(DebugEvents.maxEvents + 10) {
            buffer.append(type: "ui.tap", ts: Date(), fields: ["n": .int(i)])
        }
        let lines = buffer.lines()
        XCTAssertLessThanOrEqual(lines.count, DebugEvents.maxEvents)
        let last = try XCTUnwrap(JSONSerialization.jsonObject(with: try XCTUnwrap(lines.last)) as? [String: Any])
        XCTAssertEqual(last["seq"] as? Int, DebugEvents.maxEvents + 10)
        XCTAssertEqual(last["build"] as? String, "abc1234")
        XCTAssertEqual(last["process"] as? String, "test")
        XCTAssertEqual(last["type"] as? String, "ui.tap")
        XCTAssertNil(buffer.append(type: "Bad Type", ts: Date(), fields: [:]))

        let first = try XCTUnwrap(buffer.pending(limit: DebugEvents.batchSize))
        XCTAssertEqual(first.lines.count, DebugEvents.batchSize)
        buffer.markUploaded(through: first.maxSeq)
        let next = try XCTUnwrap(buffer.pending(limit: DebugEvents.batchSize))
        let nextFirst = try XCTUnwrap(JSONSerialization.jsonObject(with: next.lines[0]) as? [String: Any])
        XCTAssertEqual(nextFirst["seq"] as? Int, first.maxSeq + 1)
        // The mark survives a new buffer object (a relaunch).
        let reopened = DebugEvents.Buffer(directory: dir, process: "test", build: "abc1234")
        XCTAssertEqual(reopened.pending(limit: 1)?.maxSeq, first.maxSeq + 1)
    }
}
