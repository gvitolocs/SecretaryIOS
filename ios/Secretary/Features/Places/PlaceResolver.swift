import CoreLocation
import MapKit
import Observation
import UIKit

// Where a place is, where the owner is, and how far: the server geocoder, then MKLocalSearch without
// coordinates (biased to the current region, else home, else Denmark), the when-in-use location,
// home from `GET /v1/maps/config`, distances and MKDirections travel times. Nothing here talks
// to anyone but Apple's MapKit services and the app's own server.

/// `GET /v1/maps/config` (secretary-maps): only `home` is used on iOS. Decoded leniently.
struct MapsConfig: Decodable, Sendable {
    let home: DealsMap.Point?

    init(home: DealsMap.Point?) { self.home = home }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        home = try? c.decodeIfPresent(DealsMap.Point.self, forKey: .home)
    }

    private enum CodingKeys: String, CodingKey { case home }
}

extension SecretaryAPI {
    /// `GET /v1/maps/config`; nil on any error (no map server, offline, odd JSON).
    func mapsConfig() async -> MapsConfig? {
        try? await client.get("/v1/maps/config", timeout: 15)
    }
}

/// Finds the coordinate of a place and remembers it for the session.
@MainActor
@Observable
final class PlaceResolver {
    static let shared = PlaceResolver()

    /// What the server geocoder or MKLocalSearch found (or the place's own coordinate).
    struct Resolved: Equatable, Sendable {
        let lat: Double
        let lng: Double
        /// The map item's name and address (nil for a place that had coordinates).
        let name: String?
        let address: String?

        var coordinate: CLLocationCoordinate2D { CLLocationCoordinate2D(latitude: lat, longitude: lng) }
    }

    /// Home from the map server, once loaded.
    private(set) var home: CLLocationCoordinate2D?

    @ObservationIgnored private var cache: [String: Resolved?] = [:]
    @ObservationIgnored private var searches: [String: Task<Resolved?, Never>] = [:]
    @ObservationIgnored private var homeTask: Task<Void, Never>?

    /// Denmark, when neither the current position nor home is known.
    private static let denmark = MKCoordinateRegion(center: CLLocationCoordinate2D(latitude: 56.0, longitude: 10.6),
                                                    span: MKCoordinateSpan(latitudeDelta: 4, longitudeDelta: 7))

    /// The place's coordinate, else server geocoding with MKLocalSearch as a fallback.
    func resolve(_ place: Place, api: SecretaryAPI) async -> Resolved? {
        if let lat = place.lat, let lng = place.lng { return Resolved(lat: lat, lng: lng, name: nil, address: nil) }
        let query = place.searchQuery
        if let hit = cache[query] { return hit }
        if let running = searches[query] { return await running.value }
        await loadHome(api: api)
        let region = biasRegion()
        let task = Task<Resolved?, Never> {
            if let result = try? await api.mapsGeocode(q: query, near: region.center).results.first { return result }
            return await Self.search(query, region: region)
        }
        searches[query] = task
        let found = await task.value
        searches[query] = nil
        // A cancelled or failed search is tried again next time; "nothing found" is remembered.
        if !Task.isCancelled { cache[query] = .some(found) }
        return found
    }

    /// Loads home once (a failed load is retried on the next call).
    func loadHome(api: SecretaryAPI) async {
        if home != nil { return }
        if let homeTask { return await homeTask.value }
        let task = Task { @MainActor in
            if let point = await api.mapsConfig()?.home {
                self.home = CLLocationCoordinate2D(latitude: point.lat, longitude: point.lng)
            }
        }
        homeTask = task
        await task.value
        homeTask = nil
    }

    /// 60 km around the current position, else home, else Denmark.
    private func biasRegion() -> MKCoordinateRegion {
        let center = PlaceLocation.shared.current?.coordinate ?? home
        guard let center else { return Self.denmark }
        return MKCoordinateRegion(center: center, latitudinalMeters: 120_000, longitudinalMeters: 120_000)
    }

    private static func search(_ query: String, region: MKCoordinateRegion) async -> Resolved? {
        let request = MKLocalSearch.Request()
        request.naturalLanguageQuery = query
        request.region = region
        request.resultTypes = [.address, .pointOfInterest]
        guard let item = try? await MKLocalSearch(request: request).start().mapItems.first else { return nil }
        let placemark = item.placemark
        let coordinate = placemark.coordinate
        guard Place.validCoordinate(coordinate.latitude, coordinate.longitude) != nil else { return nil }
        let street = [placemark.thoroughfare, placemark.subThoroughfare].compactMap { $0 }.joined(separator: " ")
        let address = [street.isEmpty ? nil : street, placemark.locality].compactMap { $0 }.joined(separator: ", ")
        return Resolved(lat: coordinate.latitude, lng: coordinate.longitude, name: item.name,
                        address: address.isEmpty ? nil : address)
    }
}

/// The when-in-use location for the maps: the permission and the latest fix. Never asks for
/// permission on its own (only `requestPermission`, from the full map screen).
@MainActor
@Observable
final class PlaceLocation {
    static let shared = PlaceLocation()

    private(set) var status: CLAuthorizationStatus = .notDetermined
    private(set) var current: CLLocation?

    @ObservationIgnored private let manager = CLLocationManager()
    @ObservationIgnored private let delegate = Delegate()
    /// Screens navigating now (`startUpdating`): the location follows the owner while above 0.
    @ObservationIgnored private var followers = 0

    init() {
        status = manager.authorizationStatus
        manager.desiredAccuracy = kCLLocationAccuracyHundredMeters
        manager.delegate = delegate
        delegate.onStatus = { [weak self] status in
            guard let self else { return }
            self.status = status
            if self.isAuthorized { self.refresh() }
            if self.isAuthorized && self.followers > 0 { self.startFollowing() }
        }
        delegate.onLocation = { [weak self] location in self?.current = location }
        if isAuthorized { current = manager.location }
    }

    var isAuthorized: Bool { status == .authorizedWhenInUse || status == .authorizedAlways }

    /// Asks for when-in-use permission when it was never asked.
    func requestPermission() {
        if manager.authorizationStatus == .notDetermined { manager.requestWhenInUseAuthorization() }
    }

    /// One fresh fix when permitted (the last known one is used meanwhile).
    func refresh() {
        guard isAuthorized else { return }
        if current == nil { current = manager.location }
        manager.requestLocation()
    }

    /// Continuous, precise fixes for the in-app navigation, until the matching `stopUpdating`.
    func startUpdating() {
        followers += 1
        if isAuthorized { startFollowing() }
    }

    func stopUpdating() {
        followers = max(0, followers - 1)
        guard followers == 0 else { return }
        manager.stopUpdatingLocation()
        manager.desiredAccuracy = kCLLocationAccuracyHundredMeters
        manager.distanceFilter = kCLDistanceFilterNone
    }

    private func startFollowing() {
        manager.desiredAccuracy = kCLLocationAccuracyBestForNavigation
        manager.distanceFilter = 5
        manager.startUpdatingLocation()
    }

    private final class Delegate: NSObject, CLLocationManagerDelegate {
        var onStatus: ((CLAuthorizationStatus) -> Void)?
        var onLocation: ((CLLocation) -> Void)?

        func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
            let status = manager.authorizationStatus
            MainActor.assumeIsolated { onStatus?(status) }
        }

        func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
            guard let last = locations.last else { return }
            MainActor.assumeIsolated { onLocation?(last) }
        }

        func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {}
    }
}

/// Where distances and travel times are measured from: the owner's position, else home.
struct PlaceOrigin: Equatable, Sendable {
    let lat: Double
    let lng: Double
    let isHome: Bool

    var coordinate: CLLocationCoordinate2D { CLLocationCoordinate2D(latitude: lat, longitude: lng) }

    @MainActor static func current(home: CLLocationCoordinate2D?) -> PlaceOrigin? {
        let location = PlaceLocation.shared
        if location.isAuthorized, let here = location.current?.coordinate {
            return PlaceOrigin(lat: here.latitude, lng: here.longitude, isHome: false)
        }
        if let home { return PlaceOrigin(lat: home.latitude, lng: home.longitude, isHome: true) }
        return nil
    }

    /// Rounded to ~100 m, so a jittering fix does not restart the travel times.
    var key: String { String(format: "%.3f,%.3f,%d", lat, lng, isHome ? 1 : 0) }

    func meters(to coordinate: CLLocationCoordinate2D) -> Double {
        CLLocation(latitude: lat, longitude: lng).distance(from: CLLocation(latitude: coordinate.latitude, longitude: coordinate.longitude))
    }

    /// "2.3 km from you" / "560 m from home".
    func distanceText(to coordinate: CLLocationCoordinate2D) -> String {
        let distance = PlaceFormat.distance(meters(to: coordinate))
        return isHome ? String(localized: "\(distance) from home") : String(localized: "\(distance) from you")
    }
}

enum PlaceFormat {
    static func distance(_ meters: Double) -> String {
        DealsFormat.distance(meters) ?? "\(Int(meters.rounded())) m"
    }

    /// "9 min", "1 hr 5 min".
    static func duration(_ seconds: TimeInterval) -> String {
        Duration.seconds(max(60, seconds)).formatted(.units(allowed: [.hours, .minutes], width: .abbreviated))
    }
}

/// One travel time from MKDirections.
struct PlaceETA: Equatable, Identifiable, Sendable {
    enum Mode: String, CaseIterable, Sendable {
        case automobile, walking, transit

        var symbol: String {
            switch self {
            case .automobile: return "car.fill"
            case .walking: return "figure.walk"
            case .transit: return "tram.fill"
            }
        }

        var title: String {
            switch self {
            case .automobile: return String(localized: "By car")
            case .walking: return String(localized: "On foot")
            case .transit: return String(localized: "By public transport")
            }
        }

        var transportType: MKDirectionsTransportType {
            switch self {
            case .automobile: return .automobile
            case .walking: return .walking
            case .transit: return .transit
            }
        }
    }

    let mode: Mode
    let seconds: TimeInterval
    var id: Mode { mode }

    /// The modes that have a route, in car / walk / transit order (empty when none does).
    static func all(from origin: CLLocationCoordinate2D, to destination: CLLocationCoordinate2D) async -> [PlaceETA] {
        async let car = eta(.automobile, from: origin, to: destination)
        async let walk = eta(.walking, from: origin, to: destination)
        async let transit = eta(.transit, from: origin, to: destination)
        return [await car, await walk, await transit].compactMap { $0 }
    }

    private static func eta(_ mode: Mode, from origin: CLLocationCoordinate2D, to destination: CLLocationCoordinate2D) async -> PlaceETA? {
        let request = MKDirections.Request()
        request.source = MKMapItem(placemark: MKPlacemark(coordinate: origin))
        request.destination = MKMapItem(placemark: MKPlacemark(coordinate: destination))
        request.transportType = mode.transportType
        guard let response = try? await MKDirections(request: request).calculateETA() else { return nil }
        return PlaceETA(mode: mode, seconds: response.expectedTravelTime)
    }

    /// "9 min · 31 min · 14 min" with the modes spelled out (the self-test and VoiceOver).
    static func text(_ etas: [PlaceETA]) -> String {
        etas.map { "\($0.mode.title) \(PlaceFormat.duration($0.seconds))" }.joined(separator: " · ")
    }
}
