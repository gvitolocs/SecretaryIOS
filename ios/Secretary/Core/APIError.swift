import Foundation

/// Every failure the app can get from the Secretary server, normalised so views
/// never inspect raw responses. Branch on `code` (the server's stable error code)
/// or on the case; `message` is for humans.
enum APIError: Error, Equatable, Sendable {
    /// 401: missing, unknown or revoked device key.
    case unauthorized(code: String, message: String)
    /// 429: the device or login rate limit. `retryAfter` is in seconds when the server sent it.
    case rateLimited(code: String, retryAfter: Int?, message: String)
    /// 502/504 with `lucina_unreachable` / `lucina_timeout`: the server is up, Lucina is not.
    case lucinaOffline(code: String, message: String)
    /// Any other non-2xx answer with (or without) the `{"error":{code,message}}` envelope.
    case server(status: Int, code: String, message: String)
    /// The host name did not resolve (NSURLErrorCannotFindHost / NSURLErrorDNSLookupFailed):
    /// usually DNS or a VPN on the phone, not the server.
    case hostNotFound(host: String)
    /// The request never got an HTTP answer (offline, refused, TLS...).
    case network(message: String)
    /// The client-side timeout fired (15 s, or 125 s for chat).
    case timeout
    /// A 2xx body that does not match the contract.
    case decoding(message: String)
    /// The configured server URL is unusable.
    case invalidURL

    var status: Int {
        switch self {
        case .unauthorized: return 401
        case .rateLimited: return 429
        case let .lucinaOffline(code, _): return code == "lucina_timeout" ? 504 : 502
        case let .server(status, _, _): return status
        case .hostNotFound, .network, .timeout, .decoding, .invalidURL: return 0
        }
    }

    var code: String {
        switch self {
        case let .unauthorized(code, _): return code
        case let .rateLimited(code, _, _): return code
        case let .lucinaOffline(code, _): return code
        case let .server(_, code, _): return code
        case .hostNotFound: return "host_not_found"
        case .network: return "network"
        case .timeout: return "timeout"
        case .decoding: return "decoding"
        case .invalidURL: return "invalid_url"
        }
    }

    /// A sentence that can be shown as is.
    var userMessage: String {
        switch self {
        case let .unauthorized(code, _):
            return code == "invalid_credentials" ? String(localized: "That email and password don't match.") : String(localized: "You were signed out. Sign in again.")
        case let .rateLimited(code, retryAfter, _):
            if code == "account_locked" {
                if let retryAfter, retryAfter > 0 { return String(localized: "Too many failed sign-ins. Try again in \(retryAfter) s.") }
                return String(localized: "Too many failed sign-ins. Try again later.")
            }
            if let retryAfter, retryAfter > 0 { return String(localized: "Too many requests. Try again in \(retryAfter) s.") }
            return String(localized: "Too many requests. Wait a moment and try again.")
        case let .lucinaOffline(code, _):
            return code == "lucina_timeout" ? String(localized: "Lucina didn't answer in time.") : String(localized: "Lucina is offline right now.")
        case let .server(status, _, message):
            if !message.isEmpty { return message }
            return status >= 500 ? String(localized: "The server had a problem. Please try again.") : String(localized: "Request failed (\(status)).")
        case let .hostNotFound(host):
            return String(localized: "Can't find \(host) \u{2014} check DNS/VPN.")
        case .network:
            return String(localized: "Can't reach the server. Check your connection.")
        case .timeout:
            return String(localized: "The server took too long to answer.")
        case .decoding:
            return String(localized: "The server sent an answer this version of the app doesn't understand.")
        case .invalidURL:
            return String(localized: "The server address is not valid.")
        }
    }

    /// A 401 that means the stored token is no longer good (not a wrong password).
    var endsSession: Bool {
        if case let .unauthorized(code, _) = self { return code != "invalid_credentials" }
        return false
    }

    var isLucinaOffline: Bool {
        if case .lucinaOffline = self { return true }
        return false
    }

    /// The server itself could not be reached (as opposed to an answer we didn't like).
    var isConnectivity: Bool {
        switch self {
        case .hostNotFound, .network, .timeout: return true
        default: return false
        }
    }

    static func wrap(_ error: Error) -> APIError {
        if let api = error as? APIError { return api }
        if error is DecodingError { return .decoding(message: String(describing: error)) }
        if let url = error as? URLError {
            if url.code == .timedOut { return .timeout }
            if url.code == .cannotFindHost || url.code == .dnsLookupFailed {
                return .hostNotFound(host: url.failingURL?.host ?? "the server")
            }
            return .network(message: url.localizedDescription)
        }
        return .network(message: error.localizedDescription)
    }
}

extension APIError: LocalizedError {
    var errorDescription: String? { userMessage }
}
