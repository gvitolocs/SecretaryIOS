import MapKit
import SwiftUI

/// The full map of a place: its pin, the owner's position (both in view when the owner is within
/// ~50 km) and the one route, by bike (owner's choice, he lives in Denmark), and a card with the
/// address, time, distance and the bike time from the current position (else from home). Navigate
/// stays in the app: the camera follows the owner along the bike route with the next turn; Apple
/// Maps is only a secondary menu item.
struct PlaceMapView: View {
    @Environment(AppModel.self) private var app
    let place: Place
    let coordinate: CLLocationCoordinate2D
    /// The address MKLocalSearch found (shown when the place has none).
    var address: String?
    /// Opened by a Navigate action (`/map/place?…&navigate=1`, a deal store, a Shortcut).
    var startsNavigating = false
    @State private var navigating = false
    @State private var startedOnce = false
    /// The bike route, from where the owner is (else from home).
    @State private var route: BikeRoute?
    /// When the route was last loaded (its duration counts from then).
    @State private var routeAt = Date()
    @State private var routeLoading = false
    /// An event's place: pinned to the countdown widget.
    @State private var countdownPinned = false

    private var polyline: [[Double]] { route?.line ?? [] }

    /// When to leave for an appointment still ahead.
    private var leaveAt: Date? {
        guard let route, let when = place.when, when > Date() else { return nil }
        return when.addingTimeInterval(-route.seconds)
    }

    /// The route reloads when the owner moves ~100 m, every ~1 km while navigating (fewer calls).
    private var routesKey: String? { navigating ? origin.map { String(format: "nav %.2f,%.2f", $0.lat, $0.lng) } : origin?.key }

    /// The owner's live position while navigating.
    private var here: CLLocationCoordinate2D? {
        PlaceLocation.shared.isAuthorized ? PlaceLocation.shared.current?.coordinate : nil
    }

    private var arrived: Bool {
        guard navigating, let here else { return false }
        return NavigationGuide.meters(here, coordinate) <= NavigationGuide.arrivalMeters
    }

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
        // The card is a bottom safe-area inset, so the camera fits the place (and the owner) above it.
        LucinaMap(pins: [pin], showsUserLocation: true, polyline: polyline, alsoFit: nearbyUser,
                  followsUser: navigating, onCameraChange: cameraMoved)
            .accessibilityIdentifier("placeMap")
            .safeAreaInset(edge: .bottom, spacing: 0) {
                ScrollView {
                    if navigating { navigationCard } else { info }
                }
                .frame(maxHeight: 360)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, LucinaTokens.Space.gutter)
                .padding(.bottom, LucinaTokens.Space.sm)
            }
        .background(Theme.page.ignoresSafeArea())
        .navigationTitle(place.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                if let event = place.countdownEvent {
                    Button {
                        countdownPinned.toggle()
                        Countdown.setPinned(event, countdownPinned)
                    } label: {
                        Image(systemName: countdownPinned ? "pin.fill" : "pin")
                    }
                    .accessibilityLabel(countdownPinned ? Text("Unpin from countdown") : Text("Pin to countdown"))
                    .accessibilityIdentifier("placeCountdownPin")
                }
            }
        }
        .task {
            countdownPinned = place.countdownEvent.map { Countdown.isPinned($0.id) } ?? false
            PlaceLocation.shared.requestPermission()
            PlaceLocation.shared.refresh()
            await PlaceResolver.shared.loadHome(api: app.api)
        }
        .task(id: routesKey) { await loadRoute() }
        .onAppear {
            if startsNavigating && !startedOnce { startNavigation() }
            startedOnce = true
        }
        .onDisappear { stopNavigation() }
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
                bikeTime
                    .accessibilityIdentifier("placeETA")
            }
            directions
        }
    }

    /// The one time on the card: the bike's. An estimate says so.
    @ViewBuilder private var bikeTime: some View {
        if let route {
            let duration = PlaceFormat.duration(route.seconds)
            Label {
                if route.estimated {
                    Text("About \(duration) by bike (estimate)")
                } else {
                    Text(verbatim: duration)
                }
            } icon: {
                Image(systemName: PlaceTravelMode.bike.symbol)
            }
            .font(LucinaTokens.Fonts.subheadline.monospacedDigit())
            .foregroundStyle(Theme.textSecondary)
            .accessibilityElement(children: .combine)
            .accessibilityLabel(Text(verbatim: "\(route.modeText) \(duration)"))
            if let leaveAt {
                let time = leaveAt.formatted(date: .omitted, time: .shortened)
                Text("Leave at \(time) to be on time")
                    .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.accent)
            }
        } else if routeLoading {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("Travel times…")
            }
            .font(LucinaTokens.Fonts.subheadline)
            .foregroundStyle(Theme.textSecondary)
        } else {
            Text("No bike route found")
                .font(LucinaTokens.Fonts.subheadline)
                .foregroundStyle(Theme.textSecondary)
        }
    }

    /// Navigate by bike (in the app) and, in a secondary menu, Apple Maps.
    private var directions: some View {
        HStack(spacing: 8) {
            Button(action: startNavigation) {
                Label("Navigate", systemImage: PlaceTravelMode.bike.symbol)
                    .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.onAccent)
                    .frame(maxWidth: .infinity)
                    .frame(height: 44)
                    .background(Theme.accentFill, in: Capsule())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("placeNavigate")
            moreMenu
        }
    }

    private var moreMenu: some View {
        Menu {
            OpenInAppleMapsButton(bikeTo: coordinate, name: place.title)
        } label: {
            Image(systemName: "ellipsis")
                .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                .foregroundStyle(Theme.text)
                .frame(width: 44, height: 44)
                .background(Theme.field, in: Circle())
        }
        .accessibilityLabel(Text("More"))
        .accessibilityIdentifier("placeMore")
    }

    // MARK: Navigation

    /// While navigating: the next turn and how far, the arrival time, the distance left, End.
    private var navigationCard: some View {
        Card(spacing: 8) {
            if arrived {
                Label("You have arrived", systemImage: "flag.checkered")
                    .font(LucinaTokens.Fonts.headline)
                    .foregroundStyle(Theme.accent)
            } else if let here, let next = NavigationGuide.next(route?.steps ?? [], at: here) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(verbatim: next.step.instruction)
                        .font(LucinaTokens.Fonts.headline)
                        .lineLimit(3)
                    Spacer(minLength: 8)
                    Text(verbatim: PlaceFormat.distance(next.meters))
                        .font(LucinaTokens.Fonts.headline.monospacedDigit())
                        .foregroundStyle(Theme.accent)
                }
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("placeNextTurn")
            } else {
                Label(place.title, systemImage: "mappin.and.ellipse")
                    .font(LucinaTokens.Fonts.headline)
            }
            if !arrived {
                HStack(spacing: 6) {
                    Image(systemName: PlaceTravelMode.bike.symbol)
                    if let route {
                        let time = routeAt.addingTimeInterval(route.seconds).formatted(date: .omitted, time: .shortened)
                        Text("Arrive at \(time)")
                    }
                    if let here {
                        if route != nil { Text(verbatim: "·").foregroundStyle(Theme.textTertiary) }
                        let distance = PlaceFormat.distance(NavigationGuide.meters(here, coordinate))
                        Text("\(distance) to go")
                    }
                }
                .font(LucinaTokens.Fonts.subheadline.monospacedDigit())
                .foregroundStyle(Theme.textSecondary)
                .lineLimit(1)
                .minimumScaleFactor(0.8)
                if here == nil {
                    Group {
                        if PlaceLocation.shared.isAuthorized {
                            Text("Finding your position…")
                        } else {
                            Text("Allow location access to navigate.")
                        }
                    }
                    .font(LucinaTokens.Fonts.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                }
            }
            HStack(spacing: 8) {
                Button(action: stopNavigation) {
                    Text("End")
                        .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.onAccent)
                        .frame(maxWidth: .infinity)
                        .frame(height: 44)
                        .background(Theme.accentFill, in: Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("placeEndNavigation")
                moreMenu
            }
        }
    }

    private func startNavigation() {
        guard !navigating else { return }
        DebugEvents.record("place.navigate", ["mode": PlaceTravelMode.bike.rawValue, "via": route?.source.rawValue ?? "none"])
        PlaceLocation.shared.requestPermission()
        PlaceLocation.shared.startUpdating()
        UIApplication.shared.isIdleTimerDisabled = true
        navigating = true
        #if SCREENSHOTS
        MapSelfTestProbe.shared.navigating = true
        #endif
    }

    private func stopNavigation() {
        guard navigating else { return }
        PlaceLocation.shared.stopUpdating()
        UIApplication.shared.isIdleTimerDisabled = false
        navigating = false
        #if SCREENSHOTS
        MapSelfTestProbe.shared.navigating = false
        #endif
    }

    private func loadRoute() async {
        // While navigating the old route stays up until the new one arrives.
        if !navigating { route = nil }
        guard let origin else { return }
        routeLoading = true
        #if SCREENSHOTS
        MapSelfTestProbe.shared.etaDone = false
        MapSelfTestProbe.shared.distanceText = origin.distanceText(to: coordinate)
        MapSelfTestProbe.shared.routesText = nil
        MapSelfTestProbe.shared.fallbackReason = nil
        MapSelfTestProbe.shared.leaveAt = nil
        #endif
        let found = await BikeRoute.calculate(from: origin.coordinate, to: coordinate)
        guard !Task.isCancelled else { return }
        if found != nil || !navigating {
            route = found
            routeAt = Date()
        }
        routeLoading = false
        // Free of coordinates and addresses, like every debug event.
        DebugEvents.record("place.route", ["mode": PlaceTravelMode.bike.rawValue, "via": found?.source.rawValue ?? "none",
                                           "seconds": Int(found?.seconds ?? 0), "meters": Int(found?.meters ?? 0),
                                           "steps": found?.steps.count ?? 0, "navigating": navigating, "from_home": origin.isHome])
        #if SCREENSHOTS
        if let found {
            MapSelfTestProbe.shared.routesText = PlaceTravelMode.bike.rawValue
            MapSelfTestProbe.shared.etaText = "\(found.modeText) \(PlaceFormat.duration(found.seconds))"
            if found.estimated { MapSelfTestProbe.shared.fallbackReason = found.source.rawValue }
        }
        MapSelfTestProbe.shared.leaveAt = leaveAt
        if navigating { MapSelfTestProbe.shared.navigationSteps = found?.steps.count ?? 0 }
        reportFit()
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
        MapSelfTestProbe.shared.expected = LucinaMap.fitCenter([pin], also: nearbyUser, polyline: polyline)
        MapSelfTestProbe.shared.fitsUser = nearbyUser != nil
    }
    #endif
}

/// Shared presentation for rich route actions, widgets and notification taps.
struct OpenPlaceHost: ViewModifier {
    @Environment(AppModel.self) private var app

    func body(content: Content) -> some View {
        content.background {
            Color.clear.sheet(isPresented: Binding(get: { app.openPlace != nil }, set: { if !$0 { app.openPlace = nil } })) {
                if let place = app.openPlace {
                    NavigationStack {
                        RoutedPlaceView(place: place, navigate: app.openPlaceNavigates)
                            .toolbar {
                                ToolbarItem(placement: .confirmationAction) {
                                    Button("Done") { app.openPlace = nil }
                                }
                            }
                    }
                    .id(place)
                    .lucinaSheet()
                }
            }
        }
    }
}

private struct RoutedPlaceView: View {
    @Environment(AppModel.self) private var app
    let place: Place
    var navigate = false
    @State private var resolved: PlaceResolver.Resolved?
    @State private var loading = true

    var body: some View {
        Group {
            if let resolved {
                PlaceMapView(place: place, coordinate: resolved.coordinate, address: resolved.address, startsNavigating: navigate)
            } else if loading {
                ProgressView()
            } else {
                PlaceNotFound(place: place)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.page.ignoresSafeArea())
        .navigationTitle(place.title)
        .navigationBarTitleDisplayMode(.inline)
        .task(id: place) {
            loading = true
            let result = await PlaceResolver.shared.resolve(place, api: app.api)
            guard !Task.isCancelled else { return }
            resolved = result
            loading = false
        }
    }
}

/// A place the app's map could not find: says so; Apple Maps only from the secondary menu.
struct PlaceNotFound: View {
    let place: Place

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Not found on the map", systemImage: "mappin.slash")
                .font(LucinaTokens.Fonts.subheadline)
                .foregroundStyle(Theme.textSecondary)
            Menu {
                OpenInAppleMapsButton(url: ExternalMaps.appleMapsSearchURL(place.searchQuery))
            } label: {
                Label("More", systemImage: "ellipsis.circle")
                    .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
            }
            .accessibilityIdentifier("placeNotFoundMore")
        }
    }
}
