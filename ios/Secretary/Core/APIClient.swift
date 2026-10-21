import Foundation
import os

/// Thin async JSON client for the Secretary server's `/v1` API.
///
/// - Auth: `Authorization: Bearer <device key>` on every request when a key is set
///   (Bearer requests need no CSRF header).
/// - Timeouts: 15 s by default, 125 s for chat (the server allows Lucina 120 s).
/// - Errors: every failure becomes an `APIError`.
final class APIClient: @unchecked Sendable {
    static let defaultTimeout: TimeInterval = 15
    static let chatTimeout: TimeInterval = 125

    struct Response: Sendable {
        let status: Int
        let data: Data
        let headers: [String: String]
    }

    private struct Config {
        var baseURL: URL
        var accessKey: String?
    }

    private let config: OSAllocatedUnfairLock<Config>
    private let session: URLSession

    init(baseURL: URL, accessKey: String? = nil, session: URLSession? = nil) {
        config = OSAllocatedUnfairLock(initialState: Config(baseURL: baseURL, accessKey: accessKey))
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

    var baseURL: URL {
        get { config.withLock { $0.baseURL } }
        set { config.withLock { $0.baseURL = newValue } }
    }

    var accessKey: String? {
        get { config.withLock { $0.accessKey } }
        set { config.withLock { $0.accessKey = newValue } }
    }

    static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }()

    static let decoder = JSONDecoder()

    // MARK: - Typed helpers

    func get<T: Decodable>(_ path: String, query: [URLQueryItem] = [], timeout: TimeInterval = defaultTimeout) async throws -> T {
        let response = try await send("GET", path, query: query, body: nil, timeout: timeout)
        return try decode(T.self, from: response)
    }

    func send<T: Decodable, B: Encodable>(_ method: String, _ path: String, body: B, timeout: TimeInterval = defaultTimeout) async throws -> T {
        let data: Data
        do {
            data = try Self.encoder.encode(body)
        } catch {
            throw APIError.decoding(message: "Could not encode the request: \(error)")
        }
        let response = try await send(method, path, query: [], body: data, timeout: timeout)
        return try decode(T.self, from: response)
    }

    func decode<T: Decodable>(_ type: T.Type, from response: Response) throws -> T {
        do {
            return try Self.decoder.decode(T.self, from: response.data.isEmpty ? Data("null".utf8) : response.data)
        } catch {
            throw APIError.decoding(message: String(describing: error))
        }
    }

    // MARK: - Raw request

    /// Sends a request and returns the 2xx response. Non-2xx statuses throw, except the
    /// statuses listed in `passThrough`, which are returned as is (the gym/body engines
    /// answer 409/422/500 with an engine body the caller wants to read).
    func send(_ method: String, _ path: String, query: [URLQueryItem] = [], body: Data?,
              timeout: TimeInterval = defaultTimeout, passThrough: Set<Int> = [],
              authenticated: Bool = true) async throws -> Response {
        let snapshot = config.withLock { $0 }
        guard var components = URLComponents(url: snapshot.baseURL, resolvingAgainstBaseURL: false),
              let scheme = components.scheme?.lowercased(), scheme == "https" || scheme == "http" else {
            throw APIError.invalidURL
        }
        let basePath = components.path.hasSuffix("/") ? String(components.path.dropLast()) : components.path
        components.path = basePath + path
        if !query.isEmpty { components.queryItems = query }
        guard let url = components.url else { throw APIError.invalidURL }

        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: timeout)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if authenticated, let key = snapshot.accessKey, !key.isEmpty {
            request.setValue("Bearer \(key)", forHTTPHeaderField: "Authorization")
        }
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let data: Data
        let urlResponse: URLResponse
        do {
            (data, urlResponse) = try await session.data(for: request)
        } catch let error as URLError where error.code == .timedOut {
            throw APIError.timeout
        } catch is CancellationError {
            throw CancellationError()
        } catch let error as URLError where error.code == .cancelled {
            throw CancellationError()
        } catch {
            throw APIError.network(message: error.localizedDescription)
        }
        guard let http = urlResponse as? HTTPURLResponse else {
            throw APIError.network(message: "Not an HTTP response.")
        }
        var headers: [String: String] = [:]
        for (key, value) in http.allHeaderFields {
            if let key = key as? String, let value = value as? String { headers[key.lowercased()] = value }
        }
        let response = Response(status: http.statusCode, data: data, headers: headers)
        if (200..<300).contains(http.statusCode) || passThrough.contains(http.statusCode) {
            return response
        }
        throw Self.error(for: response)
    }

    // MARK: - Error mapping

    private struct ErrorEnvelope: Decodable {
        struct Body: Decodable {
            let code: String?
            let message: String?
        }
        let error: Body?
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
