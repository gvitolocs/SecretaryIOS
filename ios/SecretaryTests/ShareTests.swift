import XCTest
@testable import Secretary

final class ShareTests: XCTestCase {
    private func directory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    func testReproducibleMultipartBodyAndRecord() throws {
        let directory = try directory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("hello.txt")
        try Data("hello\n".utf8).write(to: file)
        let payload = SharePayload(title: "Page", texts: ["snippet"], urls: ["https://example.com"],
                                   files: [ShareFile(url: file, name: "hello.txt", mime: "text/plain")])
        let record = try ShareRequestBuilder.build(payload: payload, intent: "ask", note: "Look", source: "ios-share",
            credentials: BridgeCredentials(token: "never-in-body", endpoints: ["https://example.com"], sources: ["share"]),
            directory: directory, shareId: "test-id", boundary: "test-boundary", language: "it")
        let fields = [("shareId", "test-id"), ("intent", "ask"), ("note", "Look"), ("lang", "it"),
                      ("channel", "ios"), ("source", "ios-share"), ("title", "Page"), ("text", "snippet"), ("url", "https://example.com")]
        var expected = fields.map { "--test-boundary\r\nContent-Disposition: form-data; name=\"\($0.0)\"\r\n\r\n\($0.1)\r\n" }.joined()
        expected += "--test-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hello.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\n\r\n--test-boundary--\r\n"
        XCTAssertEqual(try Data(contentsOf: directory.appendingPathComponent("test-id.body")), Data(expected.utf8))
        let metadata = try Data(contentsOf: directory.appendingPathComponent("test-id.json"))
        XCTAssertFalse(String(decoding: metadata, as: UTF8.self).contains("never-in-body"))
        let decoded = try JSONDecoder().decode(ShareRecord.self, from: metadata)
        XCTAssertEqual(decoded.boundary, "test-boundary")
        XCTAssertEqual(decoded.attempts, 1)
        XCTAssertEqual(record.endpoints, ["https://example.com"])
    }

    func testRejectsEmptyAndTooManyParts() throws {
        let directory = try directory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let credentials = BridgeCredentials(token: "x", endpoints: [], sources: [])
        XCTAssertThrowsError(try ShareRequestBuilder.build(payload: SharePayload(), intent: "ask", note: "", source: "ios-share", credentials: credentials, directory: directory))
        var payload = SharePayload()
        payload.texts = Array(repeating: "x", count: 32)
        XCTAssertThrowsError(try ShareRequestBuilder.build(payload: payload, intent: "ask", note: "", source: "ios-share", credentials: credentials, directory: directory))
    }

    func testAttachmentDecodingIsLenientAndRoundTrips() throws {
        let json = #"{"kind":"image","name":"photo.jpg","mime":"image/jpeg","mediaId":"itm_1","url":"https://example.com","bytes":123}"#
        let attachment = try JSONDecoder().decode(TimelineAttachment.self, from: Data(json.utf8))
        XCTAssertEqual(attachment.mediaId, "itm_1")
        XCTAssertEqual(attachment.mime, "image/jpeg")
        XCTAssertEqual(attachment.bytes, 123)
        XCTAssertEqual(try JSONDecoder().decode(TimelineAttachment.self, from: JSONEncoder().encode(attachment)), attachment)
        let old = try JSONDecoder().decode(TimelineAttachment.self, from: Data(#"{"kind":"pdf","name":"old.pdf"}"#.utf8))
        XCTAssertNil(old.mediaId)
        let encoded = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(old)) as? [String: Any])
        XCTAssertNil(encoded["mime"])
        let malformed = try JSONDecoder().decode(TimelineAttachment.self, from: Data(#"{"kind":"file","bytes":"unknown","mime":42,"mediaId":false}"#.utf8))
        XCTAssertNil(malformed.bytes)
        XCTAssertNil(malformed.mime)
        XCTAssertNil(malformed.mediaId)
    }

    func testAttachmentSymbols() {
        let symbols = ["pdf": "doc.richtext", "document": "doc.text", "audio": "waveform", "contact": "person.crop.circle",
                       "location": "mappin.and.ellipse", "text": "text.quote", "file": "doc", "link": "link", "image": "photo", "video": "video"]
        for (kind, symbol) in symbols { XCTAssertEqual(TimelineAttachment.symbol(for: kind), symbol) }
        XCTAssertEqual(TimelineAttachment.symbol(for: "new-kind"), "doc")
    }

    func testRetryPolicy() {
        for status in [200, 202, 204, 299] { XCTAssertEqual(ShareRetryPolicy.decision(status: status, attempts: 5), .success) }
        for status in [400, 403, 404, 413, 422] { XCTAssertEqual(ShareRetryPolicy.decision(status: status, attempts: 1), .discard) }
        for status in [nil, 401, 408, 429, 500, 503] as [Int?] {
            XCTAssertEqual(ShareRetryPolicy.decision(status: status, attempts: 4), .retry)
            XCTAssertEqual(ShareRetryPolicy.decision(status: status, attempts: 5), .exhausted)
        }
    }

    func testURLTextClassification() {
        var payload = SharePayload()
        ShareItemReader.addText("https://example.com/path", to: &payload)
        ShareItemReader.addText("Some words", to: &payload)
        ShareItemReader.addText("file:///private/secret", to: &payload)
        XCTAssertEqual(payload.urls, ["https://example.com/path"])
        XCTAssertEqual(payload.texts, ["Some words", "file:///private/secret"])
    }
}
