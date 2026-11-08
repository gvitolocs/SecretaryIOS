import Foundation

// Wire types of the Links hub and the calendar (docs/server-api.md, "Links" and
// "Calendar"). The server computes every status; unknown fields are ignored and
// every optional field is decoded defensively.

struct LinkCard: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let kind: String
    let name: String
    let status: String
    let service: String?
    let account: String?
    let scopes: [String]
    let lastSyncAt: Date?
    let detail: String?
    let error: String?
    let count: Int?
    let url: URL?
    let icon: String?
    let tags: [String]
    let actions: [String]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        kind = (try? c.decodeIfPresent(String.self, forKey: .kind)) ?? "other"
        name = (try? c.decodeIfPresent(String.self, forKey: .name)) ?? id
        status = (try? c.decodeIfPresent(String.self, forKey: .status)) ?? "unknown"
        service = try? c.decodeIfPresent(String.self, forKey: .service)
        account = try? c.decodeIfPresent(String.self, forKey: .account)
        scopes = (try? c.decodeIfPresent([String].self, forKey: .scopes)) ?? []
        lastSyncAt = (try? c.decodeIfPresent(String.self, forKey: .lastSyncAt)).flatMap(LinkDates.parse)
        detail = try? c.decodeIfPresent(String.self, forKey: .detail)
        error = try? c.decodeIfPresent(String.self, forKey: .error)
        count = try? c.decodeIfPresent(Int.self, forKey: .count)
        url = (try? c.decodeIfPresent(String.self, forKey: .url)).flatMap { $0.flatMap(URL.init(string:)) }
        icon = try? c.decodeIfPresent(String.self, forKey: .icon)
        tags = (try? c.decodeIfPresent([String].self, forKey: .tags)) ?? []
        actions = ((try? c.decodeIfPresent([String].self, forKey: .actions)) ?? []).filter { $0 != "edit" }
    }

    private enum CodingKeys: String, CodingKey {
        case id, kind, name, status, service, account, scopes, lastSyncAt, detail, error, count, url, icon, tags, actions
    }
}

struct LinkGroup: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let cards: [LinkCard]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? "other"
        cards = (try? c.decodeIfPresent([Lossy<LinkCard>].self, forKey: .cards))?.compactMap(\.value) ?? []
    }

    private enum CodingKeys: String, CodingKey { case id, cards }
}

struct ProposedEvent: Decodable, Equatable, Sendable {
    let title: String
    let start: String
    let location: String?
}

struct CalendarProposal: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let event: ProposedEvent
    let from: String
}

struct LinksOverview: Decodable, Equatable, Sendable {
    let timeZone: String
    let googleConfigured: Bool
    let groups: [LinkGroup]
    let proposals: [CalendarProposal]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        timeZone = (try? c.decodeIfPresent(String.self, forKey: .timeZone)) ?? TimeZone.current.identifier
        struct Google: Decodable { let configured: Bool? }
        googleConfigured = (try? c.decodeIfPresent(Google.self, forKey: .google))?.configured ?? false
        groups = (try? c.decodeIfPresent([LinkGroup].self, forKey: .groups)) ?? []
        proposals = (try? c.decodeIfPresent([Lossy<CalendarProposal>].self, forKey: .proposals))?.compactMap(\.value) ?? []
    }

    private enum CodingKeys: String, CodingKey { case timeZone, google, groups, proposals }
}

struct CalendarEvent: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let source: String
    let calendar: String?
    let title: String
    let start: String
    let allDay: Bool
    let startMs: Double
    let location: String?

    var startDate: Date { Date(timeIntervalSince1970: startMs / 1000) }
}

struct EventsPage: Decodable, Sendable {
    let events: [CalendarEvent]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        events = (try? c.decodeIfPresent([Lossy<CalendarEvent>].self, forKey: .events))?.compactMap(\.value) ?? []
    }

    private enum CodingKeys: String, CodingKey { case events }
}

enum LinkDates {
    static func parse(_ text: String?) -> Date? {
        guard let text else { return nil }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: text) ?? ISO8601DateFormatter().date(from: text)
    }
}

/// The Google OAuth return (`secretary://links?google=connected|error&reason=…`).
enum GoogleReturn: Equatable {
    case connected
    case failed(String)

    init?(url: URL) {
        guard url.scheme == "secretary", url.host == "links",
              let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems,
              let outcome = items.first(where: { $0.name == "google" })?.value else { return nil }
        if outcome == "connected" {
            self = .connected
        } else {
            self = .failed(items.first(where: { $0.name == "reason" })?.value ?? "unknown")
        }
    }
}
