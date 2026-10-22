import AuthenticationServices
import Foundation
import Observation
import UIKit

/// The Links tab's state: the server's overview, the next week of events, and the
/// connect/disconnect calls. Google sign-in runs in `ASWebAuthenticationSession`;
/// secretary-server redirects to `secretary://links?google=…` when it is done.
@MainActor
@Observable
final class LinksStore {
    private(set) var overview: LinksOverview?
    private(set) var events: [CalendarEvent]?
    private(set) var loading = false
    private(set) var busyCard: String?
    var error: String?
    var notice: String?

    @ObservationIgnored private let client: APIClient
    @ObservationIgnored private var authSession: ASWebAuthenticationSession?
    @ObservationIgnored private let presenter = AuthPresenter()

    init(client: APIClient) {
        self.client = client
    }

    func load() async {
        loading = true
        defer { loading = false }
        do {
            overview = try await client.get("/v1/links")
            error = nil
        } catch {
            self.error = Self.message(error)
        }
        let page: EventsPage? = try? await client.get("/v1/calendar/events", query: [URLQueryItem(name: "days", value: "7")])
        events = page?.events
    }

    func sync() async {
        await run(card: "sync") {
            struct Empty: Encodable {}
            _ = try await self.client.send("POST", "/v1/calendar/sync", query: [], body: try APIClient.encoder.encode(Empty()), timeout: 60)
            self.notice = String(localized: "Calendars synced.")
        }
    }

    /// Opens Google's consent screen for `service` (calendar, gmail, drive).
    func connectGoogle(_ card: LinkCard) async {
        busyCard = card.id
        defer { busyCard = nil }
        do {
            struct Body: Encodable { let services: [String]; let client: String }
            struct Answer: Decodable { let authorizeUrl: String }
            let answer: Answer = try await client.send("POST", "/v1/links/google/connect",
                                                       body: Body(services: [card.service ?? "calendar"], client: "ios"))
            guard let url = URL(string: answer.authorizeUrl) else { throw APIError.invalidURL }
            let callback = try await authenticate(url)
            if let outcome = GoogleReturn(url: callback) { handle(outcome) }
            await load()
        } catch is CancellationError {
            // The owner closed the sign-in sheet.
        } catch let error as ASWebAuthenticationSessionError where error.code == .canceledLogin {
            // Same.
        } catch {
            self.error = Self.message(error)
        }
    }

    func handle(_ outcome: GoogleReturn) {
        switch outcome {
        case .connected: notice = String(localized: "Google is connected. Your calendar is syncing.")
        case let .failed(reason): error = String(localized: "Google sign-in didn't finish (\(reason)).")
        }
    }

    func disconnectGoogle() async {
        await run(card: "google") {
            _ = try await self.client.send("DELETE", "/v1/links/google", body: nil)
            self.notice = String(localized: "Disconnected.")
        }
    }

    func remove(_ card: LinkCard) async {
        let parts = card.id.split(separator: ":", maxSplits: 1).map(String.init)
        guard parts.count == 2 else { return }
        let path = parts[0] == "ics" ? "/v1/links/ics/" : "/v1/links/custom/"
        await run(card: card.id) {
            _ = try await self.client.send("DELETE", path + Self.segment(parts[1]), body: nil)
        }
    }

    func addSubscription(label: String, url: String) async -> Bool {
        struct Body: Encodable { let label: String?; let url: String }
        struct Answer: Decodable { let eventCount: Int? }
        do {
            let answer: Answer = try await client.send("POST", "/v1/links/ics",
                                                       body: Body(label: label.isEmpty ? nil : label, url: url), timeout: 30)
            notice = String(localized: "Subscribed: \(answer.eventCount ?? 0) events.")
            await load()
            return true
        } catch {
            self.error = Self.message(error)
            return false
        }
    }

    func addLink(kind: String, label: String, url: String, icon: String, tags: [String]) async -> Bool {
        struct Body: Encodable { let kind: String; let label: String; let url: String; let icon: String?; let tags: [String] }
        do {
            let _: JSONValue = try await client.send("POST", "/v1/links/custom",
                                                     body: Body(kind: kind, label: label, url: url, icon: icon.isEmpty ? nil : icon, tags: tags))
            notice = String(localized: "Link added.")
            await load()
            return true
        } catch {
            self.error = Self.message(error)
            return false
        }
    }

    func answer(_ proposal: CalendarProposal, accept: Bool) async {
        await run(card: proposal.id) {
            if accept {
                struct Body: Encodable { let confirm: Bool }
                let _: JSONValue = try await self.client.send("POST", "/v1/calendar/proposals/\(Self.segment(proposal.id))/accept",
                                                              body: Body(confirm: true))
                self.notice = String(localized: "Added to your calendar.")
            } else {
                _ = try await self.client.send("DELETE", "/v1/calendar/proposals/\(Self.segment(proposal.id))", body: nil)
            }
        }
    }

    // MARK: - Helpers

    private func run(card: String, _ work: @escaping () async throws -> Void) async {
        busyCard = card
        error = nil
        defer { busyCard = nil }
        do {
            try await work()
            await load()
        } catch {
            self.error = Self.message(error)
        }
    }

    private func authenticate(_ url: URL) async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            let session = ASWebAuthenticationSession(url: url, callbackURLScheme: "secretary") { callback, error in
                if let callback {
                    continuation.resume(returning: callback)
                } else {
                    continuation.resume(throwing: error ?? CancellationError())
                }
            }
            session.presentationContextProvider = presenter
            session.prefersEphemeralWebBrowserSession = false
            authSession = session
            if !session.start() { continuation.resume(throwing: CancellationError()) }
        }
    }

    static func segment(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/?#"))) ?? s
    }

    static func message(_ error: Error) -> String {
        if let api = error as? APIError { return api.localizedDescription }
        return error.localizedDescription
    }
}

/// Presents the Google sign-in sheet over the key window.
private final class AuthPresenter: NSObject, ASWebAuthenticationPresentationContextProviding {
    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        return scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? ASPresentationAnchor()
    }
}
