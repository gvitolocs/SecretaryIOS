import XCTest
@testable import Secretary

/// Parcels: the contract's JSON, the six calls, the store's order and its "not reachable" state, and the text helpers.
@MainActor
final class ParcelsTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    // MARK: Contract JSON

    nonisolated static let parcelJSON = """
    {
      "id": "pc-1a2b3c4d",
      "number": "00357059930440074440",
      "carrier": "postnord",
      "label": "Headphones",
      "state": "active",
      "schedule": "hourly",
      "near": true,
      "addedAt": "2026-11-11T15:58:00Z",
      "lastChecked": "2026-11-11T16:00:00Z",
      "nextCheck": "2026-11-11T17:00:00Z",
      "lastError": null,
      "status": {
        "stage": "out_for_delivery",
        "text": "The delivery of the shipment item is in progress.",
        "lastUpdate": "2026-09-28T05:33:46Z",
        "expectedDelivery": "2026-09-28",
        "location": "AARHUS DEPOT",
        "destinationCountry": "DK"
      },
      "events": [
        {"time": "2026-09-28T05:33:46Z", "description": "Loaded for delivery", "location": "AARHUS DEPOT", "stage": "out_for_delivery"},
        {"time": null, "description": "Something new", "location": null, "stage": "teleported"}
      ]
    }
    """

    nonisolated static let bareJSON = """
    {"id": "pc-00000001", "number": "ABC1234", "carrier": null, "label": null, "state": "stopped", "schedule": "stopped",
     "near": false, "addedAt": "2026-11-01T08:00:00Z", "lastChecked": null, "nextCheck": null, "lastError": "the carrier has no record of it yet",
     "status": null, "events": []}
    """

    nonisolated static func parcel(id: String, state: String = "active", changed: String? = nil, added: String = "2026-11-01T08:00:00Z") -> String {
        let status = changed.map { #"{"stage": "in_transit", "lastUpdate": "\#($0)"}"# } ?? "null"
        return #"{"id": "\#(id)", "number": "N\#(id)", "state": "\#(state)", "schedule": "daily", "near": false, "addedAt": "\#(added)", "status": \#(status), "events": []}"#
    }

    func testDecodesTheContractParcel() throws {
        let parcel = try Fixtures.decode(Parcel.self, Self.parcelJSON)
        XCTAssertEqual(parcel.id, "pc-1a2b3c4d")
        XCTAssertEqual(parcel.carrier, "postnord")
        XCTAssertEqual(parcel.state, .active)
        XCTAssertEqual(parcel.schedule, .hourly)
        XCTAssertTrue(parcel.near)
        XCTAssertNil(parcel.lastError)
        XCTAssertEqual(parcel.status?.stage, .outForDelivery)
        XCTAssertEqual(parcel.status?.expectedDelivery, "2026-09-28")
        XCTAssertEqual(parcel.status?.destinationCountry, "DK")
        XCTAssertEqual(parcel.events.count, 2)
        XCTAssertEqual(parcel.events[0].location, "AARHUS DEPOT")
        XCTAssertEqual(parcel.events[0].stage, .outForDelivery)
    }

    func testAnUnknownStageDecodesToUnknownAndNullsStayNil() throws {
        let parcel = try Fixtures.decode(Parcel.self, Self.parcelJSON)
        XCTAssertEqual(parcel.events[1].stage, .unknown)
        XCTAssertNil(parcel.events[1].time)
        XCTAssertNil(parcel.events[1].location)

        let bare = try Fixtures.decode(Parcel.self, Self.bareJSON)
        XCTAssertNil(bare.carrier)
        XCTAssertNil(bare.label)
        XCTAssertNil(bare.status)
        XCTAssertNil(bare.lastChecked)
        XCTAssertNil(bare.nextCheck)
        XCTAssertEqual(bare.lastError, "the carrier has no record of it yet")
        XCTAssertEqual(bare.state, .stopped)
        XCTAssertEqual(bare.schedule, .stopped)
        XCTAssertEqual(bare.events, [])
    }

    func testListResponseCarriesTheDigestHour() throws {
        let list = try Fixtures.decode(ParcelListResponse.self, #"{"parcels": [\#(Self.parcelJSON), \#(Self.bareJSON)], "digestHour": 9}"#)
        XCTAssertEqual(list.parcels.map(\.id), ["pc-1a2b3c4d", "pc-00000001"])
        XCTAssertEqual(list.digestHour, 9)
        let empty = try Fixtures.decode(ParcelListResponse.self, #"{"parcels": [], "digestHour": 9}"#)
        XCTAssertEqual(empty.parcels, [])
    }

    // MARK: The six calls

    private func body(of request: URLRequest) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: Any])
    }

    func testTheSixCallsUseTheContractPathsMethodsAndBodies() async throws {
        StubURLProtocol.handler = { request in
            switch (request.httpMethod, request.url?.path) {
            case ("GET", "/v1/parcels"): return .init(body: #"{"parcels": [\#(Self.bareJSON)], "digestHour": 9}"#)
            case ("DELETE", _): return .init(body: #"{"ok": true}"#)
            default: return .init(body: #"{"parcel": \#(Self.parcelJSON)}"#)
            }
        }
        let api = ParcelsAPI(client: StubURLProtocol.makeClient())

        let list = try await api.list()
        XCTAssertEqual(list.parcels.count, 1)
        var request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "GET")
        XCTAssertEqual(request.url?.path, "/v1/parcels")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer sec_test")

        let added = try await api.add(ParcelAddRequest(number: "00357059930440074440", carrier: nil, label: "Headphones"))
        XCTAssertEqual(added.id, "pc-1a2b3c4d")
        request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/v1/parcels")
        var sent = try body(of: request)
        XCTAssertEqual(sent["number"] as? String, "00357059930440074440")
        XCTAssertEqual(sent["label"] as? String, "Headphones")
        XCTAssertNil(sent["carrier"], "no carrier is left out, so the server detects it")

        _ = try await api.get("pc-1a2b3c4d")
        request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "GET")
        XCTAssertEqual(request.url?.path, "/v1/parcels/pc-1a2b3c4d")

        _ = try await api.refresh("pc-1a2b3c4d")
        request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/v1/parcels/pc-1a2b3c4d/refresh")

        _ = try await api.rename("pc-1a2b3c4d", label: "Gift")
        request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "PATCH")
        XCTAssertEqual(request.url?.path, "/v1/parcels/pc-1a2b3c4d")
        sent = try body(of: request)
        XCTAssertEqual(sent["label"] as? String, "Gift")

        _ = try await api.rename("pc-1a2b3c4d", label: nil)
        sent = try body(of: try XCTUnwrap(StubURLProtocol.requests.last))
        XCTAssertTrue(sent["label"] is NSNull, "clearing the label sends null")

        try await api.remove("pc-1a2b3c4d")
        request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "DELETE")
        XCTAssertEqual(request.url?.path, "/v1/parcels/pc-1a2b3c4d")
    }

    // MARK: Store

    func testTheStoreListsActiveParcelsFirstAndNewestChangeFirst() async throws {
        let json = """
        {"parcels": [
          \(Self.parcel(id: "pc-old", changed: "2026-11-01T10:00:00Z")),
          \(Self.parcel(id: "pc-done", state: "delivered", changed: "2026-11-10T10:00:00Z")),
          \(Self.parcel(id: "pc-new", changed: "2026-11-09T10:00:00Z")),
          \(Self.parcel(id: "pc-stopped", state: "stopped", changed: "2026-11-11T10:00:00Z"))
        ], "digestHour": 8}
        """
        StubURLProtocol.handler = { _ in .init(body: json) }
        let client = StubURLProtocol.makeClient()
        let store = ParcelsStore(api: { ParcelsAPI(client: client) })
        await store.load()
        XCTAssertTrue(store.hasLoaded)
        XCTAssertEqual(store.digestHour, 8)
        XCTAssertEqual(store.active.map(\.id), ["pc-new", "pc-old"])
        XCTAssertEqual(store.finished.map(\.id), ["pc-stopped", "pc-done"])
        XCTAssertNil(store.loadError)
        XCTAssertFalse(store.isUnavailable)
    }

    func testA503ParcelsUnavailableIsAStateNotAFailure() async throws {
        StubURLProtocol.handler = { _ in
            .init(status: 503, body: #"{"error":{"code":"parcels_unavailable","message":"Parcel tracking is off."}}"#)
        }
        let client = StubURLProtocol.makeClient()
        let store = ParcelsStore(api: { ParcelsAPI(client: client) })
        var reported: [APIError] = []
        store.onTransportError = { reported.append($0) }
        await store.load()
        XCTAssertTrue(store.isUnavailable)
        XCTAssertEqual(store.loadError, ParcelsStore.unavailableMessage)
        XCTAssertEqual(store.parcels, [])
        XCTAssertTrue(reported.isEmpty, "an unreachable feature does not go to the session handler")

        StubURLProtocol.handler = { _ in .init(body: #"{"parcels": [], "digestHour": 9}"#) }
        await store.load()
        XCTAssertFalse(store.isUnavailable)
        XCTAssertNil(store.loadError)
    }

    func testAddingKeepsTheServersMessageOnA400() async throws {
        StubURLProtocol.handler = { _ in
            .init(status: 400, body: #"{"error":{"code":"bad_request","message":"That tracking number looks invalid."}}"#)
        }
        let client = StubURLProtocol.makeClient()
        let store = ParcelsStore(api: { ParcelsAPI(client: client) })
        let ok = await store.add(number: " 12 ")
        XCTAssertFalse(ok)
        XCTAssertEqual(store.addError, "That tracking number looks invalid.")
        XCTAssertFalse(store.isUnavailable)
        XCTAssertEqual(store.parcels, [])
    }

    func testAddRenameCheckAndRemoveUpdateTheList() async throws {
        StubURLProtocol.handler = { request in
            switch request.httpMethod {
            case "DELETE": return .init(body: #"{"ok": true}"#)
            default: return .init(body: #"{"parcel": \#(Self.parcelJSON)}"#)
            }
        }
        let client = StubURLProtocol.makeClient()
        let store = ParcelsStore(api: { ParcelsAPI(client: client) })
        let ok = await store.add(number: "00357059930440074440", carrier: "", label: "  ")
        XCTAssertTrue(ok)
        XCTAssertEqual(store.parcels.map(\.id), ["pc-1a2b3c4d"])
        let sent = try body(of: try XCTUnwrap(StubURLProtocol.requests.last))
        XCTAssertNil(sent["carrier"])
        XCTAssertNil(sent["label"], "empty optional fields are left out")

        await store.checkNow(id: "pc-1a2b3c4d")
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.path, "/v1/parcels/pc-1a2b3c4d/refresh")
        XCTAssertEqual(store.parcels.count, 1)

        let renamed = await store.rename(id: "pc-1a2b3c4d", label: "")
        XCTAssertTrue(renamed)
        XCTAssertTrue(try body(of: try XCTUnwrap(StubURLProtocol.requests.last))["label"] is NSNull)

        let removed = await store.remove(id: "pc-1a2b3c4d")
        XCTAssertTrue(removed)
        XCTAssertEqual(store.parcels, [])
        XCTAssertTrue(store.busy.isEmpty)
    }

    func testResetEmptiesTheStore() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"parcels": [\#(Self.bareJSON)], "digestHour": 9}"#) }
        let client = StubURLProtocol.makeClient()
        let store = ParcelsStore(api: { ParcelsAPI(client: client) })
        await store.load()
        XCTAssertEqual(store.parcels.count, 1)
        store.reset()
        XCTAssertEqual(store.parcels, [])
        XCTAssertFalse(store.hasLoaded)
        XCTAssertNil(store.digestHour)
    }

    // MARK: ParcelText

    func testCarrierNames() {
        XCTAssertEqual(ParcelText.carrierName("postnord"), "PostNord")
        XCTAssertEqual(ParcelText.carrierName("gls"), "GLS")
        XCTAssertEqual(ParcelText.carrierName("gls-dk"), "GLS")
        XCTAssertEqual(ParcelText.carrierName("dao"), "DAO")
        XCTAssertEqual(ParcelText.carrierName("bring-posten"), "Bring")
        XCTAssertEqual(ParcelText.carrierName("dhl-express"), "DHL")
        XCTAssertEqual(ParcelText.carrierName("ups"), "UPS")
        XCTAssertEqual(ParcelText.carrierName("poste-italiane"), "Poste Italiane")
        XCTAssertEqual(ParcelText.carrierName("dpd-dk"), "DPD")
        XCTAssertEqual(ParcelText.carrierName("budbee"), "Budbee")
        XCTAssertEqual(ParcelText.carrierName("instabox"), "Instabox")
        XCTAssertEqual(ParcelText.carrierName("some-new-carrier"), "Some New Carrier")
        XCTAssertNil(ParcelText.carrierName(nil))
        XCTAssertNil(ParcelText.carrierName("  "))
    }

    func testTitleFallsBackToCarrierAndNumber() throws {
        let labelled = try Fixtures.decode(Parcel.self, Self.parcelJSON)
        XCTAssertEqual(ParcelText.title(labelled), "Headphones")
        XCTAssertEqual(ParcelText.subtitle(labelled), "PostNord · 00357059930440074440")
        var plain = labelled
        plain.label = nil
        XCTAssertEqual(ParcelText.title(plain), "PostNord 00357059930440074440")
        XCTAssertNil(ParcelText.subtitle(plain))
        plain.carrier = nil
        XCTAssertEqual(ParcelText.title(plain), "00357059930440074440")
    }

    func testStagesHaveNamesSymbolsAndAColour() {
        for stage in ParcelStage.allCases {
            XCTAssertFalse(ParcelText.stageName(stage).isEmpty)
            XCTAssertFalse(ParcelText.stageSymbol(stage).isEmpty)
        }
        XCTAssertEqual(ParcelText.stageName(.outForDelivery), "Out for delivery")
        XCTAssertEqual(ParcelText.stageName(.unknown), "Unknown")
    }

    func testScheduleBadgesAndThePluginLine() throws {
        var parcel = try Fixtures.decode(Parcel.self, Self.parcelJSON)
        XCTAssertEqual(ParcelText.scheduleBadge(parcel), "Hourly · near you")
        parcel.near = false
        XCTAssertEqual(ParcelText.scheduleBadge(parcel), "Hourly")
        parcel.schedule = .daily
        XCTAssertEqual(ParcelText.scheduleBadge(parcel), "Daily")
        parcel.schedule = .stopped
        XCTAssertNil(ParcelText.scheduleBadge(parcel))

        let moving = try Fixtures.decode(Parcel.self, Self.parcel(id: "pc-a", changed: "2026-11-09T10:00:00Z"))
        let moving2 = try Fixtures.decode(Parcel.self, Self.parcel(id: "pc-b", changed: "2026-11-09T11:00:00Z"))
        let done = try Fixtures.decode(Parcel.self, Self.parcel(id: "pc-c", state: "delivered"))
        XCTAssertEqual(ParcelText.onTheWay([moving, moving2, done]), 2)
        XCTAssertEqual(ParcelText.pluginDetail(parcels: [moving, moving2, done]), "2 on the way")
        XCTAssertEqual(ParcelText.pluginDetail(parcels: []), "Track deliveries with Lucina")
        XCTAssertEqual(ParcelText.pluginDetail(parcels: nil), "Track deliveries with Lucina")
    }

    func testDatesParseWithAndWithoutFractionalSeconds() {
        XCTAssertEqual(ParcelText.date("2026-11-11T16:00:00Z"), Date(timeIntervalSince1970: 1_794_412_800))
        XCTAssertEqual(ParcelText.date("2026-11-11T16:00:00.500Z"), Date(timeIntervalSince1970: 1_794_412_800.5))
        XCTAssertNil(ParcelText.date(nil))
        XCTAssertNil(ParcelText.date("soon"))
        XCTAssertEqual(ParcelText.expected("2026-09-28", locale: Locale(identifier: "en_US")), "Mon, Sep 28")
        XCTAssertEqual(ParcelText.expected("tomorrow", locale: Locale(identifier: "en_US")), "tomorrow")
        XCTAssertNil(ParcelText.expected(nil))
    }
}
