import CoreLocation
import MapKit
import SwiftUI
import UIKit

/// Apple Maps, only from an explicit secondary menu item ("Open in Apple Maps"). Every default
/// action (the Navigate button, place and event cards, deal stores, widgets, `secretary://` links,
/// chat buttons and maps links, Shortcuts, the self-tests) opens Lucina's own map instead
/// (`PlaceLinks`, `PlaceMapView`). InAppMapsTests fails when any other file builds a maps URL.
@MainActor
enum ExternalMaps {
    /// Apple Maps with bike directions to the coordinate.
    static func openBikeDirections(to coordinate: CLLocationCoordinate2D, name: String) {
        #if SCREENSHOTS
        MapSelfTestProbe.shared.externalOpens += 1
        #endif
        let item = MKMapItem(placemark: MKPlacemark(coordinate: coordinate))
        item.name = name
        item.openInMaps(launchOptions: [MKLaunchOptionsDirectionsModeKey: MKLaunchOptionsDirectionsModeCycling])
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

    /// Opens Apple Maps. `UIApplication.open`, not the environment's `openURL`: the app routes
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

    /// Opens bike directions to the coordinate.
    init(bikeTo coordinate: CLLocationCoordinate2D, name: String) {
        enabled = true
        action = { ExternalMaps.openBikeDirections(to: coordinate, name: name) }
    }

    var body: some View {
        Button(action: action) {
            Label("Open in Apple Maps", systemImage: "map")
        }
        .disabled(!enabled)
    }
}
