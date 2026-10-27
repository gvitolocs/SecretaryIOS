import CoreLocation
import Foundation

/// Server-side Google providers; credentials stay on the Secretary server.
struct MapsGeocode: Decodable, Sendable {
    let results: [PlaceResolver.Resolved]

    init(from decoder: Decoder) throws {
        let value = try JSONValue(from: decoder)
        results = (value["results"]?.arrayValue ?? []).compactMap { item in
            guard let lat = item["lat"]?.numberValue, let lng = item["lng"]?.numberValue,
                  Place.validCoordinate(lat, lng) != nil else { return nil }
            return PlaceResolver.Resolved(lat: lat, lng: lng, name: item["name"]?.nonEmptyString,
                                          address: item["address"]?.nonEmptyString)
        }
    }
}

enum PlaceTravelMode: String, CaseIterable, Sendable {
    case transit, walk, bike, drive

    var symbol: String {
        switch self {
        case .transit: return "tram.fill"
        case .walk: return "figure.walk"
        case .bike: return "bicycle"
        case .drive: return "car.fill"
        }
    }

    var title: String {
        switch self {
        case .transit: return String(localized: "By public transport")
        case .walk: return String(localized: "On foot")
        case .bike: return String(localized: "By bike")
        case .drive: return String(localized: "By car")
        }
    }
}

struct MapsRoute: Identifiable, Sendable {
    struct TransitLeg: Sendable {
        let line: String?
        let vehicle: String?
        let from: String?
        let to: String?
        let departAt: Date?
        let arriveAt: Date?
    }

    let mode: PlaceTravelMode
    let durationS: Double
    let staticDurationS: Double?
    let distanceM: Double?
    /// Validated latitude/longitude pairs, also used as the map's fit key.
    let polyline: [[Double]]
    let leaveAt: Date?
    let transit: [TransitLeg]
    var id: PlaceTravelMode { mode }
    var coordinates: [CLLocationCoordinate2D] {
        polyline.map { CLLocationCoordinate2D(latitude: $0[0], longitude: $0[1]) }
    }
    var durationText: String {
        let duration = PlaceFormat.duration(durationS)
        if mode == .drive, let staticDurationS, durationS > staticDurationS {
            return String(localized: "\(duration) with traffic")
        }
        return duration
    }

    init?(_ value: JSONValue) {
        guard let raw = value["mode"]?.stringValue, let mode = PlaceTravelMode(rawValue: raw),
              let duration = value["durationS"]?.numberValue, duration.isFinite, duration >= 0 else { return nil }
        self.mode = mode
        durationS = duration
        staticDurationS = value["staticDurationS"]?.numberValue.flatMap { $0.isFinite && $0 >= 0 ? $0 : nil }
        distanceM = value["distanceM"]?.numberValue.flatMap { $0.isFinite && $0 >= 0 ? $0 : nil }
        polyline = (value["polyline"]?.arrayValue ?? []).prefix(400).compactMap { point in
            guard let pair = point.arrayValue, pair.count == 2,
                  let lat = pair[0].numberValue, let lng = pair[1].numberValue,
                  Place.validCoordinate(lat, lng) != nil else { return nil }
            return [lat, lng]
        }
        leaveAt = Place.date(value["leaveAt"])
        transit = (value["transit"]?.arrayValue ?? []).compactMap { leg in
            guard leg.objectValue != nil else { return nil }
            return TransitLeg(line: leg["line"]?.nonEmptyString, vehicle: leg["vehicle"]?.nonEmptyString,
                              from: leg["from"]?.nonEmptyString, to: leg["to"]?.nonEmptyString,
                              departAt: Place.date(leg["departAt"]), arriveAt: Place.date(leg["arriveAt"]))
        }
    }
}

struct MapsRoutes: Decodable, Sendable {
    let provider: String?
    let routes: [MapsRoute]
    let failed: [String]

    init(from decoder: Decoder) throws {
        let value = try JSONValue(from: decoder)
        guard let entries = value["routes"]?.arrayValue else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Missing routes array"))
        }
        provider = value["provider"]?.stringValue
        var seen = Set<PlaceTravelMode>()
        routes = entries.compactMap(MapsRoute.init).filter { seen.insert($0.mode).inserted }
        failed = (value["failed"]?.arrayValue ?? []).compactMap(\.stringValue)
    }
}

extension SecretaryAPI {
    func mapsGeocode(q: String, near: CLLocationCoordinate2D?) async throws -> MapsGeocode {
        var query = [URLQueryItem(name: "q", value: q), URLQueryItem(name: "limit", value: "1")]
        if let near {
            query += [URLQueryItem(name: "lat", value: String(near.latitude)), URLQueryItem(name: "lng", value: String(near.longitude))]
        }
        return try await client.get("/v1/maps/geocode", query: query, timeout: 15)
    }

    func mapsRoutes(from: CLLocationCoordinate2D, to: CLLocationCoordinate2D, arrive: Date?,
                    modes: [PlaceTravelMode] = PlaceTravelMode.allCases) async throws -> MapsRoutes {
        var query = [URLQueryItem(name: "from", value: "\(from.latitude),\(from.longitude)"),
                     URLQueryItem(name: "to", value: "\(to.latitude),\(to.longitude)"),
                     URLQueryItem(name: "modes", value: modes.map(\.rawValue).joined(separator: ","))]
        if let arrive { query.append(URLQueryItem(name: "arrive", value: ISO8601DateFormatter().string(from: arrive))) }
        return try await client.get("/v1/maps/routes", query: query, timeout: 15)
    }
}
