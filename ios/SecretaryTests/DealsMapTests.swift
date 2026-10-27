import XCTest
@testable import Secretary

final class DealsMapTests: XCTestCase {
    private static let sample = """
    {
      "generatedAt": "2026-10-24T12:00:00.000Z",
      "home": { "lat": 56.19633, "lng": 10.2011, "label": "Risskov" },
      "radiusM": 3000,
      "failures": ["flyer"],
      "maps": { "webKey": "web-test", "iosKey": "ios-test", "mapId": null },
      "stores": [
        { "id": "tjek:9", "chain": "Netto", "chainKey": "netto", "name": "Netto Vejlby", "street": "Vejlby Centervej 1",
          "lat": 56.2, "lng": 10.21, "distanceM": 1200, "listMatches": [], "clearances": [], "flyerOffers": [] },
        { "id": "salling:0847307a", "chain": "føtex", "chainKey": "foetex", "name": "føtex Skejby", "street": "Skelagervej 1-9",
          "lat": 56.193657, "lng": 10.193406, "distanceM": 560,
          "listMatches": [{ "item": "Prosciutto crudo", "ruleId": "prosciutto_crudo",
                            "deal": { "key": "flyer:1", "source": "flyer", "name": "Prosciutto crudo 80 g", "priceDkk": 20,
                                      "normalPriceDkk": null, "discountPct": null, "unitPrice": "250,00 kr/kg",
                                      "stock": null, "stockUnit": null, "validFrom": "2026-10-27", "validTo": "2026-11-02",
                                      "status": "upcoming", "category": null } }],
          "clearances": [
            { "key": "fw:1", "source": "food_waste", "name": "Skyr", "priceDkk": "9.5", "normalPriceDkk": 19,
              "discountPct": 50, "stock": 3, "stockUnit": "stk", "validFrom": "2026-10-24T06:00:00Z",
              "validTo": "2026-10-24T20:00:00Z", "status": "active" },
            { "key": "fw:2", "source": "food_waste", "name": "Rugbrød", "priceDkk": 12, "normalPriceDkk": 20, "discountPct": 40 },
            { "key": "fw:bad", "source": "food_waste" }
          ],
          "flyerOffers": [{ "key": "flyer:1", "source": "flyer", "name": "Prosciutto crudo 80 g", "priceDkk": 20 }],
          "somethingNew": true },
        { "id": "tjek:3", "chain": "Rema 1000", "chainKey": "rema", "name": "Rema 1000 Risskov",
          "lat": 56.19, "lng": 10.22, "distanceM": 900, "clearances": [
            { "key": "fw:9", "source": "food_waste", "name": "Bananer", "priceDkk": 5, "discountPct": 30 }] },
        { "id": "broken", "name": "No coordinates" }
      ]
    }
    """

    private func decode(_ json: String) throws -> DealsMap {
        try JSONDecoder().decode(DealsMap.self, from: Data(json.utf8))
    }

    func testDecodesTheContract() throws {
        let map = try decode(Self.sample)
        XCTAssertEqual(map.home, DealsMap.Point(lat: 56.19633, lng: 10.2011, label: "Risskov"))
        XCTAssertEqual(map.radiusM, 3000)
        XCTAssertEqual(map.failures, ["flyer"])
        XCTAssertEqual(map.stores.map(\.id), ["salling:0847307a", "tjek:3", "tjek:9"], "sorted by distance, the broken store skipped")

        let foetex = try XCTUnwrap(map.stores.first)
        XCTAssertEqual(foetex.chainName, "føtex")
        XCTAssertEqual(foetex.listMatches.count, 1)
        let match = try XCTUnwrap(foetex.listMatches.first)
        XCTAssertEqual(match.item, "Prosciutto crudo")
        XCTAssertEqual(match.deal.source, .flyer)
        XCTAssertTrue(match.deal.isUpcoming)
        XCTAssertNil(match.deal.normalPriceDkk)
        XCTAssertEqual(match.deal.unitPrice, "250,00 kr/kg")
        XCTAssertEqual(foetex.clearances.map(\.key), ["fw:1", "fw:2"], "a deal without a name is skipped")
        XCTAssertEqual(foetex.clearances.first?.priceDkk, 9.5, "numbers sent as strings are read")
        XCTAssertEqual(foetex.clearances.first?.stock, 3)
        XCTAssertEqual(foetex.clearances[1].status, "active", "missing status means active")
        XCTAssertEqual(foetex.flyerOffers.count, 1)
    }

    func testMinimalAndMissingFields() throws {
        let map = try decode(#"{"stores":[{"lat":56.1,"lng":10.1,"chain":"Lidl"}],"maps":{"iosKey":"  "}}"#)
        XCTAssertNil(map.home)
        XCTAssertEqual(map.failures, [])
        let store = try XCTUnwrap(map.stores.first)
        XCTAssertEqual(store.name, "Lidl")
        XCTAssertEqual(store.chainKey, "lidl")
        XCTAssertFalse(store.id.isEmpty)
        XCTAssertEqual(store.listMatches, [])
        XCTAssertNil(store.markerLabel)
        XCTAssertNil(store.kind)

        let empty = try decode("{}")
        XCTAssertEqual(empty.stores, [])
        XCTAssertNil(empty.home)
    }

    func testMarkerLabels() throws {
        let map = try decode(Self.sample)
        XCTAssertEqual(map.stores[0].markerLabel, "1", "list matches win")
        XCTAssertEqual(map.stores[0].kind, .myList)
        XCTAssertEqual(map.stores[0].bestClearancePct, 50)
        XCTAssertEqual(map.stores[1].markerLabel, "-30%", "best clearance discount")
        XCTAssertEqual(map.stores[1].kind, .clearance)
        XCTAssertNil(map.stores[2].markerLabel)
    }

    func testFilter() throws {
        let stores = try decode(Self.sample).stores
        var filter = DealsMapFilter()
        XCTAssertTrue(filter.isEmpty)
        XCTAssertEqual(filter.apply(stores).map(\.id), ["salling:0847307a", "tjek:3", "tjek:9"], "no chips: every store")

        filter.toggle(.myList)
        XCTAssertEqual(filter.apply(stores).map(\.id), ["salling:0847307a"])
        filter.toggle(.clearance)
        XCTAssertEqual(filter.apply(stores).map(\.id), ["salling:0847307a", "tjek:3"], "kinds combine with or")
        filter.toggle(.myList)
        XCTAssertEqual(filter.apply(stores).map(\.id), ["salling:0847307a", "tjek:3"])

        filter.toggle(chain: "rema")
        XCTAssertEqual(filter.apply(stores).map(\.id), ["tjek:3"], "chains and kinds combine with and")
        filter.toggle(.clearance)
        filter.toggle(chain: "netto")
        XCTAssertEqual(filter.apply(stores).map(\.id), ["tjek:3", "tjek:9"])
        filter.toggle(chain: "rema")
        filter.toggle(chain: "netto")
        XCTAssertTrue(filter.isEmpty)
    }

    func testChainsAndPrune() throws {
        let stores = try decode(Self.sample).stores
        XCTAssertEqual(DealsMapFilter.chains(in: stores).map(\.key), ["foetex", "rema", "netto"], "nearest chain first")
        XCTAssertEqual(DealsMapFilter.chains(in: stores).first?.name, "føtex")

        var filter = DealsMapFilter(kinds: [.clearance], chains: ["rema", "gone"])
        filter.prune(to: stores)
        XCTAssertEqual(filter.chains, ["rema"])
        XCTAssertEqual(filter.kinds, [.clearance])
    }

    /// Navigate on a store opens the app's own map at the store, never another maps app.
    func testStoreNavigatesInTheApp() throws {
        let store = DealsMap.Store(id: "tjek:1", chain: "Netto", name: "Netto Vesterbro", street: "Vesterbrogade 1",
                                   lat: 56.19, lng: 10.19)
        XCTAssertEqual(store.place.title, "Netto Vesterbro")
        XCTAssertEqual(store.place.address, "Vesterbrogade 1")
        XCTAssertEqual(store.place.coordinate?.latitude, 56.19)
        let route = PlaceLinks.route(store.place, navigate: true)
        XCTAssertTrue(route.hasPrefix("/map/place?"))
        XCTAssertTrue(PlaceLinks.navigates(route))
        XCTAssertEqual(Place.fromRoute(route)?.coordinate?.longitude, 10.19)
        XCTAssertEqual(DealsMap.Store(id: "x", chain: "Rema 1000", name: "Viby", lat: 56.1, lng: 10.1).place.title, "Rema 1000 Viby")
    }

    func testDealDates() {
        XCTAssertNotNil(DealsFormat.date("2026-10-24T20:00:00Z"))
        XCTAssertNotNil(DealsFormat.date("2026-10-24T20:00:00.000Z"))
        XCTAssertNotNil(DealsFormat.date("2026-10-27"))
        XCTAssertNil(DealsFormat.date("soon"))
        XCTAssertNil(DealsFormat.date(nil))
    }

    func testDealsMapRoute() {
        XCTAssertTrue(NotificationRoute.opensDealsMap("/shopping/map"))
        XCTAssertTrue(NotificationRoute.opensDealsMap("/shopping/map?store=x"))
        XCTAssertFalse(NotificationRoute.opensDealsMap("/shopping"))
        XCTAssertFalse(NotificationRoute.opensDealsMap("//shopping/map"))
        XCTAssertEqual(NotificationRoute.tab(for: "/shopping/map"), .shopping)
    }
}
