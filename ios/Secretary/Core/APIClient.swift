import Foundation
import os

/// Thin async JSON client for the Secretary server's `/v1` API.
///
/// - Auth: `Authorization: Bearer <device key>` on every request when a key is set
///   (Bearer requests need no CSRF header).
/// - Timeouts: 15 s by default, 45 s for chat (`ChatSendQueue.timeout`: a longer answer still arrives on the
///   timeline stream, and the bubble is replaced by the server's copy).
/// - Endpoints: an ordered list of base URLs for the *same* server (public, then the
///   private Tailscale name). A request that fails at the network layer is retried once
///   on the next endpoint; HTTP answers (any status) never fall back. The endpoint that
///   last worked is remembered and tried first.
/// - Errors: every failure becomes an `APIError`.
final class APIClient: @unchecked Sendable {
    static let defaultTimeout: TimeInterval = 15
    /// Also `ChatSendQueue.timeout`. The number lives here because the widget extension compiles this file without the chat.
    static let chatTimeout: TimeInterval = 45
    static let preferredEndpointKey = "secretary.server.lastWorkingEndpoint"

    struct Response: Sendable {
        let status: Int
        let data: Data
        let headers: [String: String]
    }

    private struct Config {
        var endpoints: [URL]
        /// Index into `endpoints` of the one tried first.
        var active: Int
        var accessKey: String?
    }

    private let config: OSAllocatedUnfairLock<Config>
    private let session: URLSession
    private let defaults: UserDefaults?
    /// Called (on any thread) when the active endpoint changes.
    var onEndpointChange: (@Sendable (URL) -> Void)?

    init(endpoints: [URL], accessKey: String? = nil, session: URLSession? = nil, defaults: UserDefaults? = .standard) {
        precondition(!endpoints.isEmpty, "at least one endpoint")
        self.defaults = defaults
        let remembered = defaults?.string(forKey: Self.preferredEndpointKey)
        let active = endpoints.firstIndex { $0.absoluteString == remembered } ?? 0
        config = OSAllocatedUnfairLock(initialState: Config(endpoints: endpoints, active: active, accessKey: accessKey))
        if let session {
            self.session = session
        } else {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
            configuration.httpCookieAcceptPolicy = .never
            configuration.httpShouldSetCookies = false
            configuration.waitsForConnectivity = false
            self.session = URLSession(configuration: configuration)
        }
    }

    convenience init(baseURL: URL, accessKey: String? = nil, session: URLSession? = nil) {
        self.init(endpoints: [baseURL], accessKey: accessKey, session: session, defaults: nil)
    }

    /// The endpoint requests go to first.
    var baseURL: URL { config.withLock { $0.endpoints[$0.active] } }

    var endpoints: [URL] { config.withLock { $0.endpoints } }

    var primaryURL: URL { config.withLock { $0.endpoints[0] } }

    /// Replaces the endpoint list (debug server override). Starts on the remembered one if present.
    func setEndpoints(_ endpoints: [URL]) {
        guard !endpoints.isEmpty else { return }
        let remembered = defaults?.string(forKey: Self.preferredEndpointKey)
        let url: URL = config.withLock { config in
            config.endpoints = endpoints
            config.active = endpoints.firstIndex { $0.absoluteString == remembered } ?? 0
            return config.endpoints[config.active]
        }
        onEndpointChange?(url)
    }

    var accessKey: String? {
        get { config.withLock { $0.accessKey } }
        set { config.withLock { $0.accessKey = newValue } }
    }

    /// Makes `url` the endpoint tried first and remembers it.
    func markWorking(_ url: URL) {
        let changed: Bool = config.withLock { config in
            guard let index = config.endpoints.firstIndex(of: url), index != config.active else { return false }
            config.active = index
            return true
        }
        guard changed else { return }
        if config.withLock({ $0.endpoints.count }) > 1 {
            defaults?.set(url.absoluteString, forKey: Self.preferredEndpointKey)
        }
        onEndpointChange?(url)
    }

    static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }()

    static let decoder = JSONDecoder()

    // MARK: - Typed helpers

    func get<T: Decodable>(_ path: String, query: [URLQueryItem] = [], timeout: TimeInterval = defaultTimeout,
                           headers: [String: String] = [:]) async throws -> T {
        let response = try await send("GET", path, query: query, body: nil, timeout: timeout, headers: headers)
        return try decode(T.self, from: response)
    }

    func send<T: Decodable, B: Encodable>(_ method: String, _ path: String, body: B, timeout: TimeInterval = defaultTimeout,
                                          idempotent: Bool? = nil, headers: [String: String] = [:]) async throws -> T {
        let data: Data
        do {
            data = try Self.encoder.encode(body)
        } catch {
            throw APIError.decoding(message: "Could not encode the request: \(error)")
        }
        let response = try await send(method, path, query: [], body: data, timeout: timeout, idempotent: idempotent, headers: headers)
        return try decode(T.self, from: response)
    }

    func decode<T: Decodable>(_ type: T.Type, from response: Response) throws -> T {
        do {
            return try Self.decoder.decode(T.self, from: response.data.isEmpty ? Data("null".utf8) : response.data)
        } catch {
            throw APIError.decoding(message: String(describing: error))
        }
    }

    // MARK: - Fallback rules

    /// Failures where the request certainly never reached a server: safe to send elsewhere.
    static let unreachedCodes: Set<URLError.Code> = [.cannotFindHost, .dnsLookupFailed, .cannotConnectToHost]
    /// Failures where it might have: only idempotent requests move on.
    static let ambiguousCodes: Set<URLError.Code> = [.timedOut, .networkConnectionLost]

    static func shouldFallBack(_ code: URLError.Code, idempotent: Bool) -> Bool {
        unreachedCodes.contains(code) || (idempotent && ambiguousCodes.contains(code))
    }

    // MARK: - Raw request

    /// Sends a request and returns the 2xx response. Non-2xx statuses throw, except the
    /// statuses listed in `passThrough`, which are returned as is (the gym/body engines
    /// answer 409/422/500 with an engine body the caller wants to read).
    ///
    /// `idempotent` (default: everything but POST) decides whether a timeout or a dropped
    /// connection may be retried on the next endpoint. Chat (`clientMessageId`) and engine
    /// commands (`op_id`) are idempotent POSTs and say so.
    func send(_ method: String, _ path: String, query: [URLQueryItem] = [], body: Data?,
              timeout: TimeInterval = defaultTimeout, passThrough: Set<Int> = [],
              authenticated: Bool = true, idempotent: Bool? = nil, headers: [String: String] = [:]) async throws -> Response {
        let snapshot = config.withLock { $0 }
        let order = [snapshot.active] + snapshot.endpoints.indices.filter { $0 != snapshot.active }
        let canRetry = idempotent ?? (method != "POST")
        var lastError: URLError?
        for (attempt, index) in order.prefix(2).enumerated() {
            let base = snapshot.endpoints[index]
            let request = try makeRequest(base: base, method: method, path: path, query: query, body: body, timeout: timeout,
                                          accessKey: authenticated ? snapshot.accessKey : nil, headers: headers)
            do {
                let (data, urlResponse) = try await session.data(for: request)
                guard let http = urlResponse as? HTTPURLResponse else {
                    throw APIError.network(message: "Not an HTTP response.")
                }
                // Any HTTP answer means this endpoint reaches the server.
                markWorking(base)
                var responseHeaders: [String: String] = [:]
                for (key, value) in http.allHeaderFields {
                    if let key = key as? String, let value = value as? String { responseHeaders[key.lowercased()] = value }
                }
                let response = Response(status: http.statusCode, data: data, headers: responseHeaders)
                if (200..<300).contains(http.statusCode) || passThrough.contains(http.statusCode) {
                    return response
                }
                throw Self.error(for: response)
            } catch let error as URLError where error.code == .cancelled {
                throw CancellationError()
            } catch let error as URLError {
                lastError = error
                if attempt == 0, order.count > 1, Self.shouldFallBack(error.code, idempotent: canRetry) { continue }
                throw Self.map(error, host: base.host)
            }
        }
        throw Self.map(lastError ?? URLError(.unknown), host: snapshot.endpoints[snapshot.active].host)
    }

    private func makeRequest(base: URL, method: String, path: String, query: [URLQueryItem], body: Data?,
                             timeout: TimeInterval, accessKey: String?, headers: [String: String] = [:]) throws -> URLRequest {
        guard var components = URLComponents(url: base, resolvingAgainstBaseURL: false),
              let scheme = components.scheme?.lowercased(), scheme == "https" || scheme == "http" else {
            throw APIError.invalidURL
        }
        // Callers percent-encode path segments themselves (an id with `/` or a space); `path` would
        // encode their `%` a second time. Anything else a path may not contain is still encoded here.
        let basePath = components.percentEncodedPath.hasSuffix("/") ? String(components.percentEncodedPath.dropLast()) : components.percentEncodedPath
        // A `%` that does not start an escape is a literal one (an invalid escape would trap).
        let literal = path.replacingOccurrences(of: "%(?![0-9A-Fa-f]{2})", with: "%25", options: .regularExpression)
        let encoded = literal.addingPercentEncoding(withAllowedCharacters: CharacterSet.urlPathAllowed.union(CharacterSet(charactersIn: "%"))) ?? literal
        components.percentEncodedPath = basePath + encoded
        if !query.isEmpty { components.queryItems = query }
        guard let url = components.url else { throw APIError.invalidURL }

        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: timeout)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let accessKey, !accessKey.isEmpty {
            request.setValue("Bearer \(accessKey)", forHTTPHeaderField: "Authorization")
        }
        for (name, value) in headers { request.setValue(value, forHTTPHeaderField: name) }
        if let body {
            request.httpBody = body
            // A caller that sends something other than JSON (a multipart upload) names its own type.
            if request.value(forHTTPHeaderField: "Content-Type") == nil {
                request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            }
        }
        return request
    }

    static func map(_ error: URLError, host: String?) -> APIError {
        switch error.code {
        case .timedOut: return .timeout
        case .cannotFindHost, .dnsLookupFailed: return .hostNotFound(host: host ?? "the server")
        default: return .network(message: error.localizedDescription)
        }
    }

    // MARK: - Streaming

    /// Opens a long-lived GET (Server-Sent Events) on the active endpoint with the Bearer
    /// token. Throws the usual `APIError` for non-2xx answers. No endpoint fallback here:
    /// the caller reconnects, and by then the active endpoint reflects other requests.
    func openStream(_ path: String, lastEventId: String?, idleTimeout: TimeInterval = 75) async throws -> URLSession.AsyncBytes {
        let snapshot = config.withLock { $0 }
        let base = snapshot.endpoints[snapshot.active]
        var headers = ["Accept": "text/event-stream", "Cache-Control": "no-cache"]
        if let lastEventId { headers["Last-Event-ID"] = lastEventId }
        let request = try makeRequest(base: base, method: "GET", path: path, query: [], body: nil, timeout: idleTimeout,
                                      accessKey: snapshot.accessKey, headers: headers)
        do {
            let (bytes, response) = try await session.bytes(for: request)
            guard let http = response as? HTTPURLResponse else { throw APIError.network(message: "Not an HTTP response.") }
            guard (200..<300).contains(http.statusCode) else {
                var data = Data()
                for try await byte in bytes { data.append(byte); if data.count > 16_384 { break } }
                throw Self.error(for: Response(status: http.statusCode, data: data, headers: [:]))
            }
            return bytes
        } catch let error as URLError where error.code == .cancelled {
            throw CancellationError()
        } catch let error as URLError {
            throw Self.map(error, host: base.host)
        }
    }

    // MARK: - Probing

    struct Probe: Equatable, Sendable {
        let url: URL
        /// Round trip in milliseconds when the server answered `/v1/health`.
        let latencyMs: Int?
        let error: String?
        var ok: Bool { latencyMs != nil }
    }

    /// `GET /v1/health` on one endpoint, without fallback (no auth, short timeout).
    func probe(_ base: URL, timeout: TimeInterval = 6) async -> Probe {
        do {
            let request = try makeRequest(base: base, method: "GET", path: "/v1/health", query: [], body: nil,
                                          timeout: timeout, accessKey: nil)
            let start = Date()
            let (_, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
                return Probe(url: base, latencyMs: nil, error: "HTTP \((response as? HTTPURLResponse)?.statusCode ?? 0)")
            }
            return Probe(url: base, latencyMs: max(1, Int(Date().timeIntervalSince(start) * 1000)), error: nil)
        } catch let error as URLError {
            return Probe(url: base, latencyMs: nil, error: Self.map(error, host: base.host).userMessage)
        } catch {
            return Probe(url: base, latencyMs: nil, error: APIError.wrap(error).userMessage)
        }
    }

    /// When a fallback endpoint is active, checks the primary and switches back if it answers.
    @discardableResult
    func reprobePrimary() async -> Bool {
        let (primary, onPrimary) = config.withLock { ($0.endpoints[0], $0.active == 0) }
        guard !onPrimary else { return true }
        let result = await probe(primary)
        if result.ok { markWorking(primary) }
        return result.ok
    }


    // MARK: - Error mapping

    private struct ErrorEnvelope: Decodable {
        struct Body: Decodable {
            let code: String?
            let message: String?
            let attemptsLeft: Int?
        }
        let error: Body?
        /// `pin_wrong` carries the tries left next to (or inside) the error.
        let attemptsLeft: Int?
    }

    /// Engine-shaped failure: `{"ok": false, "code": "...", "error": "..."}`.
    private struct EngineErrorBody: Decodable {
        let code: String?
        let error: String?
    }

    static func error(for response: Response) -> APIError {
        let status = response.status
        var code = "http_\(status)"
        var message = ""
        if let envelope = try? decoder.decode(ErrorEnvelope.self, from: response.data), let body = envelope.error {
            if let c = body.code, !c.isEmpty { code = c }
            if let m = body.message { message = m }
            if code == "pin_wrong", let left = body.attemptsLeft ?? envelope.attemptsLeft { message = "\(APIError.attemptsLeftPrefix)\(left)" }
        } else if let engine = try? decoder.decode(EngineErrorBody.self, from: response.data) {
            if let c = engine.code, !c.isEmpty { code = c }
            if let m = engine.error { message = m }
        }
        if code == "lucina_unreachable" || code == "lucina_timeout" {
            return .lucinaOffline(code: code, message: message)
        }
        switch status {
        case 401:
            return .unauthorized(code: code == "http_401" ? "unauthorized" : code, message: message)
        case 429:
            return .rateLimited(code: code == "http_429" ? "rate_limited" : code, retryAfter: parseRetryAfter(response.headers["retry-after"]), message: message)
        case 502 where code == "http_502", 504 where code == "http_504":
            // A bare gateway error without a body: the server (or its proxy) is down.
            return .server(status: status, code: code, message: "The server isn't reachable right now.")
        default:
            return .server(status: status, code: code, message: message)
        }
    }

    static func parseRetryAfter(_ value: String?) -> Int? {
        guard let value = value?.trimmingCharacters(in: .whitespaces), !value.isEmpty else { return nil }
        if let seconds = Double(value), seconds >= 0 { return Int(seconds.rounded(.up)) }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss zzz"
        guard let date = formatter.date(from: value) else { return nil }
        return max(0, Int(date.timeIntervalSinceNow.rounded(.up)))
    }
}
