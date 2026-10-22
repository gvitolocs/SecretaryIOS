import XCTest
@testable import Secretary

final class APIClientTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    private func error(status: Int, body: String, headers: [String: String] = [:]) async -> APIError? {
        StubURLProtocol.handler = { _ in .init(status: status, body: body, headers: headers.merging(["Content-Type": "application/json"]) { a, _ in a }) }
        let client = StubURLProtocol.makeClient()
        do {
            let _: SessionInfo = try await client.get("/v1/session")
            return nil
        } catch {
            return error as? APIError
        }
    }

    func testSendsBearerAndNoCSRF() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"authenticated":true,"deviceName":"Phone"}"#) }
        let client = StubURLProtocol.makeClient(key: "sec_abc")
        let session: SessionInfo = try await client.get("/v1/session")
        XCTAssertTrue(session.authenticated)
        let request = try XCTUnwrap(StubURLProtocol.requests.first)
        XCTAssertEqual(request.url?.absoluteString, "https://example.test/v1/session")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer sec_abc")
        XCTAssertNil(request.value(forHTTPHeaderField: "X-Secretary-CSRF"))
        XCTAssertEqual(request.timeoutInterval, 15)
    }

    func testPathsAreEncodedOnce() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"authenticated":true,"deviceName":"Phone"}"#) }
        let client = StubURLProtocol.makeClient()
        // Pre-encoded segments stay as they are; raw spaces and a stray `%` are encoded.
        for (path, expected) in [("/v1/notes/a%20b%2Fc", "/v1/notes/a%20b%2Fc"), ("/v1/assets/a b.webp", "/v1/assets/a%20b.webp"),
                                 ("/v1/x/100%", "/v1/x/100%25")] {
            let _: SessionInfo = try await client.get(path)
            XCTAssertEqual(StubURLProtocol.requests.last?.url?.absoluteString, "https://example.test" + expected)
        }
    }

    func testChatUsesTheLongTimeoutAndJSONBody() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"reply":"hi","action":"reply","job":null}"#) }
        let api = SecretaryAPI(client: StubURLProtocol.makeClient())
        let reply = try await api.chat(ChatRequest(message: "hello", sessionId: "ios-1", clientMessageId: "abc"))
        XCTAssertEqual(reply.reply, "hi")
        let request = try XCTUnwrap(StubURLProtocol.requests.first)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.timeoutInterval, 125)
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
        let body = try JSONSerialization.jsonObject(with: try XCTUnwrap(request.httpBody)) as? [String: String]
        XCTAssertEqual(body, ["message": "hello", "sessionId": "ios-1", "clientMessageId": "abc"])
    }

    func testLoginIsSentWithoutBearer() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"token":"sec_new","deviceId":"d1","deviceName":"iPhone"}"#) }
        let api = SecretaryAPI(client: StubURLProtocol.makeClient(key: "sec_old"))
        let token = try await api.login(DeviceLoginRequest(email: "owner@example.test", password: "pw", deviceName: "iPhone"))
        XCTAssertEqual(token.token, "sec_new")
        XCTAssertNil(StubURLProtocol.requests.first?.value(forHTTPHeaderField: "Authorization"))
        XCTAssertEqual(StubURLProtocol.requests.first?.url?.path, "/v1/auth/device")
    }

    func testUnauthorizedAndInvalidCredentials() async {
        let expired = await error(status: 401, body: #"{"error":{"code":"unauthorized","message":"device token revoked"}}"#)
        XCTAssertEqual(expired, .unauthorized(code: "unauthorized", message: "device token revoked"))
        XCTAssertEqual(expired?.endsSession, true)

        let wrongPassword = await error(status: 401, body: #"{"error":{"code":"invalid_credentials","message":"nope"}}"#)
        XCTAssertEqual(wrongPassword?.code, "invalid_credentials")
        XCTAssertEqual(wrongPassword?.endsSession, false)
        XCTAssertEqual(wrongPassword?.userMessage, "That email and password don't match.")
    }

    func testRateLimitAndLockoutCarryRetryAfter() async {
        let limited = await error(status: 429, body: #"{"error":{"code":"rate_limited","message":"Slow down"}}"#, headers: ["Retry-After": "12"])
        XCTAssertEqual(limited, .rateLimited(code: "rate_limited", retryAfter: 12, message: "Slow down"))
        let locked = await error(status: 429, body: #"{"error":{"code":"account_locked","message":"locked"}}"#, headers: ["Retry-After": "300"])
        XCTAssertEqual(locked?.code, "account_locked")
        XCTAssertEqual(locked?.userMessage, "Too many failed sign-ins. Try again in 300 s.")
        XCTAssertEqual(APIClient.parseRetryAfter("1.2"), 2)
        XCTAssertNil(APIClient.parseRetryAfter("soon"))
    }

    func testLucinaOutagesAreFlagged() async {
        let down = await error(status: 502, body: #"{"error":{"code":"lucina_unreachable","message":"down"}}"#)
        XCTAssertEqual(down, .lucinaOffline(code: "lucina_unreachable", message: "down"))
        XCTAssertEqual(down?.isLucinaOffline, true)
        let slow = await error(status: 504, body: #"{"error":{"code":"lucina_timeout","message":"slow"}}"#)
        XCTAssertEqual(slow?.status, 504)
        XCTAssertEqual(slow?.isLucinaOffline, true)
        // A bare 502 (proxy, no body) is the server being down, not Lucina.
        let bare = await error(status: 502, body: "")
        XCTAssertEqual(bare?.code, "http_502")
        XCTAssertEqual(bare?.isLucinaOffline, false)
    }

    func testServerErrorsKeepCodeAndMessage() async {
        let unavailable = await error(status: 503, body: #"{"error":{"code":"grocery_unavailable","message":"off"}}"#)
        XCTAssertEqual(unavailable, .server(status: 503, code: "grocery_unavailable", message: "off"))
        let engine = await error(status: 422, body: #"{"ok":false,"code":"invalid_command","error":"Start a workout first"}"#)
        XCTAssertEqual(engine, .server(status: 422, code: "invalid_command", message: "Start a workout first"))
        let html = await error(status: 500, body: "<html>oops</html>")
        XCTAssertEqual(html?.code, "http_500")
        XCTAssertEqual(html?.userMessage, "The server had a problem. Please try again.")
    }

    func testNetworkFailuresAndTimeouts() async {
        StubURLProtocol.handler = { _ in .init(error: URLError(.notConnectedToInternet)) }
        let client = StubURLProtocol.makeClient()
        do {
            let _: SessionInfo = try await client.get("/v1/session")
            XCTFail("expected an error")
        } catch let error as APIError {
            XCTAssertEqual(error.code, "network")
            XCTAssertTrue(error.isConnectivity)
        } catch {
            XCTFail("unexpected \(error)")
        }

        for code in [URLError.Code.cannotFindHost, .dnsLookupFailed] {
            StubURLProtocol.handler = { _ in .init(error: URLError(code)) }
            do {
                let _: SessionInfo = try await client.get("/v1/session")
                XCTFail("expected an error")
            } catch {
                XCTAssertEqual(error as? APIError, .hostNotFound(host: "example.test"))
                XCTAssertEqual((error as? APIError)?.userMessage, "Can't find example.test \u{2014} check DNS/VPN.")
                XCTAssertEqual((error as? APIError)?.isConnectivity, true)
            }
        }

        StubURLProtocol.handler = { _ in .init(error: URLError(.timedOut)) }
        do {
            let _: SessionInfo = try await client.get("/v1/session")
            XCTFail("expected an error")
        } catch {
            XCTAssertEqual(error as? APIError, .timeout)
        }
    }

    func testMalformedSuccessBodyIsADecodingError() async {
        StubURLProtocol.handler = { _ in .init(body: #"{"token": 5}"#) }
        do {
            _ = try await SecretaryAPI(client: StubURLProtocol.makeClient()).login(.init(email: "a@b", password: "p", deviceName: "d"))
            XCTFail("expected an error")
        } catch {
            XCTAssertEqual((error as? APIError)?.code, "decoding")
        }
    }

    func testGymTransportReturnsEngineBodiesOnConflictAndValidation() async throws {
        let transport = HTTPGymTransport(client: StubURLProtocol.makeClient())
        let request = EngineRequest(opId: "op", expectedRevision: 1, timezone: "UTC", command: GymCommand.finishWorkout)

        StubURLProtocol.handler = { _ in .init(status: 409, body: #"{"ok":false,"replayed":false,"code":"conflict","error":"State changed."}"#) }
        let conflict = try await transport.send(request)
        XCTAssertEqual(conflict.code, "conflict")

        StubURLProtocol.handler = { _ in .init(status: 422, body: #"{"ok":false,"replayed":false,"code":"invalid_command","error":"Complete at least one set before finishing"}"#) }
        let invalid = try await transport.send(request)
        XCTAssertEqual(invalid.error, "Complete at least one set before finishing")

        // Errors raised before the engine (401, 429...) still throw.
        StubURLProtocol.handler = { _ in .init(status: 401, body: #"{"error":{"code":"unauthorized","message":"x"}}"#) }
        do {
            _ = try await transport.send(request)
            XCTFail("expected an error")
        } catch {
            XCTAssertEqual((error as? APIError)?.status, 401)
        }

        // The state route carries the timezone.
        StubURLProtocol.handler = { _ in .init(body: #"{"ok":true,"replayed":false,"view":\#(Fixtures.gymView())}"#) }
        _ = try await transport.state(timezone: "Europe/Rome")
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.query, "timezone=Europe/Rome")
    }
}
