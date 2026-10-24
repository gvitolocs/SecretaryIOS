import Foundation
import Observation

// The grocery deals map (`GET /v1/grocery/deals/map`): nearby supermarkets with the owner's
// list items on offer, food-waste clearances and flyer offers. secretary-server forwards
// Lucina's `/api/secretary/grocery/deals/map` and adds `maps` (the Google Maps keys).
// Everything is decoded leniently: unknown fields are ignored, a bad store or deal is
// skipped, and a missing optional field is nil. No Google Maps code here (tests use it).

/// The response of `GET /v1/grocery/deals/map`.
struct DealsMap: Decodable, Equatable, Sendable {
    struct Point: Decodable, Equatable, Sendable {
        let lat: Double
        let lng: Double
        let label: String?

        init(lat: Double, lng: Double, label: String? = nil) {
            self.lat = lat
            self.lng = lng
            self.label = label
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            guard let lat = c.lenientDouble(.lat), let lng = c.lenientDouble(.lng),
                  (-90...90).contains(lat), (-180...180).contains(lng) else {
                throw DecodingError.dataCorruptedError(forKey: .lat, in: c, debugDescription: "no coordinates")
            }
            self.lat = lat
            self.lng = lng
            label = c.lenientString(.label)
        }

        private enum CodingKeys: String, CodingKey { case lat, lng, label }
    }

    /// The Google Maps keys (only from secretary-server). Any of them may be null.
    struct MapsKeys: Decodable, Equatable, Sendable {
        let webKey: String?
        let iosKey: String?
        let mapId: String?

        init(webKey: String? = nil, iosKey: String? = nil, mapId: String? = nil) {
            self.webKey = webKey
            self.iosKey = iosKey
            self.mapId = mapId
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            webKey = c.lenientString(.webKey)?.nilIfBlank
            iosKey = c.lenientString(.iosKey)?.nilIfBlank
            mapId = c.lenientString(.mapId)?.nilIfBlank
        }

        private enum CodingKeys: String, CodingKey { case webKey, iosKey, mapId }
    }

    struct Deal: Decodable, Equatable, Identifiable, Sendable {
        enum Source: String, Sendable { case foodWaste = "food_waste", flyer, other }

        let key: String
        let source: Source
        let name: String
        let priceDkk: Double?
        let normalPriceDkk: Double?
        let discountPct: Double?
        let unitPrice: String?
        let stock: Double?
        let stockUnit: String?
        let validFrom: String?
        let validTo: String?
        /// `active` or `upcoming` (anything else counts as active).
        let status: String
        let category: String?

        var id: String { key }
        var isUpcoming: Bool { status == "upcoming" }

        init(key: String, source: Source, name: String, priceDkk: Double? = nil, normalPriceDkk: Double? = nil,
             discountPct: Double? = nil, unitPrice: String? = nil, stock: Double? = nil, stockUnit: String? = nil,
             validFrom: String? = nil, validTo: String? = nil, status: String = "active", category: String? = nil) {
            self.key = key
            self.source = source
            self.name = name
            self.priceDkk = priceDkk
            self.normalPriceDkk = normalPriceDkk
            self.discountPct = discountPct
            self.unitPrice = unitPrice
            self.stock = stock
            self.stockUnit = stockUnit
            self.validFrom = validFrom
            self.validTo = validTo
            self.status = status
            self.category = category
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let name = c.lenientString(.name)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            guard !name.isEmpty else {
                throw DecodingError.dataCorruptedError(forKey: .name, in: c, debugDescription: "a deal needs a name")
            }
            self.name = name
            let raw = c.lenientString(.source) ?? ""
            source = Source(rawValue: raw) ?? .other
            priceDkk = c.lenientDouble(.priceDkk)
            normalPriceDkk = c.lenientDouble(.normalPriceDkk)
            discountPct = c.lenientDouble(.discountPct)
            unitPrice = c.lenientString(.unitPrice)?.nilIfBlank
            stock = c.lenientDouble(.stock)
            stockUnit = c.lenientString(.stockUnit)?.nilIfBlank
            validFrom = c.lenientString(.validFrom)?.nilIfBlank
            validTo = c.lenientString(.validTo)?.nilIfBlank
            status = c.lenientString(.status) ?? "active"
            category = c.lenientString(.category)?.nilIfBlank
            key = c.lenientString(.key)?.nilIfBlank ?? "\(raw):\(name):\(priceDkk.map { String($0) } ?? "")"
        }

        private enum CodingKeys: String, CodingKey {
            case key, source, name, priceDkk, normalPriceDkk, discountPct, unitPrice, stock, stockUnit, validFrom, validTo, status, category
        }
    }

    /// A list item on offer at the store.
    struct ListMatch: Decodable, Equatable, Identifiable, Sendable {
        let item: String
        let ruleId: String?
        let deal: Deal

        var id: String { "\(ruleId ?? item)|\(deal.key)" }

        init(item: String, ruleId: String? = nil, deal: Deal) {
            self.item = item
            self.ruleId = ruleId
            self.deal = deal
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            deal = try c.decode(Deal.self, forKey: .deal)
            item = c.lenientString(.item)?.nilIfBlank ?? deal.name
            ruleId = c.lenientString(.ruleId)?.nilIfBlank
        }

        private enum CodingKeys: String, CodingKey { case item, ruleId, deal }
    }

    struct Store: Decodable, Equatable, Identifiable, Sendable {
        let id: String
        let chain: String?
        let chainKey: String?
        let name: String
        let street: String?
        let lat: Double
        let lng: Double
        let distanceM: Double?
        let listMatches: [ListMatch]
        let clearances: [Deal]
        let flyerOffers: [Deal]

        init(id: String, chain: String? = nil, chainKey: String? = nil, name: String, street: String? = nil,
             lat: Double, lng: Double, distanceM: Double? = nil,
             listMatches: [ListMatch] = [], clearances: [Deal] = [], flyerOffers: [Deal] = []) {
            self.id = id
            self.chain = chain
            self.chainKey = chainKey
            self.name = name
            self.street = street
            self.lat = lat
            self.lng = lng
            self.distanceM = distanceM
            self.listMatches = listMatches
            self.clearances = clearances
            self.flyerOffers = flyerOffers
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            guard let lat = c.lenientDouble(.lat), let lng = c.lenientDouble(.lng),
                  (-90...90).contains(lat), (-180...180).contains(lng) else {
                throw DecodingError.dataCorruptedError(forKey: .lat, in: c, debugDescription: "a store needs coordinates")
            }
            self.lat = lat
            self.lng = lng
            chain = c.lenientString(.chain)?.nilIfBlank
            chainKey = c.lenientString(.chainKey)?.nilIfBlank ?? chain.map(TextFold.fold)
            let name = c.lenientString(.name)?.nilIfBlank ?? chain ?? ""
            self.name = name
            street = c.lenientString(.street)?.nilIfBlank
            id = c.lenientString(.id)?.nilIfBlank ?? "\(chainKey ?? name)@\(lat),\(lng)"
            distanceM = c.lenientDouble(.distanceM)
            listMatches = c.lossyArray(ListMatch.self, .listMatches)
            clearances = c.lossyArray(Deal.self, .clearances)
            flyerOffers = c.lossyArray(Deal.self, .flyerOffers)
        }

        private enum CodingKeys: String, CodingKey {
            case id, chain, chainKey, name, street, lat, lng, distanceM, listMatches, clearances, flyerOffers
        }

        /// The chain's display name (the store name when the chain is unknown).
        var chainName: String { chain ?? name }

        /// The best clearance discount here, in percent (rounded).
        var bestClearancePct: Int? {
            clearances.compactMap(\.discountPct).max().map { Int($0.rounded()) }.flatMap { $0 > 0 ? $0 : nil }
        }

        /// What the marker says: the number of list items on offer, else the best clearance
        /// discount (`-40%`), else nothing (a plain dot).
        var markerLabel: String? {
            if !listMatches.isEmpty { return "\(listMatches.count)" }
            if let pct = bestClearancePct { return "-\(pct)%" }
            if !clearances.isEmpty { return "%" }
            return nil
        }

        var kind: DealsMapFilter.Kind? {
            if !listMatches.isEmpty { return .myList }
            if !clearances.isEmpty { return .clearance }
            return nil
        }
    }

    let generatedAt: String?
    let home: Point?
    let radiusM: Double?
    /// Sources that failed this time (`food_waste`, `flyer`).
    let failures: [String]
    let maps: MapsKeys?
    /// Sorted by distance (re-sorted here in case the server did not).
    let stores: [Store]

    init(generatedAt: String? = nil, home: Point? = nil, radiusM: Double? = nil, failures: [String] = [],
         maps: MapsKeys? = nil, stores: [Store] = []) {
        self.generatedAt = generatedAt
        self.home = home
        self.radiusM = radiusM
        self.failures = failures
        self.maps = maps
        self.stores = DealsMap.byDistance(stores)
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        generatedAt = c.lenientString(.generatedAt)
        home = try? c.decodeIfPresent(Point.self, forKey: .home)
        radiusM = c.lenientDouble(.radiusM)
        failures = c.lossyArray(FlexibleID.self, .failures).map(\.value)
        maps = try? c.decodeIfPresent(MapsKeys.self, forKey: .maps)
        stores = DealsMap.byDistance(c.lossyArray(Store.self, .stores))
    }

    private enum CodingKeys: String, CodingKey { case generatedAt, home, radiusM, failures, maps, stores }

    /// The key for the iOS map, when there is one.
    var iosKey: String? { maps?.iosKey }

    static func byDistance(_ stores: [Store]) -> [Store] {
        stores.enumerated().sorted { a, b in
            switch (a.element.distanceM, b.element.distanceM) {
            case let (x?, y?) where x != y: return x < y
            case (_?, nil): return true
            case (nil, _?): return false
            default: return a.offset < b.offset
            }
        }.map(\.element)
    }
}

/// The filter chips over the map: My list, Clearance, and one per chain.
struct DealsMapFilter: Equatable, Sendable {
    enum Kind: String, CaseIterable, Sendable { case myList, clearance }

    /// Empty: every store. Otherwise a store needs at least one of these.
    var kinds: Set<Kind> = []
    /// Empty: every chain.
    var chains: Set<String> = []

    var isEmpty: Bool { kinds.isEmpty && chains.isEmpty }

    func matches(_ store: DealsMap.Store) -> Bool {
        if !chains.isEmpty {
            guard let key = store.chainKey, chains.contains(key) else { return false }
        }
        if kinds.isEmpty { return true }
        return (kinds.contains(.myList) && !store.listMatches.isEmpty)
            || (kinds.contains(.clearance) && !store.clearances.isEmpty)
    }

    /// The matching stores, in their (distance) order.
    func apply(_ stores: [DealsMap.Store]) -> [DealsMap.Store] {
        stores.filter(matches)
    }

    mutating func toggle(_ kind: Kind) {
        if kinds.contains(kind) { kinds.remove(kind) } else { kinds.insert(kind) }
    }

    mutating func toggle(chain key: String) {
        if chains.contains(key) { chains.remove(key) } else { chains.insert(key) }
    }

    /// Drops chains that are no longer in the data (after a refresh).
    mutating func prune(to stores: [DealsMap.Store]) {
        chains.formIntersection(Set(stores.compactMap(\.chainKey)))
    }

    struct Chain: Equatable, Identifiable, Sendable {
        let key: String
        let name: String
        let count: Int
        var id: String { key }
    }

    /// One chip per chain, nearest chain first.
    static func chains(in stores: [DealsMap.Store]) -> [Chain] {
        var order: [String] = []
        var names: [String: String] = [:]
        var counts: [String: Int] = [:]
        for store in stores {
            guard let key = store.chainKey else { continue }
            if names[key] == nil {
                order.append(key)
                names[key] = store.chainName
            }
            counts[key, default: 0] += 1
        }
        return order.map { Chain(key: $0, name: names[$0] ?? $0, count: counts[$0] ?? 0) }
    }
}

/// The Directions button: Google Maps when it is installed, else Apple Maps.
enum DealsDirections {
    static let googleMapsProbe = URL(string: "comgooglemaps://")!

    static func url(lat: Double, lng: Double, googleMapsInstalled: Bool) -> URL? {
        let daddr = "\(lat),\(lng)"
        return URL(string: googleMapsInstalled ? "comgooglemaps://?daddr=\(daddr)" : "maps://?daddr=\(daddr)")
    }
}

/// Display helpers (shared by the map markers, the list and the store sheet).
enum DealsFormat {
    static func price(_ dkk: Double?) -> String? {
        guard let dkk else { return nil }
        return dkk.formatted(.currency(code: "DKK").precision(.fractionLength(dkk.rounded() == dkk ? 0 : 2)))
    }

    static func distance(_ meters: Double?) -> String? {
        guard let meters, meters >= 0 else { return nil }
        if meters < 1000 { return "\(Int(meters.rounded())) m" }
        return "\((meters / 1000).formatted(.number.precision(.fractionLength(1)))) km"
    }

    static func stock(_ deal: DealsMap.Deal) -> String? {
        guard let stock = deal.stock, stock > 0 else { return nil }
        let amount = stock.rounded() == stock ? String(Int(stock)) : stock.formatted(.number.precision(.fractionLength(0...2)))
        return [amount, deal.stockUnit].compactMap { $0 }.joined(separator: " ")
    }

    /// `2026-10-26T…` or `2026-10-26` → a date (local midnight for a bare date).
    static func date(_ value: String?) -> Date? {
        guard let value, !value.isEmpty else { return nil }
        let full = ISO8601DateFormatter()
        full.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = full.date(from: value) { return date }
        full.formatOptions = [.withInternetDateTime]
        if let date = full.date(from: value) { return date }
        let day = DateFormatter()
        day.locale = Locale(identifier: "en_US_POSIX")
        day.timeZone = .current
        day.dateFormat = "yyyy-MM-dd"
        return day.date(from: String(value.prefix(10)))
    }

    static func day(_ value: String?) -> String? {
        date(value).map { $0.formatted(.dateTime.weekday(.abbreviated).day().month(.abbreviated)) }
    }
}

/// Loads the map and holds the filter and the selected store.
@Observable
@MainActor
final class DealsMapStore {
    private(set) var map: DealsMap?
    private(set) var isLoading = false
    var error: String?
    var filter = DealsMapFilter()
    var selectedId: String?

    @ObservationIgnored private let api: SecretaryAPI

    init(api: SecretaryAPI) {
        self.api = api
    }

    var stores: [DealsMap.Store] { map?.stores ?? [] }
    var visibleStores: [DealsMap.Store] { filter.apply(stores) }
    var chains: [DealsMapFilter.Chain] { DealsMapFilter.chains(in: stores) }

    var selected: DealsMap.Store? {
        get { selectedId.flatMap { id in stores.first { $0.id == id } } }
        set { selectedId = newValue?.id }
    }

    func load(radiusM: Int?) async {
        isLoading = true
        defer { isLoading = false }
        do {
            let map = try await api.dealsMap(radiusM: radiusM)
            self.map = map
            filter.prune(to: map.stores)
            if let selectedId, !map.stores.contains(where: { $0.id == selectedId }) { self.selectedId = nil }
            error = nil
        } catch is CancellationError {
            return
        } catch {
            let apiError = APIError.wrap(error)
            if case .server(404, _, _) = apiError {
                self.error = String(localized: "The deals map isn't available on the server yet.")
            } else {
                self.error = apiError.userMessage
            }
        }
    }
}

extension SecretaryAPI {
    /// `GET /v1/grocery/deals/map?radiusM=` (500-15000 m; the server's default when nil).
    func dealsMap(radiusM: Int? = nil) async throws -> DealsMap {
        let query = radiusM.map { [URLQueryItem(name: "radiusM", value: String(min(15_000, max(500, $0))))] } ?? []
        return try await client.get("/v1/grocery/deals/map", query: query, timeout: 30)
    }
}

// MARK: - Lenient decoding

extension KeyedDecodingContainer {
    /// A number, or a string holding one.
    func lenientDouble(_ key: Key) -> Double? {
        if let value = try? decodeIfPresent(Double.self, forKey: key) { return value.isFinite ? value : nil }
        if let text = try? decodeIfPresent(String.self, forKey: key) {
            return Double(text.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: ",", with: "."))
        }
        return nil
    }

    /// A string, or a number written as one.
    func lenientString(_ key: Key) -> String? {
        if let value = try? decodeIfPresent(String.self, forKey: key) { return value }
        if let value = try? decodeIfPresent(Int64.self, forKey: key) { return String(value) }
        if let value = try? decodeIfPresent(Double.self, forKey: key) { return String(value) }
        return nil
    }

    /// An array whose bad elements are skipped (a missing or non-array value is empty).
    func lossyArray<T: Decodable>(_ type: T.Type, _ key: Key) -> [T] {
        guard let items = try? decodeIfPresent([LossyElement<T>].self, forKey: key) else { return [] }
        return items.compactMap(\.value)
    }
}

private struct LossyElement<T: Decodable>: Decodable {
    let value: T?

    init(from decoder: Decoder) throws {
        value = try? T(from: decoder)
    }
}

private extension String {
    var nilIfBlank: String? { trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : self }
}
