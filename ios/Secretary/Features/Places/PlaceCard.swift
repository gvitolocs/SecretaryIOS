import MapKit
import SwiftUI

/// A place in the chat (a `place` block, a proactive message's `meta.place`, a shared location or
/// maps link): a still map, the title, address, time and distance. Tapping it opens the full map.
/// A place MKLocalSearch can't find says so and offers to search in Maps (only on tap).
struct PlaceCard: View {
    @Environment(AppModel.self) private var app
    @Environment(\.openURL) private var openURL
    let place: Place
    @State private var resolved: PlaceResolver.Resolved?
    @State private var notFound = false
    @State private var showMap = false

    var body: some View {
        Group {
            if resolved != nil {
                Button(action: open) { card }
                    .buttonStyle(.plain)
                    .accessibilityHint(Text("Opens the map"))
            } else {
                card
            }
        }
        .accessibilityIdentifier("placeCard")
        .task(id: place) { await resolve() }
        .sheet(isPresented: $showMap) {
            if let resolved {
                NavigationStack {
                    PlaceMapView(place: place, coordinate: resolved.coordinate, address: resolved.address)
                        .toolbar {
                            ToolbarItem(placement: .confirmationAction) { Button("Done") { showMap = false } }
                        }
                }
                .lucinaSheet()
            }
        }
        #if SCREENSHOTS
        .onChange(of: MapSelfTestProbe.shared.openTick) { _, _ in
            if MapSelfTestProbe.shared.targetTitle == place.title, resolved != nil { open() }
        }
        #endif
    }

    private var card: some View {
        Card(spacing: 10) {
            if let resolved {
                PlaceSnapshotView(coordinate: resolved.coordinate)
            } else if !notFound {
                RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile, style: .continuous)
                    .fill(Theme.field)
                    .frame(height: PlaceSnapshotView.height)
                    .overlay { ProgressView() }
                    .accessibilityHidden(true)
            }
            VStack(alignment: .leading, spacing: 4) {
                Label {
                    Text(place.title).font(LucinaTokens.Fonts.headline).foregroundStyle(Theme.text)
                } icon: {
                    Image(systemName: place.kind == .event ? "calendar" : "mappin.and.ellipse").foregroundStyle(Theme.accent)
                }
                if let address = place.address ?? resolved?.address {
                    Text(address).font(LucinaTokens.Fonts.subheadline).foregroundStyle(Theme.textSecondary)
                }
                if let time = place.timeText {
                    Label(time, systemImage: "clock")
                        .font(LucinaTokens.Fonts.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
                if let resolved, let origin = PlaceOrigin.current(home: PlaceResolver.shared.home) {
                    Label(origin.distanceText(to: resolved.coordinate), systemImage: origin.isHome ? "house" : "location.fill")
                        .font(LucinaTokens.Fonts.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
            }
            .multilineTextAlignment(.leading)
            if notFound {
                Label("Not found on the map", systemImage: "mappin.slash")
                    .font(LucinaTokens.Fonts.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                Button {
                    if let url = PlaceDirections.searchURL(place.searchQuery) { openURL(url) }
                } label: {
                    Label("Search in Maps", systemImage: "magnifyingglass")
                        .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                        .foregroundStyle(Theme.onAccent)
                        .padding(.horizontal, 14)
                        .frame(height: 34)
                        .background(Theme.accentFill, in: Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("placeSearchInMaps")
            }
        }
        .contentShape(RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous))
    }

    private func open() {
        showMap = true
    }

    private func resolve() async {
        notFound = false
        let location = PlaceLocation.shared
        if location.isAuthorized && location.current == nil { location.refresh() }
        let found = await PlaceResolver.shared.resolve(place, api: app.api)
        guard !Task.isCancelled else { return }
        resolved = found
        notFound = found == nil
        if found != nil { await PlaceResolver.shared.loadHome(api: app.api) }
        #if SCREENSHOTS
        if MapSelfTestProbe.shared.targetTitle == place.title {
            MapSelfTestProbe.shared.resolved = found
            MapSelfTestProbe.shared.notFound = found == nil
        }
        #endif
    }
}

/// A still map of a coordinate (MKMapSnapshotter, light or dark like the app) with a pin at the
/// centre, as wide as its container.
struct PlaceSnapshotView: View {
    @Environment(\.colorScheme) private var colorScheme
    let coordinate: CLLocationCoordinate2D
    @State private var image: UIImage?

    static let height: CGFloat = 140

    var body: some View {
        GeometryReader { geo in
            let size = CGSize(width: geo.size.width.rounded(), height: Self.height)
            let dark = colorScheme == .dark
            ZStack {
                Theme.field
                if let image { Image(uiImage: image).resizable().scaledToFill() }
                Image(systemName: "mappin.circle.fill")
                    .font(.system(size: 28))
                    .symbolRenderingMode(.palette)
                    .foregroundStyle(.white, Theme.accentFill)
                    .shadow(color: .black.opacity(0.3), radius: 2, y: 1)
            }
            .frame(width: geo.size.width, height: Self.height)
            .task(id: PlaceSnapshots.Key(coordinate, size: size, dark: dark)) {
                guard size.width > 10 else { return }
                if let hit = PlaceSnapshots.cached(coordinate, size: size, dark: dark) {
                    image = hit
                } else {
                    image = await PlaceSnapshots.image(coordinate, size: size, dark: dark)
                }
            }
        }
        .frame(height: Self.height)
        .clipShape(RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile, style: .continuous))
        .accessibilityHidden(true)
    }
}

/// Map snapshots by coordinate, size and colour scheme, for the session (the last 60).
@MainActor
enum PlaceSnapshots {
    struct Key: Hashable {
        let lat: Int
        let lng: Int
        let width: Int
        let height: Int
        let dark: Bool

        init(_ coordinate: CLLocationCoordinate2D, size: CGSize, dark: Bool) {
            lat = Int((coordinate.latitude * 100_000).rounded())
            lng = Int((coordinate.longitude * 100_000).rounded())
            width = Int(size.width)
            height = Int(size.height)
            self.dark = dark
        }
    }

    private static var cache: [Key: UIImage] = [:]
    private static var order: [Key] = []
    private static let limit = 60

    static func cached(_ coordinate: CLLocationCoordinate2D, size: CGSize, dark: Bool) -> UIImage? {
        cache[Key(coordinate, size: size, dark: dark)]
    }

    static func image(_ coordinate: CLLocationCoordinate2D, size: CGSize, dark: Bool) async -> UIImage? {
        let key = Key(coordinate, size: size, dark: dark)
        if let hit = cache[key] { return hit }
        let options = MKMapSnapshotter.Options()
        options.region = MKCoordinateRegion(center: coordinate, latitudinalMeters: 600,
                                            longitudinalMeters: 600 * max(1, size.width / max(1, size.height)))
        options.size = size
        options.pointOfInterestFilter = .excludingAll
        options.traitCollection = UITraitCollection(userInterfaceStyle: dark ? .dark : .light)
        guard let image = try? await MKMapSnapshotter(options: options).start().image else { return nil }
        if cache[key] == nil { order.append(key) }
        cache[key] = image
        if order.count > limit { cache[order.removeFirst()] = nil }
        return image
    }
}
