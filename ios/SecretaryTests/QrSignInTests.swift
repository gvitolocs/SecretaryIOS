import XCTest
@testable import Secretary

final class QrSignInTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    func testReadsApproveLinksAndTypedCodes() {
        let cases: [(String, String?, String)] = [
            ("https://getsecretary.tech/approve?c=w0CwKBb_4q6MacWLPEja7w", nil, "w0CwKBb_4q6MacWLPEja7w"),
            ("  https://getsecretary.tech/approve?c=abcdef123\n", nil, "abcdef123"),
            ("https://GetSecretary.tech/approve?c=abc-def_12", nil, "abc-def_12"),
            // The server the app talks to (a dev server) may be plain http.
            ("http://192.168.1.20:8791/approve?c=localcode1", "192.168.1.20", "localcode1"),
            ("482913", nil, "482913"),
            ("482 913", nil, "482913"),
            ("482-913", nil, "482913"),
        ]
        for (text, host, code) in cases {
            XCTAssertEqual(QrSignInCode.code(from: text, serverHost: host), code, text)
        }
    }

    func testIgnoresEverythingElse() {
        for text in [
            "https://evil.example/approve?c=abcdef123",
            "http://getsecretary.tech/approve?c=abcdef123",
            "https://getsecretary.tech/other?c=abcdef123",
            "https://getsecretary.tech/approve",
            "https://getsecretary.tech/approve?c=abc",
            "https://getsecretary.tech/approve?c=bad%20code",
            "https://getsecretary.tech/approve?c=\(String(repeating: "a", count: 65))",
            "12345", "1234567", "４８２９１３", "hello", "",
        ] {
            XCTAssertNil(QrSignInCode.code(from: text, serverHost: "app.example"), text)
        }
    }

    func testDescribesUserAgents() {
        let mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15"
        XCTAssertEqual(UserAgentSummary.describe(mac).browser, "Safari")
        XCTAssertEqual(UserAgentSummary.describe(mac).os, "macOS")
        let edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0"
        XCTAssertEqual(UserAgentSummary.describe(edge).browser, "Edge")
        XCTAssertEqual(UserAgentSummary.describe(edge).os, "Windows")
        let embedded = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)"
        XCTAssertEqual(UserAgentSummary.describe(embedded).browser, "Secretary app")
        let curl = UserAgentSummary.describe("curl/8.5.0")
        XCTAssertNil(curl.browser)
        XCTAssertNil(curl.os)
        XCTAssertNil(QrSignInInfo(ip: "1.2.3.4", userAgent: "curl/8.5.0").deviceDescription)
        // "Firefox on Linux" in the device's language ("Firefox su Linux", "Firefox på Linux").
        let firefox = QrSignInInfo(ip: "1.2.3.4", userAgent: "Mozilla/5.0 (X11; Linux x86_64; rv:144.0) Gecko/20100101 Firefox/144.0")
        XCTAssertEqual(firefox.deviceDescription, String(localized: "\("Firefox") on \("Linux")"))
        XCTAssertTrue(firefox.deviceDescription?.hasPrefix("Firefox ") ?? false)
        XCTAssertTrue(firefox.deviceDescription?.hasSuffix(" Linux") ?? false)
    }

    func testAsksForInfoAndApprovesWithTheDeviceToken() async throws {
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/session/qr/info" {
                return .init(body: #"{"ip": "203.0.113.7", "userAgent": "Mozilla/5.0 (Macintosh) Version/26.0 Safari/605.1.15", "createdAt": "2026-10-27T21:00:00Z", "expiresIn": 90}"#)
            }
            return .init(body: #"{"status": "approved"}"#)
        }
        let api = QrSignInAPI(client: StubURLProtocol.makeClient())
        let info = try await api.info("482913")
        XCTAssertEqual(info.ip, "203.0.113.7")
        XCTAssertEqual(info.expiresIn, 90)
        XCTAssertEqual(info.createdAt, ISO8601DateFormatter().date(from: "2026-10-27T21:00:00Z"))
        try await api.answer("482913", approve: true)

        let requests = StubURLProtocol.requests
        XCTAssertEqual(requests.count, 2)
        XCTAssertEqual(requests[0].url?.query, "c=482913")
        XCTAssertEqual(requests[1].httpMethod, "POST")
        XCTAssertEqual(requests[1].url?.path, "/v1/session/qr/approve")
        XCTAssertTrue(requests[1].value(forHTTPHeaderField: "Authorization")?.hasPrefix("Bearer ") ?? false)
        let body = try XCTUnwrap(requests[1].httpBody)
        XCTAssertEqual(try JSONSerialization.jsonObject(with: body) as? NSDictionary, ["code": "482913", "approve": true])
    }

    func testReadsTheServersNanosecondTimestamps() {
        let plain = ISO8601DateFormatter().date(from: "2026-10-27T21:18:47Z")
        XCTAssertEqual(QrSignInInfo.date("2026-10-27T21:18:47.499728123Z"), plain)
        XCTAssertEqual(QrSignInInfo.date("2026-10-27T21:18:47Z"), plain)
        XCTAssertNil(QrSignInInfo.date("yesterday"))
    }

    func testAnExpiredCodeIsA404() async {
        StubURLProtocol.handler = { _ in
            .init(status: 404, body: #"{"error": {"code": "qr_not_found", "message": "gone"}}"#)
        }
        do {
            _ = try await QrSignInAPI(client: StubURLProtocol.makeClient()).info("482913")
            XCTFail("expected an error")
        } catch let error as APIError {
            XCTAssertEqual(error.status, 404)
            XCTAssertEqual(error.code, "qr_not_found")
        } catch {
            XCTFail("unexpected \(error)")
        }
    }
}
