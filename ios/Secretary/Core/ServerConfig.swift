import Foundation

/// Where the app talks to. Production is fixed; Debug builds can point at a local mock
/// with the launch argument `-SecretaryBaseURL http://localhost:8791` or from the hidden
/// developer row in Settings.
enum ServerConfig {
    static let production = URL(string: "https://getsecretary.tech")!
    /// The same secretary-server behind `tailscale serve`, reachable only inside the tailnet.
    /// Used when the public name does not resolve (e.g. the Tailscale client's DNS on the phone).
    static let tailnet = URL(string: "https://secretary-p1.tail75dcfc.ts.net")!
    /// Same server, same data, same device token: tried in this order.
    static let productionEndpoints = [production, tailnet]

    static func label(for url: URL) -> String {
        switch url.host?.lowercased() {
        case production.host?.lowercased(): return "Public"
        case tailnet.host?.lowercased(): return "Private (Tailscale)"
        default: return url.host ?? url.absoluteString
        }
    }

    /// The endpoint list: a Debug override pins a single URL, otherwise public then tailnet.
    static func resolvedEndpoints(defaults: UserDefaults = .standard) -> [URL] {
        let url = resolvedBaseURL(defaults: defaults)
        return url == production ? productionEndpoints : [url]
    }

    /// Launch argument (UserDefaults argument domain), Debug only.
    static let launchArgumentKey = "SecretaryBaseURL"
    /// Persisted developer override, Debug only.
    static let overrideKey = "secretary.debug.baseURLOverride"

    static var isDebugBuild: Bool {
        #if DEBUG
        return true
        #else
        return false
        #endif
    }

    static func resolvedBaseURL(defaults: UserDefaults = .standard) -> URL {
        #if DEBUG
        if let raw = defaults.string(forKey: launchArgumentKey), let url = validated(raw) { return url }
        if let raw = defaults.string(forKey: overrideKey), let url = validated(raw) { return url }
        #endif
        return production
    }

    /// Whether the launch argument pins the URL (the developer row then only shows it).
    static func isPinnedByLaunchArgument(defaults: UserDefaults = .standard) -> Bool {
        #if DEBUG
        return defaults.string(forKey: launchArgumentKey).flatMap(validated) != nil
        #else
        return false
        #endif
    }

    /// Accepts `https://host[:port][/path]`, and `http://` only for local hosts (ATS allows
    /// nothing else).
    static func validated(_ raw: String) -> URL? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed), let scheme = url.scheme?.lowercased(), let host = url.host?.lowercased(), !host.isEmpty else {
            return nil
        }
        if scheme == "https" { return url }
        if scheme == "http", isLocal(host) { return url }
        return nil
    }

    static func isLocal(_ host: String) -> Bool {
        host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]" || host.hasSuffix(".local")
    }
}
