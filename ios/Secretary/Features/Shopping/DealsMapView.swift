import MapKit
import SwiftUI
import UIKit

// The grocery deals map: nearby supermarkets on the app's map (LucinaMap, MapKit: home pin plus
// one pin per store), filter chips, and the stores by distance under it. Tapping a store opens a
// sheet with the list items on offer there, its clearances and directions.

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

struct DealsMapView: View {
    @AppStorage("secretary.deals.radiusM") private var radiusM = 0
    @State private var store: DealsMapStore
    @State private var focus: MapFocus?

    static let radii = [0, 1000, 2000, 3000, 5000, 10_000]

    init(api: SecretaryAPI) {
        _store = State(initialValue: DealsMapStore(api: api))
    }

    private var hasMapContent: Bool { store.map?.home != nil || !store.stores.isEmpty }

    var body: some View {
        VStack(spacing: 0) {
            if !store.stores.isEmpty { chips }
            if hasMapContent {
                LucinaMap(pins: pins, selectedId: Binding(get: { store.selectedId }, set: { store.selectedId = $0 }),
                          showsUserLocation: true, focus: focus)
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
        #if SCREENSHOTS
        .onChange(of: store.map, initial: true) { _, map in
            if map != nil { MapSelfTestProbe.shared.dealsPins = pins.count }
        }
        #endif
        .onChange(of: radiusM) { _, _ in Task { await load() } }
        .sheet(item: Binding(get: { store.selected }, set: { store.selected = $0 })) { selected in
            DealsStoreSheet(store: selected)
                .presentationDetents([.medium, .large])
                .presentationBackgroundInteraction(.enabled(upThrough: .medium))
        }
    }

    /// Home, then one pin per visible store: its label (list items on offer, else the best clearance)
    /// and the colour of what it has.
    private var pins: [MapPin] {
        var pins: [MapPin] = []
        if let home = store.map?.home {
            pins.append(MapPin(id: Self.homePinId, coordinate: CLLocationCoordinate2D(latitude: home.lat, longitude: home.lng),
                               title: home.label ?? String(localized: "Home"), tint: .home))
        }
        for s in store.visibleStores {
            pins.append(MapPin(id: s.id, coordinate: CLLocationCoordinate2D(latitude: s.lat, longitude: s.lng), title: s.name,
                               subtitle: s.street, label: s.markerLabel, tint: DealsMapTint.tint(for: s.kind)))
        }
        return pins
    }

    private static let homePinId = "\u{0}home"

    private func load() async {
        await store.load(radiusM: radiusM > 0 ? radiusM : nil)
    }

    private func select(_ s: DealsMap.Store) {
        focus = MapFocus(id: s.id, tick: (focus?.tick ?? 0) + 1)
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
        .lucinaList()
        .scrollContentBackground(.hidden)
        .refreshable { await load() }
    }

    @ViewBuilder
    private func notes(_ map: DealsMap) -> some View {
        let failures = Set(map.failures)
        if failures.contains("food_waste") || failures.contains("flyer") {
            Section {
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
            .background(DealsMapTint.tint(for: store.kind).color(selected: false), in: Circle())
            .accessibilityHidden(true)
    }
}

// MARK: - Store sheet

/// A store: the list items on offer, clearances, directions.
private struct DealsStoreSheet: View {
    let store: DealsMap.Store
    @Environment(\.dismiss) private var dismiss

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
                    // Lucina's map, already navigating; Apple Maps only from the long-press menu.
                    NavigationLink {
                        if let coordinate = store.place.coordinate {
                            PlaceMapView(place: store.place, coordinate: coordinate, address: store.street, startsNavigating: true)
                        }
                    } label: {
                        Label("Navigate", systemImage: PlaceTravelMode.bike.symbol)
                    }
                    .accessibilityIdentifier("dealsDirections")
                    .contextMenu {
                        let coordinate = CLLocationCoordinate2D(latitude: store.lat, longitude: store.lng)
                        OpenInGoogleMapsButton(to: coordinate, mode: .bike)
                        OpenInAppleMapsButton(to: coordinate, name: store.place.title, mode: .bike)
                    }
                }
            }
            .lucinaList()
            .navigationTitle(store.chainName)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
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
                        .background(MapPin.Tint.clearance.color(selected: false), in: Capsule())
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

// MARK: - Pins

/// The pin colour of a store: accent for list items on offer, orange for clearances, grey otherwise.
enum DealsMapTint {
    static func tint(for kind: DealsMapFilter.Kind?) -> MapPin.Tint {
        switch kind {
        case .myList: return .accent
        case .clearance: return .clearance
        case nil: return .neutral
        }
    }
}
