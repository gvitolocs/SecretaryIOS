import CoreLocation
import Foundation

// A place in the chat: a `place` block of Lucina message format v1 (docs/lucina-message-format.md),
// `meta.place` on a proactive message (a calendar reminder), a shared location or a maps link.
// It comes from the network, so `parse` validates it again: a title (one line, 120 chars) and an
// address (one line, 200 chars) or a valid coordinate pair; anything else is dropped.

struct Place: Codable, Hashable, Sendable {
    enum Kind: String, Codable, Sendable { case event, place }

    /// At most this many place blocks per message (they count against the 8-block limit).
    static let maxPerMessage = 3

    let title: String
    let address: String?
    let lat: Double?
    let lng: Double?
    /// The appointment's start and end.
    let when: Date?
    let end: Date?
    /// The calendar event behind it.
    let eventId: String?
    let kind: Kind

    init(title: String, address: String? = nil, coordinate: CLLocationCoordinate2D? = nil, when: Date? = nil,
         end: Date? = nil, eventId: String? = nil, kind: Kind = .place) {
        let valid = coordinate.flatMap { Place.validCoordinate($0.latitude, $0.longitude) }
        self.title = title
        self.address = address
        lat = valid?.latitude
        lng = valid?.longitude
        self.when = when
        self.end = end
        self.eventId = eventId
        self.kind = kind
    }

    var coordinate: CLLocationCoordinate2D? {
        guard let lat, let lng else { return nil }
        return CLLocationCoordinate2D(latitude: lat, longitude: lng)
    }

    /// What MKLocalSearch looks for when there are no coordinates.
    var searchQuery: String { address ?? title }

    /// "📍 Title — address" (plain-text rendering).
    var plainText: String { address.map { "📍 \(title) — \($0)" } ?? "📍 \(title)" }

    /// The time line of the card: "27 Oct 2026, 18:00", or a range when it ends later.
    var timeText: String? {
        guard let when else { return nil }
        if let end, end > when {
            return (when..<end).formatted(date: .abbreviated, time: .shortened)
        }
        return when.formatted(date: .abbreviated, time: .shortened)
    }

    // MARK: Validation

    /// A place block (`type` is ignored) or `meta.place`, or nil when it is not valid.
    static func parse(_ v: JSONValue?) -> Place? {
        guard let v, case .object = v else { return nil }
        let title = RichMessage.line(v["title"], 120)
        guard !title.isEmpty else { return nil }
        let address = RichMessage.line(v["address"], 200)
        let coordinate = number(v["lat"]).flatMap { lat in number(v["lng"]).flatMap { validCoordinate(lat, $0) } }
        guard !address.isEmpty || coordinate != nil else { return nil }
        let eventId = v["eventId"]?.nonEmptyString.flatMap { $0.count <= 200 ? $0 : nil }
        return Place(title: title, address: address.isEmpty ? nil : address, coordinate: coordinate,
                     when: date(v["when"]), end: date(v["end"]), eventId: eventId,
                     kind: v["kind"]?.stringValue == "event" ? .event : .place)
    }

    /// Both finite, in range, and not (0, 0).
    static func validCoordinate(_ lat: Double, _ lng: Double) -> CLLocationCoordinate2D? {
        guard lat.isFinite, lng.isFinite, (-90...90).contains(lat), (-180...180).contains(lng), !(lat == 0 && lng == 0) else { return nil }
        return CLLocationCoordinate2D(latitude: lat, longitude: lng)
    }

    private static func number(_ v: JSONValue?) -> Double? {
        switch v {
        case let .number(n)?: return n
        case let .string(s)?: return Double(s.trimmingCharacters(in: .whitespaces))
        default: return nil
        }
    }

    /// RFC 3339, with or without fractional seconds.
    static func date(_ v: JSONValue?) -> Date? {
        guard let text = v?.nonEmptyString else { return nil }
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = formatter.date(from: text) { return date }
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: text)
    }

    /// Widget, notification and custom-scheme destinations share the message validator.
    static func fromRoute(_ route: String) -> Place? {
        guard let components = URLComponents(string: route) else { return nil }
        let path: String
        if components.scheme != nil {
            guard components.scheme?.lowercased() == "secretary", components.host == "map" else { return nil }
            path = "/map" + components.path
        } else {
            guard components.host == nil else { return nil }
            path = components.path
        }
        guard path == "/map/place" else { return nil }
        let allowed: Set<String> = ["title", "address", "lat", "lng", "when", "end", "eventId"]
        var fields: [String: JSONValue] = [:]
        for item in components.queryItems ?? [] where allowed.contains(item.name) {
            if let value = item.value, fields[item.name] == nil { fields[item.name] = .string(value) }
        }
        if fields["when"] != nil || fields["eventId"] != nil { fields["kind"] = .string("event") }
        return parse(.object(fields))
    }

    // MARK: Maps links

    /// A shared Apple Maps or Google Maps link that carries coordinates (`ll=`, `q=lat,lng`,
    /// `/@lat,lng,`, `!3dlat!4dlng`), or nil. `name` is the link's title when it has one.
    static func fromMapsURL(_ url: URL, name: String?) -> Place? {
        let host = url.host?.lowercased() ?? ""
        let scheme = url.scheme?.lowercased() ?? ""
        let apple = scheme == "maps" || host == "maps.apple.com" || host == "maps.apple"
        let google = host.hasPrefix("maps.google.") || (host.contains("google.") && url.path.hasPrefix("/maps"))
        guard apple || google else { return nil }
        let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        func item(_ key: String) -> String? { items.first(where: { $0.name == key })?.value?.trimmedOrNil }
        var coordinate: CLLocationCoordinate2D?
        for key in ["ll", "coordinate", "q", "query", "destination", "daddr", "sll", "center"] {
            if let value = item(key), let found = pair(value) {
                coordinate = found
                break
            }
        }
        let text = url.absoluteString.removingPercentEncoding ?? url.absoluteString
        if coordinate == nil, google {
            coordinate = match(#"!3d(-?\d+(?:\.\d+)?)!4d(-?\d+(?:\.\d+)?)"#, in: text) ?? match(#"/@(-?\d+(?:\.\d+)?),(-?\d+(?:\.\d+)?)"#, in: text)
        }
        guard let coordinate else { return nil }
        // A name: the link title, else a non-coordinate `q`/`name`, else the `/place/<name>/` path part.
        var title = name?.trimmedOrNil
        if title == nil { title = item("name") ?? item("q").flatMap { pair($0) == nil ? $0 : nil } }
        let path = url.path
        if title == nil, google, let range = path.range(of: "/place/") {
            title = path[range.upperBound...].split(separator: "/").first
                .map { String($0).replacingOccurrences(of: "+", with: " ") }?.trimmedOrNil
        }
        let line = RichMessage.line(.string(title ?? String(localized: "Location")), 120)
        return Place(title: line, coordinate: coordinate)
    }

    private static func pair(_ value: String) -> CLLocationCoordinate2D? {
        let parts = value.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
        guard parts.count >= 2, let lat = Double(parts[0]), let lng = Double(parts[1]) else { return nil }
        return validCoordinate(lat, lng)
    }

    private static func match(_ pattern: String, in text: String) -> CLLocationCoordinate2D? {
        guard let regex = try? NSRegularExpression(pattern: pattern),
              let m = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)), m.numberOfRanges == 3,
              let a = Range(m.range(at: 1), in: text), let b = Range(m.range(at: 2), in: text),
              let lat = Double(text[a]), let lng = Double(text[b]) else { return nil }
        return validCoordinate(lat, lng)
    }

    // MARK: Codable (the block's own shape, so a cached message parses the same way)

    private enum CodingKeys: String, CodingKey { case title, address, lat, lng, when, end, eventId, kind }

    init(from decoder: Decoder) throws {
        guard let place = Place.parse(try JSONValue(from: decoder)) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "not a valid place"))
        }
        self = place
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        try c.encode(title, forKey: .title)
        try c.encodeIfPresent(address, forKey: .address)
        try c.encodeIfPresent(lat, forKey: .lat)
        try c.encodeIfPresent(lng, forKey: .lng)
        try c.encodeIfPresent(when.map(formatter.string(from:)), forKey: .when)
        try c.encodeIfPresent(end.map(formatter.string(from:)), forKey: .end)
        try c.encodeIfPresent(eventId, forKey: .eventId)
        try c.encode(kind.rawValue, forKey: .kind)
    }
}

private extension String {
    var trimmedOrNil: String? {
        let trimmed = trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}
