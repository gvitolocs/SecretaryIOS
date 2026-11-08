import CoreLocation
import MapKit

// Lucina's own navigation: every place action lands on the app's map (PlaceMapView), and its
// Navigate button follows the owner along the route in the selected mode, by default the bike
// (MapKit's route and its turn-by-turn steps). Nothing here opens another app.

/// In-app links to a place: `/map/place?…` (`AppModel.open(route:)`, `secretary://open/map/place?…`).
enum PlaceLinks {
    /// The route of the in-app map for a place; `navigate` starts the navigation right away.
    static func route(_ place: Place, navigate: Bool = false) -> String {
        let iso = ISO8601DateFormatter()
        var components = URLComponents()
        components.path = "/map/place"
        var items = [URLQueryItem(name: "title", value: place.title)]
        if let address = place.address { items.append(URLQueryItem(name: "address", value: address)) }
        if let lat = place.lat, let lng = place.lng {
            items += [URLQueryItem(name: "lat", value: String(lat)), URLQueryItem(name: "lng", value: String(lng))]
        }
        if let when = place.when { items.append(URLQueryItem(name: "when", value: iso.string(from: when))) }
        if let end = place.end { items.append(URLQueryItem(name: "end", value: iso.string(from: end))) }
        if let eventId = place.eventId { items.append(URLQueryItem(name: "eventId", value: eventId)) }
        if navigate { items.append(URLQueryItem(name: "navigate", value: "1")) }
        components.queryItems = items
        return components.string ?? "/map/place"
    }

    /// A `/map/place?…&navigate=1` route: open the map already navigating.
    static func navigates(_ route: String) -> Bool {
        URLComponents(string: route)?.queryItems?.contains { $0.name == "navigate" && ($0.value == "1" || $0.value == "true") } ?? false
    }

    /// A maps link (Apple Maps, Google Maps) tapped in the chat, as a place for the app's map; nil
    /// for any other URL.
    static func place(fromMapsLink url: URL) -> Place? {
        guard ExternalMaps.isExternal(url) else { return nil }
        if let place = Place.fromMapsURL(url, name: nil) { return place }
        // No coordinates: the searched text or destination, geocoded by the map itself.
        let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        for key in ["daddr", "destination", "address", "q", "query"] {
            guard let value = items.first(where: { $0.name == key })?.value else { continue }
            let text = RichMessage.line(.string(value), 200)
            if !text.isEmpty { return Place(title: RichMessage.line(.string(text), 120), address: text) }
        }
        return nil
    }
}

/// One turn-by-turn instruction and its stretch of road, start to end (latitude/longitude pairs).
struct NavigationStep: Equatable, Sendable {
    let instruction: String
    let line: [[Double]]

    var start: CLLocationCoordinate2D? { line.first.map { CLLocationCoordinate2D(latitude: $0[0], longitude: $0[1]) } }
}

/// A route from MapKit in one travel mode (transit: only the time, Apple gives no line or turns).
/// Where Apple has no cycling route, the bike's is a marked estimate from the distance at cycling
/// speed: a walking or car time is never shown as the bike's.
struct PlaceRoute: Equatable, Sendable {
    enum Source: String, Sendable {
        /// MapKit's route in that mode.
        case mapKit
        /// Bike estimate: the length of MapKit's walking path, whose line and turns stand in for the bike's.
        case walkingPath
        /// Bike estimate: the straight line plus a margin for the roads; no line and no turns.
        case straightLine
    }

    /// Cycling speed for an estimate, in metres a second (15 km/h, about what MapKit gives in Aarhus).
    static let estimateSpeed: Double = 15 / 3.6
    /// Roads are about this much longer than the straight line.
    static let roadFactor: Double = 1.3
    /// Further than this nobody cycles: no route rather than an estimate.
    static let estimateLimitMeters: Double = 100_000

    let mode: PlaceTravelMode
    let source: Source
    let seconds: TimeInterval
    let meters: Double
    let steps: [NavigationStep]

    var estimated: Bool { source != .mapKit }
    var line: [[Double]] { steps.flatMap(\.line) }
    /// "By bike", marked when it is an estimate.
    var modeText: String { estimated ? String(localized: "By bike (estimate)") : mode.title }
    /// "14 min", "≈ 14 min" when it is an estimate.
    var durationText: String {
        let duration = PlaceFormat.duration(seconds)
        return estimated ? "≈ \(duration)" : duration
    }

    /// The bike estimate for a stretch of road, the walking path's or `straightLine`'s.
    static func estimate(_ source: Source, meters: Double, steps: [NavigationStep] = []) -> PlaceRoute {
        PlaceRoute(mode: .bike, source: source, seconds: meters / estimateSpeed, meters: meters, steps: steps)
    }

    /// The bike estimate from the straight line; nil past `estimateLimitMeters`.
    static func straightLine(_ meters: Double) -> PlaceRoute? {
        meters <= estimateLimitMeters ? estimate(.straightLine, meters: meters * roadFactor) : nil
    }

    static func calculate(_ mode: PlaceTravelMode, from origin: CLLocationCoordinate2D,
                          to destination: CLLocationCoordinate2D) async -> PlaceRoute? {
        switch mode {
        case .bike:
            if let route = await mapKitRoute(.bike, from: origin, to: destination) { return route }
            if let path = await mapKitRoute(.walk, from: origin, to: destination), path.meters <= estimateLimitMeters {
                return estimate(.walkingPath, meters: path.meters, steps: path.steps)
            }
            return straightLine(NavigationGuide.meters(origin, destination))
        case .walk, .drive:
            return await mapKitRoute(mode, from: origin, to: destination)
        case .transit:
            let request = request(.transit, from: origin, to: destination)
            guard let eta = try? await MKDirections(request: request).calculateETA() else { return nil }
            return PlaceRoute(mode: .transit, source: .mapKit, seconds: eta.expectedTravelTime, meters: eta.distance, steps: [])
        }
    }

    private static func request(_ transport: MKDirectionsTransportType, from origin: CLLocationCoordinate2D,
                                to destination: CLLocationCoordinate2D) -> MKDirections.Request {
        let request = MKDirections.Request()
        request.source = MKMapItem(placemark: MKPlacemark(coordinate: origin))
        request.destination = MKMapItem(placemark: MKPlacemark(coordinate: destination))
        request.transportType = transport
        return request
    }

    private static func mapKitRoute(_ mode: PlaceTravelMode, from origin: CLLocationCoordinate2D,
                                    to destination: CLLocationCoordinate2D) async -> PlaceRoute? {
        let transport: MKDirectionsTransportType = mode == .bike ? .cycling : mode == .drive ? .automobile : .walking
        guard let route = try? await MKDirections(request: request(transport, from: origin, to: destination)).calculate().routes.first else {
            return nil
        }
        return PlaceRoute(mode: mode, source: .mapKit, seconds: route.expectedTravelTime, meters: route.distance, steps: steps(of: route))
    }

    private static func steps(of route: MKRoute) -> [NavigationStep] {
        route.steps.map { step in
            let polyline = step.polyline
            var coordinates = [CLLocationCoordinate2D](repeating: kCLLocationCoordinate2DInvalid, count: polyline.pointCount)
            polyline.getCoordinates(&coordinates, range: NSRange(location: 0, length: polyline.pointCount))
            return NavigationStep(instruction: step.instructions,
                                  line: coordinates.filter(CLLocationCoordinate2DIsValid).map { [$0.latitude, $0.longitude] })
        }
    }
}

enum NavigationGuide {
    /// Closer than this to the place: arrived.
    static let arrivalMeters: Double = 30

    /// The next instruction from where the owner is, and the metres to where it applies: the owner
    /// is on the step whose line passes nearest, the next one is the following step with words.
    /// Nil when no step is left (the last stretch to the place).
    static func next(_ steps: [NavigationStep], at position: CLLocationCoordinate2D) -> (step: NavigationStep, meters: Double)? {
        var nearest: (index: Int, meters: Double)?
        for (index, step) in steps.enumerated() {
            guard let meters = distance(from: position, to: step.line) else { continue }
            if nearest == nil || meters < nearest!.meters { nearest = (index, meters) }
        }
        guard let current = nearest?.index else { return nil }
        guard let next = steps[(current + 1)...].first(where: { !$0.instruction.isEmpty && $0.start != nil }),
              let start = next.start else { return nil }
        return (next, meters(position, start))
    }

    static func meters(_ a: CLLocationCoordinate2D, _ b: CLLocationCoordinate2D) -> Double {
        CLLocation(latitude: a.latitude, longitude: a.longitude).distance(from: CLLocation(latitude: b.latitude, longitude: b.longitude))
    }

    /// Metres from the point to the nearest segment of the line (a single point counts too).
    static func distance(from point: CLLocationCoordinate2D, to line: [[Double]]) -> Double? {
        let points = line.compactMap { pair -> MKMapPoint? in
            guard pair.count == 2 else { return nil }
            return MKMapPoint(CLLocationCoordinate2D(latitude: pair[0], longitude: pair[1]))
        }
        guard let first = points.first else { return nil }
        let p = MKMapPoint(point)
        var best = p.distance(to: first)
        for (a, b) in zip(points, points.dropFirst()) {
            let dx = b.x - a.x, dy = b.y - a.y
            let length = dx * dx + dy * dy
            let t = length > 0 ? max(0, min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / length)) : 0
            best = min(best, p.distance(to: MKMapPoint(x: a.x + t * dx, y: a.y + t * dy)))
        }
        return best
    }
}
