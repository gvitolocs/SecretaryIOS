import Foundation

/// The server's merged calendar; these models are separate from Links' compact event previews.
struct CalendarEntry: Decodable, Identifiable, Equatable, Sendable {
    var id = ""
    var source = ""
    var sourceId = ""
    var calendar: String?
    var title = ""
    var start = ""
    var end = ""
    var allDay = false
    var startMs: Double = 0
    var endMs: Double = 0
    var location: String?
    var description: String?
    var url: String?
    var recurring = false
    var editable = false
    var attendees: [String] = []
    var addedBy: String?
    var conferenceUrl: String?

    init() {}
    private enum CodingKeys: String, CodingKey {
        case id, source, sourceId, calendar, title, start, end, allDay, startMs, endMs, location, description, url, recurring, editable, attendees, addedBy, conferenceUrl
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        func text(_ key: CodingKeys) -> String? { (try? c.decode(String.self, forKey: key)) ?? (try? c.decode(Int64.self, forKey: key)).map { String($0) } }
        id = text(.id) ?? ""; source = text(.source) ?? ""; sourceId = text(.sourceId) ?? ""
        calendar = text(.calendar); title = text(.title) ?? ""; start = text(.start) ?? ""; end = text(.end) ?? ""
        allDay = (try? c.decode(Bool.self, forKey: .allDay)) ?? false
        startMs = (try? c.decode(Double.self, forKey: .startMs)) ?? CalendarMath.parse(start).map { $0.timeIntervalSince1970 * 1000 } ?? 0
        endMs = (try? c.decode(Double.self, forKey: .endMs)) ?? CalendarMath.parse(end).map { $0.timeIntervalSince1970 * 1000 } ?? startMs + 3_600_000
        location = text(.location); description = text(.description); url = text(.url)
        recurring = (try? c.decode(Bool.self, forKey: .recurring)) ?? false
        editable = (try? c.decode(Bool.self, forKey: .editable)) ?? false
        attendees = (try? c.decode([String].self, forKey: .attendees)) ?? []
        addedBy = text(.addedBy); conferenceUrl = text(.conferenceUrl)
    }
    var calendarName: String {
        if source == "lucina" { return "Lucina" }
        return calendar?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfCalendarEmpty
            ?? (source == "google" ? "Google" : source == "iphone" ? "iPhone" : source)
    }
    var startDate: Date { Date(timeIntervalSince1970: startMs / 1000) }
    var endDate: Date { Date(timeIntervalSince1970: endMs / 1000) }
    var publicLink: URL {
        var c = URLComponents(string: "https://getsecretary.tech/calendar")!
        c.percentEncodedQuery = "event=" + CalendarAPI.segment(id) + "&at=" + String(format: "%.0f", startMs)
        return c.url!
    }
}

private extension String {
    var nilIfCalendarEmpty: String? { isEmpty ? nil : self }
}

struct CalendarEventDraft: Codable, Equatable, Sendable {
    var title: String?
    var start: String?
    var end: String?
    var allDay: Bool?
    var location: String?
    var description: String?
    var conferenceUrl: String?
    var confirm: Bool?
}

struct CalendarPendingProposal: Decodable, Identifiable, Sendable {
    var id: String
    var event: CalendarEventDraft
    var from: String
    var createdAt: String
    var expiresAtMs: Double
    var reason: String?
    private enum CodingKeys: String, CodingKey { case id, event, from, createdAt, expiresAtMs, reason }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? (try? c.decode(Int64.self, forKey: .id)).map { String($0) } ?? ""
        event = (try? c.decode(CalendarEventDraft.self, forKey: .event)) ?? CalendarEventDraft()
        from = (try? c.decode(String.self, forKey: .from)) ?? ""
        createdAt = (try? c.decode(String.self, forKey: .createdAt)) ?? ""
        expiresAtMs = (try? c.decode(Double.self, forKey: .expiresAtMs)) ?? 0
        reason = try? c.decode(String.self, forKey: .reason)
    }
}

struct CalendarEventsPage: Decodable, Sendable {
    var from: String
    var to: String
    var timeZone: String
    var events: [CalendarEntry]
    var sources: JSONValue?
    private enum CodingKeys: String, CodingKey { case from, to, timeZone, events, sources }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        from = (try? c.decode(String.self, forKey: .from)) ?? ""
        to = (try? c.decode(String.self, forKey: .to)) ?? ""
        timeZone = (try? c.decode(String.self, forKey: .timeZone)) ?? TimeZone.current.identifier
        events = (try? c.decode(LossyArray<CalendarEntry>.self, forKey: .events))?.values ?? []
        sources = try? c.decode(JSONValue.self, forKey: .sources)
    }
}

/// Supports both the server's direct answers and `{ok, data}` proxy envelopes.
struct CalendarEnvelope<T: Decodable>: Decodable {
    let value: T
    private enum CodingKeys: String, CodingKey { case ok, data }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if (try? c.decode(Bool.self, forKey: .ok)) == false {
            throw APIError.server(status: 200, code: "calendar_not_ok", message: "")
        }
        value = c.contains(.data) ? try c.decode(T.self, forKey: .data) : try T(from: decoder)
    }
}

struct CalendarAPI: Sendable {
    let client: APIClient
    struct Mutation: Decodable { let event: CalendarEntry? }
    private struct Proposals: Decodable { let proposals: [CalendarPendingProposal]? }
    private struct Confirm: Encodable { let confirm = true }
    private struct Empty: Encodable {}

    func events(from: Date, to: Date) async throws -> CalendarEventsPage {
        let page: CalendarEnvelope<CalendarEventsPage> = try await client.get("/v1/calendar/events", query: [
            URLQueryItem(name: "from", value: CalendarMath.instant(from)), URLQueryItem(name: "to", value: CalendarMath.instant(to))])
        return page.value
    }
    func proposals() async throws -> [CalendarPendingProposal] {
        let page: CalendarEnvelope<Proposals> = try await client.get("/v1/calendar/proposals")
        return page.value.proposals ?? []
    }
    func save(_ draft: CalendarEventDraft, id: String?) async throws -> CalendarEntry? {
        var body = draft; body.confirm = id == nil ? nil : true
        let path = "/v1/calendar/events" + (id.map { "/" + Self.segment($0) } ?? "")
        let result: CalendarEnvelope<Mutation> = try await client.send(id == nil ? "POST" : "PATCH", path, body: body)
        return result.value.event
    }
    func delete(_ id: String) async throws {
        _ = try await client.send("DELETE", "/v1/calendar/events/" + Self.segment(id), query: [URLQueryItem(name: "confirm", value: "true")], body: nil)
    }
    func accept(_ id: String) async throws -> CalendarEntry? {
        let result: CalendarEnvelope<Mutation> = try await client.send("POST", "/v1/calendar/proposals/" + Self.segment(id) + "/accept", body: Confirm())
        return result.value.event
    }
    func dismiss(_ id: String) async throws {
        _ = try await client.send("DELETE", "/v1/calendar/proposals/" + Self.segment(id), body: nil)
    }
    func sync() async throws -> JSONValue {
        let result: CalendarEnvelope<JSONValue> = try await client.send("POST", "/v1/calendar/sync", body: Empty(), timeout: 60)
        return result.value
    }
    static func segment(_ id: String) -> String { id.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? "" }
}
