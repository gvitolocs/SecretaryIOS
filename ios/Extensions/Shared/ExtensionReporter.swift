import Foundation

/// Sends one device event from an app extension (Safari, Screen Time monitor) straight to
/// the server, with the device token the app put in the shared keychain group. It does
/// nothing unless the owner turned the matching source on (the app only writes the
/// credentials then).
enum ExtensionReporter {
    static func send(type: String, source: String, requires sourceSwitch: String, data: [String: Any], at date: Date = Date()) async -> Bool {
        guard let credentials = BridgeCredentials.read(), credentials.sources.contains(sourceSwitch), !credentials.token.isEmpty else {
            return false
        }
        let event: [String: Any] = [
            "id": UUID().uuidString,
            "ts": Int(date.timeIntervalSince1970 * 1000),
            "type": type,
            "source": source,
            "data": data,
        ]
        guard let body = try? JSONSerialization.data(withJSONObject: ["events": [event]]) else { return false }
        // Same order as the app: the endpoint that worked last first, then the others.
        for endpoint in credentials.endpoints {
            guard let base = URL(string: endpoint) else { continue }
            var request = URLRequest(url: base.appendingPathComponent("v1/device/events"), timeoutInterval: 15)
            request.httpMethod = "POST"
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue("Bearer \(credentials.token)", forHTTPHeaderField: "Authorization")
            do {
                let (_, response) = try await URLSession.shared.data(for: request)
                if let http = response as? HTTPURLResponse { return (200..<300).contains(http.statusCode) }
            } catch let error as URLError where [.cannotFindHost, .dnsLookupFailed, .cannotConnectToHost].contains(error.code) {
                continue // try the next endpoint (e.g. the Tailscale name)
            } catch {
                return false
            }
        }
        return false
    }
}
