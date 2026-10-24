import GoogleMaps
import SwiftUI
import UIKit

// The grocery deals map: nearby supermarkets on a Google map (home marker plus one marker
// per store), filter chips, and the stores by distance under it. Tapping a store opens a
// sheet with the list items on offer there, its clearances and directions. Without an iOS
// Maps key from the server (`maps.iosKey`) the screen shows the list only.

/// The map button on the Shopping tab and the pushed screen. `/shopping/map` (grocery deal
/// alerts) opens it through `AppModel.showDealsMap`.
private struct DealsMapEntry: ViewModifier {
    @Environment(AppModel.self) private var app
    /// The legacy list has a navigation bar; the Bring! layout hides it, so the button floats.
    let inToolbar: Bool

    func body(content: Content) -> some View {
        @Bindable var app = app
        content
            .toolbar {
                if inToolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button { app.showDealsMap = true } label: { Image(systemName: "map") }
                            .accessibilityLabel("Deals map")
                            .accessibilityIdentifier("dealsMapButton")
                    }
                }
            }
            .overlay(alignment: .topTrailing) {
                if !inToolbar {
                    Button { app.showDealsMap = true } label: {
                        Image(systemName: "map")
                            .font(.title3.weight(.semibold))
                            .frame(width: 44, height: 44)
                            .background(.thinMaterial, in: Circle())
                    }
                    .buttonStyle(.plain)
                    .padding(.trailing, 16)
                    .padding(.top, 8)
                    .accessibilityLabel("Deals map")
                    .accessibilityIdentifier("dealsMapButton")
                }
            }
            .navigationDestination(isPresented: $app.showDealsMap) { DealsMapView(api: app.api) }
    }
}

extension View {
    /// The deals map button and screen, on the Shopping tab.
    func dealsMapEntry(inToolbar: Bool) -> some View { modifier(DealsMapEntry(inToolbar: inToolbar)) }
}

/// `GMSServices.provideAPIKey` once per process, before the first `GMSMapView`.
@MainActor
enum GoogleMapsSetup {
    private static var providedKey: String?

    /// True when a map can be shown (a key was provided now or earlier).
    static func provide(_ key: String?) -> Bool {
        if providedKey != nil { return true }
        guard let key = key?.trimmingCharacters(in: .whitespacesAndNewlines), !key.isEmpty else { return false }
        guard GMSServices.provideAPIKey(key) else { return false }
        providedKey = key
        return true
    }
}

struct DealsMapView: View {
    @Environment(\.colorScheme) private var colorScheme
    @AppStorage("secretary.deals.radiusM") private var radiusM = 0
    @State private var store: DealsMapStore
    @State private var mapReady = false
    @State private var focus: DealsGoogleMap.Focus?

    static let radii = [0, 1000, 2000, 3000, 5000, 10_000]

    init(api: SecretaryAPI) {
        _store = State(initialValue: DealsMapStore(api: api))
    }

    private var hasMapContent: Bool { store.map?.home != nil || !store.stores.isEmpty }

    var body: some View {
        VStack(spacing: 0) {
            if !store.stores.isEmpty { chips }
            if mapReady && hasMapContent, let map = store.map {
                DealsGoogleMap(home: map.home, stores: store.visibleStores, selectedId: store.selectedId,
                               mapId: map.maps?.mapId, style: colorScheme == .dark ? .dark : .light, focus: focus) { id in
                    store.selectedId = id
                }
                .containerRelativeFrame(.vertical) { height, _ in height * 0.42 }
                .accessibilityIdentifier("dealsMap")
            }
            list
        }
        .background(Theme.page.ignoresSafeArea())
        .navigationTitle("Deals nearby")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.visible, for: .navigationBar)
        .toolbar { ToolbarItem(placement: .topBarTrailing) { radiusMenu } }
        .task { if store.map == nil { await load() } }
        .onChange(of: store.map?.iosKey, initial: true) { _, key in
            mapReady = GoogleMapsSetup.provide(key)
        }
        .onChange(of: radiusM) { _, _ in Task { await load() } }
        .sheet(item: Binding(get: { store.selected }, set: { store.selected = $0 })) { selected in
            DealsStoreSheet(store: selected)
                .presentationDetents([.medium, .large])
                .presentationBackgroundInteraction(.enabled(upThrough: .medium))
        }
    }

    private func load() async {
        await store.load(radiusM: radiusM > 0 ? radiusM : nil)
    }

    private func select(_ s: DealsMap.Store) {
        focus = DealsGoogleMap.Focus(id: s.id, tick: (focus?.tick ?? 0) + 1)
        store.selectedId = s.id
    }

    // MARK: Chips

    private var chips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                chip(Text("My list"), systemImage: "checklist", on: store.filter.kinds.contains(.myList)) {
                    store.filter.toggle(.myList)
                }
                .accessibilityIdentifier("dealsChipMyList")
                chip(Text("Clearance"), systemImage: "tag", on: store.filter.kinds.contains(.clearance)) {
                    store.filter.toggle(.clearance)
                }
                .accessibilityIdentifier("dealsChipClearance")
                if store.chains.count > 1 {
                    Divider().frame(height: 20)
                    ForEach(store.chains) { chain in
                        chip(Text(verbatim: chain.name), systemImage: nil, on: store.filter.chains.contains(chain.key)) {
                            store.filter.toggle(chain: chain.key)
                        }
                    }
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .animation(.easeInOut(duration: 0.15), value: store.filter)
    }

    private func chip(_ title: Text, systemImage: String?, on: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 4) {
                if let systemImage { Image(systemName: systemImage).accessibilityHidden(true) }
                title.lineLimit(1)
            }
            .font(.subheadline.weight(.semibold))
            .padding(.horizontal, 12)
            .frame(height: 32)
            .foregroundStyle(on ? Color.white : Color.primary)
            .background(on ? Theme.accentFill : Theme.field, in: Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }

    private var radiusMenu: some View {
        Menu {
            Picker("Radius", selection: $radiusM) {
                ForEach(Self.radii, id: \.self) { meters in
                    if meters == 0 {
                        Text("Default").tag(meters)
                    } else {
                        Text(verbatim: DealsFormat.distance(Double(meters)) ?? "\(meters) m").tag(meters)
                    }
                }
            }
        } label: {
            Image(systemName: "scope")
        }
        .accessibilityLabel("Radius")
        .accessibilityIdentifier("dealsRadius")
    }

    // MARK: List

    private var list: some View {
        List {
            if let error = store.error {
                Section {
                    ErrorBanner(message: error) { store.error = nil }
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                    Button("Try again") { Task { await load() } }
                }
            }
            if let map = store.map {
                notes(map)
            }
            if store.map == nil {
                if store.isLoading {
                    Section { ProgressView().frame(maxWidth: .infinity) }
                }
            } else if store.stores.isEmpty {
                Section {
                    ContentUnavailableView("No supermarkets nearby", systemImage: "cart",
                                           description: Text("Try a larger radius."))
                        .listRowBackground(Color.clear)
                }
            } else if store.visibleStores.isEmpty {
                Section {
                    Text("No store matches these filters.").foregroundStyle(.secondary)
                    Button("Clear filters") { store.filter = DealsMapFilter() }
                }
            } else {
                Section {
                    ForEach(store.visibleStores) { s in
                        Button { select(s) } label: { DealsStoreRow(store: s) }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("dealsStore-\(s.id)")
                    }
                } header: {
                    Text("Stores by distance")
                }
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .refreshable { await load() }
    }

    @ViewBuilder
    private func notes(_ map: DealsMap) -> some View {
        let noKey = map.iosKey == nil && !mapReady
        let failures = Set(map.failures)
        if noKey || failures.contains("food_waste") || failures.contains("flyer") {
            Section {
                if noKey {
                    Label("The map needs a Google Maps key on the server. Here are the stores as a list.", systemImage: "map")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("dealsMapNoKey")
                }
                if failures.contains("food_waste") {
                    Label("Clearance deals couldn't be loaded this time.", systemImage: "exclamationmark.triangle")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                if failures.contains("flyer") {
                    Label("Flyer offers couldn't be loaded this time.", systemImage: "exclamationmark.triangle")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}

/// One store in the list: its badge, name, street, distance and what it has.
private struct DealsStoreRow: View {
    let store: DealsMap.Store

    var body: some View {
        HStack(spacing: 12) {
            DealsBadge(store: store)
            VStack(alignment: .leading, spacing: 2) {
                Text(store.name).font(.body.weight(.semibold)).lineLimit(1)
                if let place = [store.street, DealsFormat.distance(store.distanceM)].compactMap({ $0 }).joined(separator: " · ").nilIfEmpty {
                    Text(place).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
                Text(summary).font(.caption).foregroundStyle(store.kind == nil ? .secondary : .primary).lineLimit(2)
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(.tertiary)
                .accessibilityHidden(true)
        }
        .padding(.vertical, 2)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private var summary: String {
        var parts: [String] = []
        if !store.listMatches.isEmpty { parts.append(String(localized: "\(store.listMatches.count) from your list")) }
        if !store.clearances.isEmpty {
            if let pct = store.bestClearancePct {
                parts.append(String(localized: "\(store.clearances.count) on clearance, up to \("-\(pct)%")"))
            } else {
                parts.append(String(localized: "\(store.clearances.count) on clearance"))
            }
        }
        return parts.isEmpty ? String(localized: "Nothing on offer right now") : parts.joined(separator: " · ")
    }
}

/// The marker's look as a SwiftUI badge (the list rows).
private struct DealsBadge: View {
    let store: DealsMap.Store

    var body: some View {
        Text(verbatim: store.markerLabel ?? "")
            .font(.caption.weight(.bold).monospacedDigit())
            .foregroundStyle(.white)
            .lineLimit(1)
            .minimumScaleFactor(0.6)
            .frame(width: 40, height: 40)
            .background(Color(uiColor: DealsMarkerImage.color(for: store.kind, selected: false)), in: Circle())
            .accessibilityHidden(true)
    }
}

// MARK: - Store sheet

/// A store: the list items on offer, clearances, directions.
private struct DealsStoreSheet: View {
    let store: DealsMap.Store
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL

    var body: some View {
        NavigationStack {
            List {
                Section {
                    VStack(alignment: .leading, spacing: 4) {
                        if store.chain != nil && store.chainName != store.name {
                            Text(store.chainName).font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                        }
                        Text(store.name).font(.title3.bold())
                        if let place = [store.street, DealsFormat.distance(store.distanceM)].compactMap({ $0 }).joined(separator: " · ").nilIfEmpty {
                            Text(place).font(.subheadline).foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityElement(children: .combine)
                }
                Section("Your list") {
                    if store.listMatches.isEmpty {
                        Text("Nothing from your list here right now.").foregroundStyle(.secondary)
                    } else {
                        ForEach(store.listMatches) { match in
                            DealRow(deal: match.deal, title: match.item, showSource: true)
                        }
                    }
                }
                Section("Clearance") {
                    if store.clearances.isEmpty {
                        Text("No clearance deals here right now.").foregroundStyle(.secondary)
                    } else {
                        ForEach(store.clearances) { deal in
                            DealRow(deal: deal, title: deal.name, showSource: false)
                        }
                    }
                }
                Section("Directions") {
                    Button { openDirections() } label: {
                        Label("Get directions", systemImage: "arrow.triangle.turn.up.right.diamond")
                    }
                    .accessibilityIdentifier("dealsDirections")
                }
            }
            .listStyle(.insetGrouped)
            .navigationTitle(store.chainName)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
    }

    private func openDirections() {
        let google = UIApplication.shared.canOpenURL(DealsDirections.googleMapsProbe)
        if let url = DealsDirections.url(lat: store.lat, lng: store.lng, googleMapsInstalled: google) {
            openURL(url)
        }
    }
}

/// One deal: name (and the list item it matched), validity and stock; price, normal price
/// and discount on the right.
private struct DealRow: View {
    let deal: DealsMap.Deal
    let title: String
    let showSource: Bool

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.body.weight(.semibold))
                if title != deal.name {
                    Text(deal.name).font(.subheadline).foregroundStyle(.secondary)
                }
                if let meta = metaLine.nilIfEmpty {
                    Text(meta).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 2) {
                if let price = DealsFormat.price(deal.priceDkk) {
                    Text(verbatim: price).font(.body.weight(.bold).monospacedDigit())
                }
                if let normal = DealsFormat.price(deal.normalPriceDkk), deal.normalPriceDkk != deal.priceDkk {
                    Text(verbatim: normal).font(.caption.monospacedDigit()).strikethrough().foregroundStyle(.secondary)
                }
                if let pct = deal.discountPct, pct >= 1 {
                    Text(verbatim: "-\(Int(pct.rounded()))%")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(Color(uiColor: DealsMarkerImage.clearanceColor), in: Capsule())
                }
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var metaLine: String {
        var parts: [String] = []
        if showSource {
            switch deal.source {
            case .foodWaste: parts.append(String(localized: "Clearance"))
            case .flyer: parts.append(String(localized: "Flyer"))
            case .other: break
            }
        }
        if deal.isUpcoming, let from = DealsFormat.day(deal.validFrom) {
            parts.append(String(localized: "From \(from)"))
        } else if let until = DealsFormat.day(deal.validTo) {
            parts.append(String(localized: "Until \(until)"))
        }
        if let stock = DealsFormat.stock(deal) { parts.append(String(localized: "\(stock) left")) }
        if let unit = deal.unitPrice { parts.append(unit) }
        return parts.joined(separator: " · ")
    }
}

// MARK: - Google map

/// `GMSMapView` in SwiftUI: a home marker, one marker per store, the camera fitted to all of
/// them. Markers are rebuilt only when the stores, their labels or the selection change.
struct DealsGoogleMap: UIViewRepresentable {
    struct Focus: Equatable {
        let id: String
        let tick: Int
    }

    let home: DealsMap.Point?
    let stores: [DealsMap.Store]
    let selectedId: String?
    let mapId: String?
    let style: UIUserInterfaceStyle
    let focus: Focus?
    let onSelect: (String) -> Void

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> MapContainer {
        let options = GMSMapViewOptions()
        let center = home.map { CLLocationCoordinate2D(latitude: $0.lat, longitude: $0.lng) }
            ?? stores.first.map { CLLocationCoordinate2D(latitude: $0.lat, longitude: $0.lng) }
            ?? CLLocationCoordinate2D(latitude: 56.1629, longitude: 10.2039)
        options.camera = GMSCameraPosition(target: center, zoom: 14)
        if let mapId { options.mapID = GMSMapID(identifier: mapId) }
        let mapView = GMSMapView(options: options)
        mapView.settings.rotateGestures = false
        mapView.settings.tiltGestures = false
        mapView.settings.compassButton = false
        mapView.overrideUserInterfaceStyle = style
        mapView.delegate = context.coordinator
        let container = MapContainer(mapView: mapView)
        container.onLayout = { [weak coordinator = context.coordinator] in coordinator?.fitIfNeeded() }
        context.coordinator.container = container
        return container
    }

    func updateUIView(_ container: MapContainer, context: Context) {
        let coordinator = context.coordinator
        coordinator.onSelect = onSelect
        container.mapView.overrideUserInterfaceStyle = style
        coordinator.update(home: home, stores: stores, selectedId: selectedId)
        if let focus, focus != coordinator.lastFocus {
            coordinator.lastFocus = focus
            if let s = stores.first(where: { $0.id == focus.id }) {
                let zoom = max(container.mapView.camera.zoom, 15)
                container.mapView.animate(to: GMSCameraPosition(latitude: s.lat, longitude: s.lng, zoom: zoom))
            }
        }
    }

    /// Lays the map out and reports size changes (the camera fit needs a real frame).
    final class MapContainer: UIView {
        let mapView: GMSMapView
        var onLayout: (() -> Void)?

        init(mapView: GMSMapView) {
            self.mapView = mapView
            super.init(frame: .zero)
            addSubview(mapView)
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

        override func layoutSubviews() {
            super.layoutSubviews()
            mapView.frame = bounds
            onLayout?()
        }
    }

    @MainActor
    final class Coordinator: NSObject, @preconcurrency GMSMapViewDelegate {
        weak var container: MapContainer?
        var onSelect: ((String) -> Void)?
        var lastFocus: Focus?
        private var markers: [GMSMarker] = []
        private var homeMarker: GMSMarker?
        private var markerSignature: [String] = []
        private var fitSignature: [String] = []
        private var needsFit = false
        private var fitCoordinates: [CLLocationCoordinate2D] = []

        static let homeId = "\u{0}home"

        func update(home: DealsMap.Point?, stores: [DealsMap.Store], selectedId: String?) {
            guard let mapView = container?.mapView else { return }
            let homeKey = home.map { "\($0.lat),\($0.lng)" } ?? "-"
            let signature = [homeKey, selectedId ?? "-"] + stores.map { "\($0.id)|\($0.markerLabel ?? "")|\($0.kind?.rawValue ?? "")" }
            if signature != markerSignature {
                markerSignature = signature
                rebuild(on: mapView, home: home, stores: stores, selectedId: selectedId)
            }
            let fit = [homeKey] + stores.map(\.id)
            if fit != fitSignature {
                fitSignature = fit
                fitCoordinates = (home.map { [CLLocationCoordinate2D(latitude: $0.lat, longitude: $0.lng)] } ?? [])
                    + stores.map { CLLocationCoordinate2D(latitude: $0.lat, longitude: $0.lng) }
                needsFit = true
                fitIfNeeded()
            }
        }

        private func rebuild(on mapView: GMSMapView, home: DealsMap.Point?, stores: [DealsMap.Store], selectedId: String?) {
            markers.forEach { $0.map = nil }
            markers = []
            homeMarker?.map = nil
            homeMarker = nil
            if let home {
                let marker = GMSMarker(position: CLLocationCoordinate2D(latitude: home.lat, longitude: home.lng))
                marker.title = home.label ?? String(localized: "Home")
                marker.icon = DealsMarkerImage.home()
                marker.groundAnchor = CGPoint(x: 0.5, y: 0.5)
                marker.userData = Self.homeId
                marker.zIndex = 1
                marker.map = mapView
                homeMarker = marker
            }
            for store in stores {
                let selected = store.id == selectedId
                let marker = GMSMarker(position: CLLocationCoordinate2D(latitude: store.lat, longitude: store.lng))
                marker.title = store.name
                marker.icon = DealsMarkerImage.store(label: store.markerLabel, kind: store.kind, selected: selected)
                marker.groundAnchor = CGPoint(x: 0.5, y: 1)
                marker.userData = store.id
                marker.zIndex = selected ? 30 : (store.kind == .myList ? 20 : store.kind == .clearance ? 10 : 2)
                marker.map = mapView
                markers.append(marker)
            }
        }

        func fitIfNeeded() {
            guard needsFit, let mapView = container?.mapView, mapView.bounds.width > 1, mapView.bounds.height > 1 else { return }
            needsFit = false
            guard let first = fitCoordinates.first else { return }
            if fitCoordinates.count == 1 {
                mapView.moveCamera(GMSCameraUpdate.setTarget(first, zoom: 15))
                return
            }
            var bounds = GMSCoordinateBounds(coordinate: first, coordinate: first)
            for coordinate in fitCoordinates.dropFirst() { bounds = bounds.includingCoordinate(coordinate) }
            mapView.moveCamera(GMSCameraUpdate.fit(bounds, with: UIEdgeInsets(top: 56, left: 36, bottom: 24, right: 36)))
        }

        func mapView(_ mapView: GMSMapView, didTap marker: GMSMarker) -> Bool {
            guard let id = marker.userData as? String, id != Self.homeId else { return false }
            onSelect?(id)
            return true
        }
    }
}

/// Marker images: a coloured pill with the label and a pointer (stores), a house (home).
enum DealsMarkerImage {
    static var listColor: UIColor { UIColor(named: "BringRed") ?? .systemRed }
    static var clearanceColor: UIColor { .systemOrange }
    static var selectedColor: UIColor { UIColor(named: "AccentFill") ?? .systemIndigo }

    static func color(for kind: DealsMapFilter.Kind?, selected: Bool) -> UIColor {
        if selected { return selectedColor }
        switch kind {
        case .myList: return listColor
        case .clearance: return clearanceColor
        case nil: return .systemGray
        }
    }

    static func store(label: String?, kind: DealsMapFilter.Kind?, selected: Bool) -> UIImage {
        let fill = color(for: kind, selected: selected)
        let font = UIFont.systemFont(ofSize: selected ? 15 : 13, weight: .bold)
        let text = (label ?? "") as NSString
        let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: UIColor.white]
        let textSize = label == nil ? .zero : text.size(withAttributes: attributes)
        let height: CGFloat = label == nil ? (selected ? 20 : 16) : (selected ? 30 : 26)
        let width = max(height, ceil(textSize.width) + 16)
        let pointer: CGFloat = label == nil ? 0 : 6
        let border: CGFloat = 2
        let size = CGSize(width: width + border * 2, height: height + pointer + border * 2)
        return UIGraphicsImageRenderer(size: size).image { _ in
            let body = CGRect(x: border, y: border, width: width, height: height)
            let path = UIBezierPath(roundedRect: body, cornerRadius: height / 2)
            if pointer > 0 {
                let tip = UIBezierPath()
                tip.move(to: CGPoint(x: size.width / 2 - 6, y: body.maxY - 1))
                tip.addLine(to: CGPoint(x: size.width / 2, y: body.maxY + pointer))
                tip.addLine(to: CGPoint(x: size.width / 2 + 6, y: body.maxY - 1))
                tip.close()
                path.append(tip)
            }
            UIColor.white.setStroke()
            path.lineWidth = border * 2
            path.stroke()
            fill.setFill()
            path.fill()
            if label != nil {
                let origin = CGPoint(x: body.midX - textSize.width / 2, y: body.midY - textSize.height / 2)
                text.draw(at: origin, withAttributes: attributes)
            }
        }
    }

    static func home() -> UIImage {
        let size = CGSize(width: 34, height: 34)
        return UIGraphicsImageRenderer(size: size).image { _ in
            let circle = UIBezierPath(ovalIn: CGRect(origin: .zero, size: size).insetBy(dx: 2, dy: 2))
            UIColor.white.setStroke()
            circle.lineWidth = 4
            circle.stroke()
            selectedColor.setFill()
            circle.fill()
            let config = UIImage.SymbolConfiguration(pointSize: 15, weight: .bold)
            if let symbol = UIImage(systemName: "house.fill", withConfiguration: config)?.withTintColor(.white, renderingMode: .alwaysOriginal) {
                symbol.draw(at: CGPoint(x: (size.width - symbol.size.width) / 2, y: (size.height - symbol.size.height) / 2))
            }
        }
    }
}
