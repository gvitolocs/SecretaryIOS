import CoreLocation
import MapKit
import SwiftUI
import UIKit

/// Google Maps and Apple Maps, only from explicit actions ("Open in Google Maps", "Open in Apple
/// Maps"). Every default action (the Navigate button, place and event cards, deal stores, widgets,
/// `secretary://` links, chat buttons and maps links, Shortcuts, the self-tests) opens Lucina's own
/// map instead (`PlaceLinks`, `PlaceMapView`). InAppMapsTests fails when any other file builds a
/// maps URL.
@MainActor
enum ExternalMaps {
    /// Apple Maps with directions to the coordinate in the mode.
    static func openAppleDirections(to coordinate: CLLocationCoordinate2D, name: String, mode: PlaceTravelMode) {
        #if SCREENSHOTS
        MapSelfTestProbe.shared.externalOpens += 1
        #endif
        DebugEvents.record("place.open_external", ["maps": "apple", "mode": mode.rawValue])
        let directions: String
        switch mode {
        case .bike: directions = MKLaunchOptionsDirectionsModeCycling
        case .drive: directions = MKLaunchOptionsDirectionsModeDriving
        case .walk: directions = MKLaunchOptionsDirectionsModeWalking
        case .transit: directions = MKLaunchOptionsDirectionsModeTransit
        }
        let item = MKMapItem(placemark: MKPlacemark(coordinate: coordinate))
        item.name = name
        item.openInMaps(launchOptions: [MKLaunchOptionsDirectionsModeKey: directions])
    }

    /// Google Maps with directions to the coordinate in the mode: its app when installed, else its website.
    static func openGoogleDirections(to coordinate: CLLocationCoordinate2D, mode: PlaceTravelMode) {
        let hasApp = URL(string: "comgooglemaps://").map(UIApplication.shared.canOpenURL) ?? false
        DebugEvents.record("place.open_external", ["maps": "google", "mode": mode.rawValue, "app": hasApp])
        open(googleMapsURL(to: coordinate, mode: mode, hasApp: hasApp))
    }

    /// Google's URLs need no API key. The app takes `directionsmode`, the website `travelmode`.
    nonisolated static func googleMapsURL(to coordinate: CLLocationCoordinate2D, mode: PlaceTravelMode, hasApp: Bool) -> URL? {
        let destination = "\(coordinate.latitude),\(coordinate.longitude)"
        let travel: String
        switch mode {
        case .bike: travel = "bicycling"
        case .drive: travel = "driving"
        case .walk: travel = "walking"
        case .transit: travel = "transit"
        }
        var components = URLComponents()
        if hasApp {
            components.scheme = "comgooglemaps"
            components.host = ""
            components.queryItems = [URLQueryItem(name: "daddr", value: destination), URLQueryItem(name: "directionsmode", value: travel)]
        } else {
            components.scheme = "https"
            components.host = "www.google.com"
            components.path = "/maps/dir/"
            components.queryItems = [URLQueryItem(name: "api", value: "1"), URLQueryItem(name: "destination", value: destination),
                                     URLQueryItem(name: "travelmode", value: travel)]
        }
        return components.url
    }

    /// Google Maps searching for the text (a place the app's map could not find); opens its app when installed.
    nonisolated static func googleMapsSearchURL(_ query: String) -> URL? {
        var components = URLComponents()
        components.scheme = "https"
        components.host = "www.google.com"
        components.path = "/maps/search/"
        components.queryItems = [URLQueryItem(name: "api", value: "1"), URLQueryItem(name: "query", value: query)]
        return components.url
    }

    /// Apple Maps searching for the text (a place the app's map could not find).
    nonisolated static func appleMapsSearchURL(_ query: String) -> URL? {
        var components = URLComponents()
        components.scheme = "https"
        components.host = "maps.apple.com"
        components.path = "/"
        components.queryItems = [URLQueryItem(name: "q", value: query)]
        return components.url
    }

    /// A URL that would leave the app for a maps app (Apple Maps, Google Maps, either's website).
    nonisolated static func isExternal(_ url: URL) -> Bool {
        let scheme = url.scheme?.lowercased() ?? ""
        let host = url.host?.lowercased() ?? ""
        if ["maps", "comgooglemaps", "comgooglemaps-x-callback", "googlemaps"].contains(scheme) { return true }
        if host == "maps.apple.com" || host == "maps.apple" || host.hasPrefix("maps.google.") { return true }
        return host.contains("google.") && url.path.hasPrefix("/maps")
    }

    /// Opens another maps app. `UIApplication.open`, not the environment's `openURL`: the app routes
    /// maps links to its own map (RootView), and this one is the owner's explicit choice.
    static func open(_ url: URL?) {
        guard let url else { return }
        #if SCREENSHOTS
        MapSelfTestProbe.shared.externalOpens += 1
        #endif
        UIApplication.shared.open(url)
    }
}

/// "Open in Apple Maps": the secondary, explicit way out of the app's map (inside a Menu).
struct OpenInAppleMapsButton: View {
    private let enabled: Bool
    private let action: @MainActor () -> Void

    /// Opens the URL (a search for a place the app's map could not find).
    init(url: URL?) {
        enabled = url != nil
        action = { ExternalMaps.open(url) }
    }

    /// Opens directions to the coordinate in the mode.
    init(to coordinate: CLLocationCoordinate2D, name: String, mode: PlaceTravelMode) {
        enabled = true
        action = { ExternalMaps.openAppleDirections(to: coordinate, name: name, mode: mode) }
    }

    var body: some View {
        Button(action: action) {
            Label("Open in Apple Maps", systemImage: "map")
        }
        .disabled(!enabled)
    }
}

/// "Open in Google Maps", inside a Menu.
struct OpenInGoogleMapsButton: View {
    private let enabled: Bool
    private let action: @MainActor () -> Void

    /// Opens the URL (a search for a place the app's map could not find).
    init(url: URL?) {
        enabled = url != nil
        action = { ExternalMaps.open(url) }
    }

    /// Opens directions to the coordinate in the mode.
    init(to coordinate: CLLocationCoordinate2D, mode: PlaceTravelMode) {
        enabled = true
        action = { ExternalMaps.openGoogleDirections(to: coordinate, mode: mode) }
    }

    var body: some View {
        Button(action: action) {
            Label("Open in Google Maps", systemImage: "map.fill")
        }
        .disabled(!enabled)
    }
}
