import MapKit
import SwiftUI

/// The full map of a place: its pin, the owner's position (both in view when the owner is within
/// ~50 km) and the route in the selected mode (by default the bike: he lives in Denmark), and a card
/// with the address, time, distance and a tappable travel time per mode from the current position
/// (else from home). Navigate stays in the app: the camera follows the owner along the route with
/// the next turn. Google Maps and Apple Maps are explicit actions, in the selected mode.
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
    /// The routes per mode, from where the owner is (else from home).
    @State private var routes: [PlaceTravelMode: PlaceRoute] = [:]
    /// Bike unless the owner taps another mode, for this screen only.
    @State private var selectedMode: PlaceTravelMode = .bike
    /// When the routes were last loaded (their durations count from then).
    @State private var routeAt = Date()
    @State private var routeLoading = false
    /// An event's place: pinned to the countdown widget.
    @State private var countdownPinned = false

    private var route: PlaceRoute? { routes[selectedMode] }
    private var polyline: [[Double]] { route?.line ?? [] }

    /// When to leave for an appointment still ahead.
    private var leaveAt: Date? {
        guard let route, let when = place.when, when > Date() else { return nil }
        return when.addingTimeInterval(-route.seconds)
    }

    /// The routes reload when the owner moves ~100 m, every ~1 km while navigating (fewer calls).
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
        .task(id: routesKey) { await loadRoutes() }
        .onAppear {
            if startsNavigating && !startedOnce { startNavigation() }
            startedOnce = true
        }
        .onDisappear { stopNavigation() }
        #if SCREENSHOTS
        .onChange(of: selectedMode) { _, _ in
            reportFit()
            MapSelfTestProbe.shared.leaveAt = leaveAt
        }
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

    /// A button per mode with its time, the selected one filled: tapping selects it, and the map line,
    /// Navigate and the other maps apps follow.
    private var modePicker: some View {
        HStack(spacing: 4) {
            ForEach(PlaceTravelMode.cardOrder, id: \.self) { mode in
                let selected = mode == selectedMode
                Button {
                    select(mode)
                } label: {
                    VStack(spacing: 4) {
                        Image(systemName: mode.symbol)
                        if let route = routes[mode] {
                            Text(verbatim: route.durationText)
                                .font(LucinaTokens.Fonts.caption.monospacedDigit().weight(selected ? .semibold : .regular))
                                .lineLimit(1)
                                .minimumScaleFactor(0.7)
                        } else if routeLoading {
                            ProgressView().controlSize(.mini)
                        } else {
                            Text(verbatim: "–").font(LucinaTokens.Fonts.caption)
                        }
                    }
                    .frame(maxWidth: .infinity, minHeight: 44)
                    .padding(6)
                    .foregroundStyle(selected ? Theme.onAccent : Theme.text)
                    .background(selected ? Theme.accentFill : Theme.field,
                                in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field))
                    .contentShape(RoundedRectangle(cornerRadius: LucinaTokens.Radius.field))
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text(verbatim: "\(routes[mode]?.modeText ?? mode.title) \(routes[mode]?.durationText ?? "")"))
                .accessibilityAddTraits(selected ? .isSelected : [])
                .accessibilityIdentifier("placeMode-\(mode.rawValue)")
            }
        }
    }

    /// The modes, then what the selected one needs said. A bike estimate says so; a walking or car
    /// time never stands in for it.
    @ViewBuilder private var travelTimes: some View {
        modePicker
        Group {
            if let route, route.estimated {
                Label(route.modeText, systemImage: route.mode.symbol)
            } else if route == nil, !routeLoading {
                selectedMode == .bike ? Text("No bike route found") : Text("No route found")
            } else if selectedMode == .transit {
                Text("Apple Maps can only give the time for public transport. Open in Google Maps to see the lines.")
            }
        }
        .font(LucinaTokens.Fonts.caption)
        .foregroundStyle(Theme.textSecondary)
        if let leaveAt {
            let time = leaveAt.formatted(date: .omitted, time: .shortened)
            Text("Leave at \(time) to be on time")
                .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                .foregroundStyle(Theme.accent)
        }
    }

    /// Navigate in the selected mode (in the app), Google Maps next to it, Apple Maps in the menu.
    private var directions: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Button(action: startNavigation) {
                    Label("Navigate", systemImage: selectedMode.symbol)
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
            Button {
                ExternalMaps.openGoogleDirections(to: coordinate, mode: selectedMode)
            } label: {
                Label("Open in Google Maps", systemImage: "map.fill")
                    .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.text)
                    .frame(maxWidth: .infinity)
                    .frame(height: 44)
                    .background(Theme.field, in: Capsule())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("placeOpenGoogle")
        }
    }

    private var moreMenu: some View {
        Menu {
            OpenInGoogleMapsButton(to: coordinate, mode: selectedMode)
            OpenInAppleMapsButton(to: coordinate, name: place.title, mode: selectedMode)
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
                    Image(systemName: selectedMode.symbol)
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
        DebugEvents.record("place.navigate", ["mode": selectedMode.rawValue, "via": route?.source.rawValue ?? "none"])
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

    private func select(_ mode: PlaceTravelMode) {
        selectedMode = mode
        DebugEvents.record("ui.tap", ["screen": "place_map", "element": "travel_mode", "mode": mode.rawValue,
                                      "via": routes[mode]?.source.rawValue ?? "none", "seconds": Int(routes[mode]?.seconds ?? 0),
                                      "steps": routes[mode]?.steps.count ?? 0])
    }

    private func loadRoutes() async {
        // While navigating the old route stays up until the new one arrives, and only that mode
        // reloads (fewer calls).
        if !navigating { routes = [:] }
        guard let origin else { return }
        routeLoading = true
        #if SCREENSHOTS
        MapSelfTestProbe.shared.etaDone = false
        MapSelfTestProbe.shared.distanceText = origin.distanceText(to: coordinate)
        MapSelfTestProbe.shared.routesText = nil
        MapSelfTestProbe.shared.fallbackReason = nil
        MapSelfTestProbe.shared.leaveAt = nil
        #endif
        let from = origin.coordinate, to = coordinate, isHome = origin.isHome
        await withTaskGroup(of: (PlaceTravelMode, PlaceRoute?).self) { group in
            for mode in navigating ? [selectedMode] : PlaceTravelMode.cardOrder {
                group.addTask { (mode, await PlaceRoute.calculate(mode, from: from, to: to)) }
            }
            for await (mode, found) in group {
                guard !Task.isCancelled else { return }
                if found != nil || !navigating { routes[mode] = found }
                // Free of coordinates and addresses, like every debug event.
                DebugEvents.record("place.route", ["mode": mode.rawValue, "via": found?.source.rawValue ?? "none",
                                                   "seconds": Int(found?.seconds ?? 0), "meters": Int(found?.meters ?? 0),
                                                   "steps": found?.steps.count ?? 0, "navigating": navigating, "from_home": isHome])
            }
        }
        guard !Task.isCancelled else { return }
        routeAt = Date()
        routeLoading = false
        #if SCREENSHOTS
        let found = PlaceTravelMode.cardOrder.compactMap { routes[ash] }
        MapSelfTestProbe.shared.routesText = found.isEmpty ? nil : found.map(\.mode.rawValue).joined(separator: ",")
        MapSelfTestProbe.shared.etaText = found.map { "\(ash.modeText) \(ash.durationText)" }.joined(separator: " · ")
        if let bike = routes[.bike], bike.estimated { MapSelfTestProbe.shared.fallbackReason = bike.source.rawValue }
        MapSelfTestProbe.shared.leaveAt = leaveAt
        if navigating { MapSelfTestProbe.shared.navigationSteps = route?.steps.count ?? 0 }
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

/// A place the app's map could not find: says so; Google Maps and Apple Maps only from the secondary menu.
struct PlaceNotFound: View {
    let place: Place

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("Not found on the map", systemImage: "mappin.slash")
                .font(LucinaTokens.Fonts.subheadline)
                .foregroundStyle(Theme.textSecondary)
            Menu {
                OpenInGoogleMapsButton(url: ExternalMaps.googleMapsSearchURL(place.searchQuery))
                OpenInAppleMapsButton(url: ExternalMaps.appleMapsSearchURL(place.searchQuery))
            } label: {
                Label("More", systemImage: "ellipsis.circle")
                    .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
            }
            .accessibilityIdentifier("placeNotFoundMore")
        }
    }
}
