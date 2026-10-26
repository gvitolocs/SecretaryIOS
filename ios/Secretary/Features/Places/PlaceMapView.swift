import MapKit
import SwiftUI

/// The full map of a place: its pin and the owner's position (both in view when the owner is within
/// ~50 km), and a card with the address, time, distance and travel times from the current position
/// (else from home). Navigate stays in the app: the camera follows the owner along the selected
/// route with the next turn; Apple Maps is only a secondary menu item.
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
    /// MapKit's turns for the selected mode while navigating.
    @State private var steps: [NavigationStep] = []
    /// When the routes were last loaded (their durations count from then).
    @State private var routesAt = Date()
    @State private var etas: [PlaceETA] = []
    @State private var etaLoading = false
    @State private var routes: [MapsRoute] = []
    @State private var selectedMode: PlaceTravelMode?
    @AppStorage("placeTravelMode") private var rememberedMode = ""
    /// An event's place: pinned to the countdown widget.
    @State private var countdownPinned = false

    private var selectedRoute: MapsRoute? { routes.first { $0.mode == selectedMode } }
    private var polyline: [[Double]] {
        if let line = selectedRoute?.polyline, !line.isEmpty { return line }
        return navigating ? steps.flatMap(\.line) : []
    }

    /// The mode the navigation guides in: the selected route's, else by distance.
    private var navigationMode: PlaceTravelMode {
        if let selectedMode { return selectedMode }
        return (origin?.meters(to: coordinate) ?? 0) > 2000 ? .drive : .walk
    }

    /// The routes reload when the owner moves ~100 m, every ~1 km while navigating (fewer calls).
    private var routesKey: String? { navigating ? origin.map { String(format: "nav %.2f,%.2f", $0.lat, $0.lng) } : origin?.key }

    private var stepsKey: String { "\(navigating) \(navigationMode.rawValue) \(routesKey ?? "-")" }

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
        .task(id: routesKey) { await loadETAs() }
        .task(id: stepsKey) { await loadSteps() }
        .onAppear {
            if startsNavigating && !startedOnce { startNavigation() }
            startedOnce = true
        }
        .onDisappear { stopNavigation() }
        .onChange(of: selectedMode) { _, _ in
            #if SCREENSHOTS
            reportFit()
            MapSelfTestProbe.shared.leaveAt = selectedRoute?.leaveAt
            #endif
        }
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
        if !routes.isEmpty {
            routePicker
            if let route = selectedRoute {
                if place.when != nil, let leaveAt = route.leaveAt {
                    let time = leaveAt.formatted(date: .omitted, time: .shortened)
                    Text("Leave at \(time) to be on time")
                        .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.accent)
                }
                if !route.transit.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(route.transit.enumerated()), id: \.offset) { _, leg in
                            transitLeg(leg)
                        }
                    }
                }
            }
        } else if !etas.isEmpty {
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

    private var routePicker: some View {
        HStack(alignment: .top, spacing: 4) {
            ForEach(PlaceTravelMode.allCases, id: \.self) { mode in
                if let route = routes.first(where: { $0.mode == mode }) {
                    Button {
                        rememberedMode = mode.rawValue
                        selectedMode = mode
                    } label: {
                        VStack(spacing: 4) {
                            Image(systemName: mode.symbol)
                            Text(verbatim: route.durationText)
                                .font(LucinaTokens.Fonts.caption2.monospacedDigit())
                                .multilineTextAlignment(.center)
                        }
                        .frame(maxWidth: .infinity, minHeight: 44)
                        .padding(6)
                        .foregroundStyle(selectedMode == mode ? Theme.onAccent : Theme.textSecondary)
                        .background(selectedMode == mode ? Theme.accentFill : Theme.field,
                                    in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field))
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel(Text(verbatim: "\(mode.title) \(route.durationText)"))
                    .accessibilityAddTraits(selectedMode == mode ? .isSelected : [])
                }
            }
        }
    }

    private func transitLeg(_ leg: MapsRoute.TransitLeg) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Label([leg.line, leg.vehicle].compactMap { $0 }.joined(separator: " · "), systemImage: "tram.fill")
                .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
            Text(verbatim: [leg.from, leg.to].compactMap { $0 }.joined(separator: " → "))
                .font(LucinaTokens.Fonts.caption)
            if let depart = leg.departAt {
                Text(verbatim: [depart, leg.arriveAt].compactMap { $0 }
                    .map { $0.formatted(date: .omitted, time: .shortened) }.joined(separator: " – "))
                    .font(LucinaTokens.Fonts.caption.monospacedDigit())
            }
        }
        .foregroundStyle(Theme.textSecondary)
    }

    /// Navigate (in the app) and, in a secondary menu, Apple Maps.
    private var directions: some View {
        HStack(spacing: 8) {
            Button(action: startNavigation) {
                Label("Navigate", systemImage: "location.north.line.fill")
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
            OpenInAppleMapsButton(url: ExternalMaps.appleMapsURL(to: coordinate, name: place.title))
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
            } else if let here, let next = NavigationGuide.next(steps, at: here) {
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
                    Image(systemName: navigationMode.symbol)
                    if let route = selectedRoute {
                        let time = routesAt.addingTimeInterval(route.durationS).formatted(date: .omitted, time: .shortened)
                        Text("Arrive at \(time)")
                    }
                    if let here {
                        if selectedRoute != nil { Text(verbatim: "·").foregroundStyle(Theme.textTertiary) }
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
                if let route = selectedRoute, !route.transit.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(route.transit.enumerated()), id: \.offset) { _, leg in
                            transitLeg(leg)
                        }
                    }
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
        steps = []
        #if SCREENSHOTS
        MapSelfTestProbe.shared.navigating = false
        #endif
    }

    private func loadSteps() async {
        guard navigating, let origin, !origin.isHome else {
            if !navigating { steps = [] }
            return
        }
        let found = await NavigationGuide.steps(navigationMode, from: origin.coordinate, to: coordinate)
        guard !Task.isCancelled else { return }
        steps = found
        #if SCREENSHOTS
        MapSelfTestProbe.shared.navigationSteps = found.count
        #endif
    }

    private func loadETAs() async {
        // While navigating the old route stays up until the new one arrives.
        if !navigating {
            routes = []
            etas = []
        }
        guard let origin else { return }
        etaLoading = true
        #if SCREENSHOTS
        MapSelfTestProbe.shared.etaDone = false
        MapSelfTestProbe.shared.distanceText = origin.distanceText(to: coordinate)
        MapSelfTestProbe.shared.routesText = nil
        MapSelfTestProbe.shared.fallbackReason = nil
        MapSelfTestProbe.shared.leaveAt = nil
        #endif
        let arrive = place.when.flatMap { $0 > Date() ? $0 : nil }
        do {
            let response = try await app.api.mapsRoutes(from: origin.coordinate, to: coordinate, arrive: arrive)
            guard !Task.isCancelled else { return }
            routes = response.routes
            routesAt = Date()
            let preferred = selectedMode ?? PlaceTravelMode(rawValue: rememberedMode)
                ?? (origin.meters(to: coordinate) > 2000 ? .transit : .walk)
            selectedMode = routes.first(where: { $0.mode == preferred })?.mode ?? routes.first?.mode
            #if SCREENSHOTS
            MapSelfTestProbe.shared.routesText = routes.map { $0.mode.rawValue }.joined(separator: ",")
            MapSelfTestProbe.shared.leaveAt = selectedRoute?.leaveAt
            MapSelfTestProbe.shared.etaText = routes.map { "\($0.mode.title) \($0.durationText)" }.joined(separator: " · ")
            #endif
        } catch {
            guard !Task.isCancelled else { return }
            let found = await PlaceETA.all(from: origin.coordinate, to: coordinate)
            guard !Task.isCancelled else { return }
            etas = found
            #if SCREENSHOTS
            // Keep the diagnostic free of request URLs, coordinates and addresses.
            if let error = error as? APIError {
                MapSelfTestProbe.shared.fallbackReason = "HTTP \(error.status) \(error.code)"
            } else {
                MapSelfTestProbe.shared.fallbackReason = "endpoint unavailable"
            }
            MapSelfTestProbe.shared.etaText = PlaceETA.text(found)
            #endif
        }
        etaLoading = false
        #if SCREENSHOTS
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
