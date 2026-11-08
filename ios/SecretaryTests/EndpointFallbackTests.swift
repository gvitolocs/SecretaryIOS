import XCTest
@testable import Secretary

final class EndpointFallbackTests: XCTestCase {
    private let primary = URL(string: "https://public.example.test")!
    private let secondary = URL(string: "https://private.example.ts.net")!
    private var defaults: UserDefaults!
    private var suite: String!

    override func setUp() {
        suite = "EndpointFallbackTests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suite)
    }

    private func makeClient(races: Bool = false) -> APIClient {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [StubURLProtocol.self]
        return APIClient(endpoints: [primary, secondary], accessKey: "sec_same_token",
                         session: URLSession(configuration: configuration), defaults: defaults, racesEndpoints: races)
    }

    private var paths: [String] { StubURLProtocol.requests.compactMap { r in r.url.map { "\($0.host ?? "")\($0.path)" } } }

    func testRaceSendsTheFirstRequestToTheEndpointThatAnswers() async throws {
        primaryFails(.timedOut)
        let client = makeClient(races: true)
        let session: SessionInfo = try await client.get("/v1/session")
        XCTAssertTrue(session.authenticated)
        XCTAssertEqual(Set(paths.prefix(2)), ["public.example.test/v1/health", "private.example.ts.net/v1/health"],
                       "both endpoints are asked at once")
        XCTAssertEqual(paths.last, "private.example.ts.net/v1/session", "the request goes where the answer came from, without a timeout first")
        XCTAssertEqual(paths.count, 3)
        XCTAssertEqual(client.baseURL, secondary)

        // Settled: the next request does not race again.
        StubURLProtocol.requests = []
        let _: SessionInfo = try await client.get("/v1/session")
        XCTAssertEqual(paths, ["private.example.ts.net/v1/session"])

        // A return to the foreground after a quiet spell races again.
        StubURLProtocol.requests = []
        client.resettle()
        let _: SessionInfo = try await client.get("/v1/session")
        XCTAssertEqual(paths.filter { $0.hasSuffix("/v1/health") }.count, 2)
    }

    func testRaceWithNoAnswerFallsBackToTheUsualOrder() async {
        StubURLProtocol.handler = { _ in .init(error: URLError(.cannotFindHost)) }
        let client = makeClient(races: true)
        _ = try? await client.get("/v1/session") as SessionInfo
        XCTAssertEqual(paths.filter { $0.hasSuffix("/v1/session") }, ["public.example.test/v1/session", "private.example.ts.net/v1/session"])

        // No second race right after a failed one.
        StubURLProtocol.requests = []
        _ = try? await client.get("/v1/session") as SessionInfo
        XCTAssertFalse(paths.contains { $0.hasSuffix("/v1/health") })
    }

    private var hosts: [String] { StubURLProtocol.requests.compactMap(\.url?.host) }

    private func primaryFails(_ code: URLError.Code) {
        StubURLProtocol.handler = { [primary] request in
            if request.url?.host == primary.host { return .init(error: URLError(code)) }
            return .init(body: #"{"authenticated":true,"deviceName":"Phone"}"#)
        }
    }

    func testDNSFailureOnThePrimaryUsesAndRemembersTheSecondary() async throws {
        primaryFails(.cannotFindHost)
        let client = makeClient()
        let session: SessionInfo = try await client.get("/v1/session")
        XCTAssertTrue(session.authenticated)
        XCTAssertEqual(hosts, ["public.example.test", "private.example.ts.net"])
        XCTAssertEqual(StubURLProtocol.requests.last?.value(forHTTPHeaderField: "Authorization"), "Bearer sec_same_token",
                       "the same device token works on both hosts")
        XCTAssertEqual(client.baseURL, secondary)
        XCTAssertEqual(defaults.string(forKey: APIClient.preferredEndpointKey), secondary.absoluteString)

        // The next request goes to the remembered endpoint first.
        StubURLProtocol.requests = []
        let _: SessionInfo = try await client.get("/v1/session")
        XCTAssertEqual(hosts, ["private.example.ts.net"])

        // A new client (next launch) starts on it too.
        XCTAssertEqual(makeClient().baseURL, secondary)
    }

    func testEveryUnreachedFailureFallsBack() async throws {
        for code in [URLError.Code.dnsLookupFailed, .cannotConnectToHost, .timedOut, .networkConnectionLost] {
            defaults.removeObject(forKey: APIClient.preferredEndpointKey)
            StubURLProtocol.requests = []
            primaryFails(code)
            let _: SessionInfo = try await makeClient().get("/v1/session")
            XCTAssertEqual(hosts, ["public.example.test", "private.example.ts.net"], "\(code)")
        }
    }

    func testHTTPErrorsOnThePrimaryDoNotFallBack() async {
        for (status, body) in [(401, #"{"error":{"code":"unauthorized","message":"revoked"}}"#),
                               (500, #"{"error":{"code":"internal","message":"boom"}}"#),
                               (502, #"{"error":{"code":"lucina_unreachable","message":"down"}}"#)] {
            StubURLProtocol.requests = []
            StubURLProtocol.handler = { _ in .init(status: status, body: body) }
            let client = makeClient()
            do {
                let _: SessionInfo = try await client.get("/v1/session")
                XCTFail("expected \(status)")
            } catch {
                XCTAssertEqual((error as? APIError)?.status, status)
            }
            XCTAssertEqual(hosts, ["public.example.test"], "HTTP \(status) comes from the server: no fallback")
            XCTAssertEqual(client.baseURL, primary)
        }
        XCTAssertNil(defaults.string(forKey: APIClient.preferredEndpointKey))
    }

    func testNonIdempotentPostDoesNotRetryAfterATimeout() async {
        primaryFails(.timedOut)
        let api = SecretaryAPI(client: makeClient())
        do {
            _ = try await api.addShopping(rawName: "milk")
            XCTFail("expected a timeout")
        } catch {
            XCTAssertEqual(error as? APIError, .timeout)
        }
        XCTAssertEqual(hosts, ["public.example.test"], "a timed-out add might have landed; don't send it twice")

        // But a POST that never reached a server (DNS) does move on.
        StubURLProtocol.requests = []
        primaryFails(.cannotFindHost)
        _ = try? await api.addShopping(rawName: "milk")
        XCTAssertEqual(hosts, ["public.example.test", "private.example.ts.net"])
    }

    func testIdempotentChatRetriesAfterAConnectionDrop() async throws {
        StubURLProtocol.handler = { [primary] request in
            if request.url?.host == primary.host { return .init(error: URLError(.networkConnectionLost)) }
            return .init(body: #"{"reply":"hi","action":"reply","job":null}"#)
        }
        let reply = try await SecretaryAPI(client: makeClient()).chat(.init(message: "x", sessionId: "s", clientMessageId: "c"))
        XCTAssertEqual(reply.reply, "hi")
        XCTAssertEqual(hosts, ["public.example.test", "private.example.ts.net"])
    }

    func testBothEndpointsDownReportsTheError() async {
        StubURLProtocol.handler = { _ in .init(error: URLError(.cannotFindHost)) }
        do {
            let _: SessionInfo = try await makeClient().get("/v1/session")
            XCTFail("expected an error")
        } catch {
            XCTAssertEqual((error as? APIError)?.code, "host_not_found")
        }
        XCTAssertEqual(hosts.count, 2, "one retry, not a loop")
    }

    func testReprobeSwitchesBackToThePrimary() async throws {
        defaults.set(secondary.absoluteString, forKey: APIClient.preferredEndpointKey)
        let client = makeClient()
        XCTAssertEqual(client.baseURL, secondary)

        StubURLProtocol.handler = { _ in .init(error: URLError(.cannotFindHost)) }
        let stillDown = await client.reprobePrimary()
        XCTAssertFalse(stillDown)
        XCTAssertEqual(client.baseURL, secondary)

        StubURLProtocol.handler = { _ in .init(body: #"{"ok":true}"#) }
        let back = await client.reprobePrimary()
        XCTAssertTrue(back)
        XCTAssertEqual(client.baseURL, primary)
        XCTAssertEqual(defaults.string(forKey: APIClient.preferredEndpointKey), primary.absoluteString)
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.path, "/v1/health")
        XCTAssertNil(StubURLProtocol.requests.last?.value(forHTTPHeaderField: "Authorization"), "health probes carry no token")
    }

    func testProbeReportsEachEndpoint() async {
        primaryFails(.cannotFindHost)
        let client = makeClient()
        let down = await client.probe(primary)
        let up = await client.probe(secondary)
        XCTAssertFalse(down.ok)
        XCTAssertEqual(down.error, String(localized: "Can't find \("public.example.test") \u{2014} check DNS/VPN."))
        XCTAssertTrue(up.ok)
        XCTAssertNotNil(up.latencyMs)
    }

    func testEndpointConfig() {
        XCTAssertEqual(ServerConfig.productionEndpoints.map(\.absoluteString),
                       ["https://getsecretary.tech", "https://secretary-p1.tail75dcfc.ts.net"])
        XCTAssertEqual(ServerConfig.label(for: ServerConfig.production), "Public")
        XCTAssertEqual(ServerConfig.label(for: ServerConfig.tailnet), "Private (Tailscale)")
    }
}
