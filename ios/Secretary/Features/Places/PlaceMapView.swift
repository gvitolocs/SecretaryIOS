import MapKit
import SwiftUI

/// The full map of a place: its pin and the owner's position (both in view when the owner is within
/// ~50 km), and a card with the address, time, distance and travel times from the current position
/// (else from home), and Directions in Apple Maps or Google Maps.
struct PlaceMapView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.openURL) private var openURL
    let place: Place
    let coordinate: CLLocationCoordinate2D
    /// The address MKLocalSearch found (shown when the place has none).
    var address: String?
    @State private var etas: [PlaceETA] = []
    @State private var etaLoading = false

    private var origin: PlaceOrigin? { PlaceOrigin.current(home: PlaceResolver.shared.home) }

    /// The owner's position when it is close enough to show both on the map.
    private var nearbyUser: CLLocationCoordinate2D? {
        guard let origin, !origin.isHome, origin.meters(to: coordinate) <= 50_000 else { return nil }
        return origin.coordinate
    }

    private var pin: MapPin {
        MapPin(id: "place", coordinate: coordinate, title: place.title, subtitle: address ?? place.address, tint: .accent)
    }

    var body: some View {
        ZStack(alignment: .bottom) {
            LucinaMap(pins: [pin], showsUserLocation: true, alsoFit: nearbyUser, onCameraChange: cameraMoved)
                .ignoresSafeArea(edges: .bottom)
                .accessibilityIdentifier("placeMap")
            info
                .padding(.horizontal, LucinaTokens.Space.gutter)
                .padding(.bottom, LucinaTokens.Space.sm)
        }
        .background(Theme.page.ignoresSafeArea())
        .navigationTitle(place.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .task {
            PlaceLocation.shared.requestPermission()
            PlaceLocation.shared.refresh()
            await PlaceResolver.shared.loadHome(api: app.api)
        }
        .task(id: origin?.key) { await loadETAs() }
        #if SCREENSHOTS
        .onAppear {
            MapSelfTestProbe.shared.mapShown = true
            reportFit()
        }
        .onChange(of: nearbyUser != nil) { _, _ in reportFit() }
        #endif
    }

    private var info: some View {
        Card(spacing: 8) {
            VStack(alignment: .leading, spacing: 3) {
                Text(place.title).font(LucinaTokens.Fonts.headline)
                if let address = place.address ?? address {
                    Text(address).font(LucinaTokens.Fonts.subheadline).foregroundStyle(Theme.textSecondary)
                }
                if let time = place.timeText {
                    Label(time, systemImage: "clock")
                        .font(LucinaTokens.Fonts.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
            }
            if let origin {
                Label(origin.distanceText(to: coordinate), systemImage: origin.isHome ? "house" : "location.fill")
                    .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                    .accessibilityIdentifier("placeDistance")
                travelTimes
                    .accessibilityIdentifier("placeETA")
            }
            directions
        }
    }

    @ViewBuilder private var travelTimes: some View {
        if !etas.isEmpty {
            HStack(spacing: 6) {
                ForEach(Array(etas.enumerated()), id: \.element.id) { index, eta in
                    if index > 0 { Text(verbatim: "·").foregroundStyle(Theme.textTertiary) }
                    Label(PlaceFormat.duration(eta.seconds), systemImage: eta.mode.symbol)
                        .labelStyle(.titleAndIcon)
                        .accessibilityLabel(Text(verbatim: "\(eta.mode.title) \(PlaceFormat.duration(eta.seconds))"))
                }
            }
            .font(LucinaTokens.Fonts.subheadline.monospacedDigit())
            .foregroundStyle(Theme.textSecondary)
            .lineLimit(1)
            .minimumScaleFactor(0.8)
            .accessibilityElement(children: .combine)
        } else if etaLoading {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("Travel times…")
            }
            .font(LucinaTokens.Fonts.subheadline)
            .foregroundStyle(Theme.textSecondary)
        } else {
            Text("No route found")
                .font(LucinaTokens.Fonts.subheadline)
                .foregroundStyle(Theme.textSecondary)
        }
    }

    private var directions: some View {
        Menu {
            Button {
                PlaceDirections.appleMaps(to: coordinate, name: place.title)
            } label: {
                Label("Apple Maps", systemImage: "map")
            }
            Button {
                if let url = PlaceDirections.googleMapsURL(to: coordinate) { openURL(url) }
            } label: {
                Label("Google Maps", systemImage: "globe")
            }
        } label: {
            Label("Directions", systemImage: "arrow.triangle.turn.up.right.diamond.fill")
                .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                .foregroundStyle(Theme.onAccent)
                .frame(maxWidth: .infinity)
                .frame(height: 44)
                .background(Theme.accentFill, in: Capsule())
        }
        .accessibilityIdentifier("placeDirections")
    }

    private func loadETAs() async {
        guard let origin else {
            etas = []
            return
        }
        etaLoading = true
        let found = await PlaceETA.all(from: origin.coordinate, to: coordinate)
        guard !Task.isCancelled else { return }
        etas = found
        etaLoading = false
        #if SCREENSHOTS
        MapSelfTestProbe.shared.distanceText = origin.distanceText(to: coordinate)
        MapSelfTestProbe.shared.etaText = PlaceETA.text(found)
        MapSelfTestProbe.shared.etaDone = true
        #endif
    }

    /// The camera after a move: only the map self-test reads it.
    private func cameraMoved(_ center: CLLocationCoordinate2D) {
        #if SCREENSHOTS
        MapSelfTestProbe.shared.camera = center
        reportFit()
        #endif
    }

    #if SCREENSHOTS
    private func reportFit() {
        MapSelfTestProbe.shared.expected = LucinaMap.fitCenter([pin], also: nearbyUser)
        MapSelfTestProbe.shared.fitsUser = nearbyUser != nil
    }
    #endif
}
