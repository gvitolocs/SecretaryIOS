import Foundation
import Observation

/// `GET /v1/health` (no auth), polled while the app is in front.
@Observable
@MainActor
final class HealthStore {
    enum Status: Equatable {
        case unknown
        case online(latencyMs: Int?)
        case lucinaDown(reason: String?)
        case serverDown
    }

    private(set) var status: Status = .unknown
    private(set) var version: String?
    private(set) var checkedAt: Date?

    @ObservationIgnored private let api: () -> SecretaryAPI

    init(api: @escaping () -> SecretaryAPI) {
        self.api = api
    }

    func refresh() async {
        do {
            let health = try await api().health()
            version = health.version
            if let lucina = health.lucina, lucina.reachable {
                status = .online(latencyMs: lucina.latencyMs)
            } else {
                status = .lucinaDown(reason: health.lucina?.error)
            }
        } catch {
            status = .serverDown
        }
        checkedAt = Date()
    }

    /// Refreshes every 30 s until the task is cancelled.
    func poll() async {
        while !Task.isCancelled {
            await refresh()
            try? await Task.sleep(for: .seconds(30))
        }
    }

    var summary: String {
        switch status {
        case .unknown: return "Checking…"
        case let .online(latency): return latency.map { "Lucina online · \($0) ms" } ?? "Lucina online"
        case let .lucinaDown(reason):
            switch reason {
            case "lucina_timeout": return "Lucina isn't answering"
            case "not_configured", "lucina_not_configured": return "Lucina isn't configured on the server"
            default: return "Lucina is offline"
            }
        case .serverDown: return "Server unreachable"
        }
    }
}
