import XCTest
@testable import Secretary

final class ShoppingCatalogTests: XCTestCase {
    private let catalog = ShoppingCatalog(json: try! JSONDecoder().decode(JSONValue.self, from: Data("""
    {"version":1,"lang":"it",
     "sections":[{"id":"dairy","name":"Latticini","icon":"🧀","order":3},{"id":"produce","name":"Frutta e verdura","icon":"🥦","order":1},{"bad":1}],
     "items":[
       {"id":"banana","name":"Banane","names":{"en":"Bananas","da":"Bananer"},"aliases":["banana"],"section":"produce","icon":"🍌"},
       {"id":"oat-milk","name":"Latte d'avena","names":{"en":"Oat milk"},"aliases":["latte vegetale"],"section":"dairy","icon":"🥛"},
       {"id":"milk","name":"Latte","names":{"en":"Milk","da":"Mælk"},"aliases":[],"section":"dairy","icon":"🥛"},
       {"id":"cafe","name":"Caffè","names":{},"aliases":["espresso"],"section":null},
       {"name":"no id"}
     ]}
    """.utf8)))

    func testCatalogParsing() {
        XCTAssertEqual(catalog.sections.map(\.id), ["produce", "dairy"], "sorted by order, bad entries dropped")
        XCTAssertEqual(catalog.items.count, 4)
        XCTAssertEqual(catalog.item("cafe")?.icon, ShoppingCatalog.defaultIcon)
    }

    func testFolding() {
        XCTAssertEqual(TextFold.fold("  Caffè  in GRANI! "), "caffe in grani")
        XCTAssertEqual(TextFold.fold("Mælk"), "maelk")
        XCTAssertEqual(TextFold.fold("Latte d'avena"), "latte d avena")
        XCTAssertEqual(TextFold.fold("Straße 50%"), "strasse 50%")
        XCTAssertEqual(TextFold.fold("Rugbrød"), "rugbroed", "docs/catalog.md: ø -> oe")
        XCTAssertEqual(TextFold.fold("Blåbær"), "blaabaer", "docs/catalog.md: å -> aa")
        XCTAssertEqual(TextFold.itemKey(catalogId: "milk", name: "x"), "c:milk")
        XCTAssertEqual(TextFold.itemKey(catalogId: nil, name: "Caffè"), "n:caffe")
    }

    func testSearchRanking() {
        let search = CatalogSearch(catalog: catalog, lang: "it")
        XCTAssertEqual(search.search("latte").map(\.id), ["milk", "oat-milk"], "exact before prefix")
        XCTAssertEqual(search.search("caffe").map(\.id), ["cafe"], "diacritics folded")
        XCTAssertEqual(search.search("lat ave").map(\.id), ["oat-milk"], "every word starts a word")
        XCTAssertEqual(search.search("espresso").map(\.id), ["cafe"], "aliases")
        XCTAssertEqual(search.search("bananas").map(\.id), ["banana"], "other languages count as aliases")
        XCTAssertTrue(search.search("   ").isEmpty)
        // A boost (recently used) wins ties.
        XCTAssertEqual(search.search("lat", boost: { $0 == "oat-milk" ? 1 : 0 }).first?.id, "oat-milk")
        XCTAssertEqual(search.exactMatch("Banane")?.id, "banana")
        XCTAssertEqual(search.exactMatch("banana")?.id, "banana")
        XCTAssertNil(search.exactMatch("ban"))
        XCTAssertEqual(CatalogSearch(catalog: catalog, lang: "da").search("maelk").first?.id, "milk")
    }

    func testResolveRowsWithoutCatalogId() {
        let search = CatalogSearch(catalog: catalog, lang: "it")
        let hit = { (name: String) in search.resolve(name).map { [$0.item.id, $0.exact ? "name" : "alias"] } }
        XCTAssertEqual(hit("latte"), ["milk", "name"])
        XCTAssertEqual(hit("Milk"), ["milk", "name"], "names in every language")
        XCTAssertEqual(hit("MÆLK"), ["milk", "name"])
        XCTAssertEqual(hit("Bananas"), ["banana", "name"])
        XCTAssertEqual(hit("espresso"), ["cafe", "alias"])
        XCTAssertEqual(hit("latte vegetale"), ["oat-milk", "alias"])
        XCTAssertNil(hit("latt"), "no prefixes")
        XCTAssertNil(hit("milks"), "no plurals")
        XCTAssertNil(hit("the milk"))
        XCTAssertNil(hit(""))
    }
}

final class ShopOpsTests: XCTestCase {
    private func ctx(_ ids: [String] = []) -> ShopOps.Context {
        var queue = ids
        return ShopOps.Context(listId: "default", now: "2026-10-21T10:00:00.000Z",
                               newId: { queue.isEmpty ? UUID().uuidString : queue.removeFirst() },
                               catalog: { $0 == "banana" ? CatalogItem(id: "banana", name: "Banane", names: [:], aliases: [], section: "produce", icon: "🍌") : nil })
    }

    private let base = [
        ShopItem(id: "a", catalogId: "banana", name: "Banane", section: "produce", status: .needed, position: 1),
        ShopItem(id: "b", name: "Pane", status: .recent, position: 2),
    ]

    func testAddRevivesRecentAndDedupesByKey() {
        let result = ShopOps.apply(base, [ShopOp(opId: "1", kind: .add, name: "pane"), ShopOp(opId: "2", kind: .add, catalogId: "banana", name: "Banane", spec: "6")], context: ctx())
        XCTAssertEqual(result.items.count, 2, "one active row per key")
        XCTAssertEqual(result.items.first { $0.id == "b" }?.status, .needed, "recent comes back")
        XCTAssertEqual(result.items.first { $0.id == "a" }?.spec, "6")
        XCTAssertEqual(result.items.first { $0.id == "b" }?.updatedBy, "ios")
    }

    private static let banana = CatalogItem(id: "banana", name: "Banane", names: [:], aliases: [], section: "produce", icon: "🍌")

    func testNewRowUsesTheClientIdAndCatalogHints() {
        let result = ShopOps.apply([], [ShopOp.add(object: Self.banana, rowId: "client-1", name: "Banane")], context: ctx())
        XCTAssertEqual(result.items.first?.id, "client-1")
        XCTAssertEqual(result.items.first?.objectId, "banana")
        XCTAssertEqual(result.items.first?.kind, "catalog")
        XCTAssertEqual(result.items.first?.icon, "🍌")
        XCTAssertEqual(result.items.first?.section, "produce")
    }

    func testItemObjectsMatchInAnyLanguageAndEditEverywhere() {
        let milk = ShopItem(id: "m", objectId: "milk", kind: "catalog", names: ["it": "Latte", "en": "Milk", "da": "Mælk"],
                            catalogId: "milk", name: "Latte", status: .recent, position: 1)
        let soap = ShopItem(id: "s", objectId: "u_01JB8Z6Q3W4E5R6T7Y8U9I0P1A", kind: "custom",
                            names: ["it": "Detersivo piatti", "en": "Dish soap", "da": "Opvaskemiddel"], name: "Detersivo piatti", position: 2)
        let added = ShopOps.apply([milk, soap], [ShopOp.add(text: "MÆLK", rowId: "r1"), ShopOp.add(text: "dish soap", rowId: "r2")], context: ctx())
        XCTAssertEqual(added.items.count, 2, "one active row per object")
        XCTAssertEqual(added.items.first { $0.id == "m" }?.status, .needed, "recent comes back")
        XCTAssertEqual(milk.displayName("da"), "Mælk")

        var context = ctx()
        context.lang = "en"
        let edited = ShopOps.apply([soap], [ShopOp.editItem("u_01JB8Z6Q3W4E5R6T7Y8U9I0P1A", names: ["en": "Dishwashing liquid"], icon: "🫧")],
                                   context: context).items[0]
        XCTAssertEqual(edited.name, "Dishwashing liquid")
        XCTAssertEqual(edited.icon, "🫧")
        XCTAssertEqual(edited.names?["it"], "Detersivo piatti")

        // An outbox from before item objects: the add's row id was a UUID in itemId.
        let rowUUID = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b"
        let legacy = ShopOps.apply([], [ShopOp(opId: "l", kind: .add, itemId: rowUUID, catalogId: "banana", name: "Banane")], context: ctx())
        XCTAssertEqual(legacy.items.first?.id, rowUUID)
        XCTAssertEqual(legacy.items.first?.objectId, "banana")
    }

    func testRemovedStaysRemovedAndDuplicatesAreSkipped() {
        var seen = Set<String>()
        let removed = ShopOps.apply(base, [ShopOp(opId: "r", kind: .remove, itemId: "a")], context: ctx(), seen: &seen)
        XCTAssertEqual(removed.items.first { $0.id == "a" }?.status, .removed)
        let again = ShopOps.apply(removed.items, [ShopOp(opId: "r", kind: .remove, itemId: "a"), ShopOp(opId: "n", kind: .need, itemId: "a")],
                                  context: ctx(), seen: &seen)
        XCTAssertEqual(again.duplicates, ["r"])
        XCTAssertEqual(again.items.first { $0.id == "a" }?.status, .removed, "need can't revive a removed row")
        // Re-adding the name makes a new row with a fresh id.
        let readd = ShopOps.apply(again.items, [ShopOp.add(object: Self.banana, rowId: "a", name: "Banane")], context: ctx(["fresh"]))
        XCTAssertEqual(readd.items.filter(\.isActive).first { $0.catalogId == "banana" }?.id, "fresh")
    }

    func testPurchaseNeedClearRecentAndMove() {
        let ops = [ShopOp(opId: "1", kind: .purchase, itemId: "a"), ShopOp(opId: "2", kind: .clearRecent)]
        let result = ShopOps.apply(base, ops, context: ctx())
        XCTAssertTrue(result.items.allSatisfy { $0.status == .removed })
        let moved = ShopOps.apply(base, [ShopOp(opId: "m", kind: .move, itemId: "a", toListId: "hardware")], context: ctx())
        XCTAssertEqual(moved.items.first { $0.id == "a" }?.status, .removed)
        let same = ShopOps.apply(base, [ShopOp(opId: "m", kind: .move, itemId: "a", toListId: "default")], context: ctx())
        XCTAssertEqual(same.items.first { $0.id == "a" }?.status, .needed, "moving to the same list does nothing")
    }

    func testRebaseGivesNewIdsAndDropsDeadOps() {
        let fresh = [ShopItem(id: "a", catalogId: "banana", name: "Banane", status: .recent, position: 1)] // someone bought it; b is gone
        let pending = [
            ShopOp(opId: "p1", kind: .purchase, itemId: "a"),     // now a no-op
            ShopOp(opId: "p2", kind: .setSpec, itemId: "b", spec: "x"), // row is gone
            ShopOp(opId: "p3", kind: .need, itemId: "a"),
        ]
        let rebased = ShopOps.rebase(fresh, pending, context: ctx(["n1", "n2", "n3"]))
        XCTAssertEqual(rebased.map(\.kind), [.need])
        XCTAssertFalse(rebased.contains { ["p1", "p2", "p3"].contains($0.opId) }, "new op ids")
    }

    func testOpWireEncoding() throws {
        func json(_ op: ShopOp) throws -> [String: Any] {
            try JSONSerialization.jsonObject(with: JSONEncoder().encode(op)) as! [String: Any]
        }
        let clear = try json(ShopOp(opId: "1", kind: .setQuantity, itemId: "a", quantity: nil))
        XCTAssertEqual(clear["type"] as? String, "set_quantity")
        XCTAssertTrue(clear["quantity"] is NSNull)
        let add = try json(.add(text: "Pane", rowId: "c"))
        XCTAssertEqual(Set(add.keys), ["opId", "type", "rowId", "text", "name"])
        let known = try json(.add(object: Self.banana, rowId: "c", name: "Banane"))
        XCTAssertEqual(known["itemId"] as? String, "banana")
        XCTAssertEqual(known["catalogId"] as? String, "banana", "legacy field for servers before item objects")
        let edit = try json(.editItem("u_1", names: ["en": "Dish soap"], icon: ""))
        XCTAssertEqual(edit["type"] as? String, "edit_item")
        XCTAssertEqual((edit["names"] as? [String: String])?["en"], "Dish soap")
        XCTAssertEqual(edit["icon"] as? String, "")
        XCTAssertEqual(try JSONDecoder().decode(ShopOp.self, from: JSONEncoder().encode(ShopOp.editItem("u_1", names: ["da": "x"]))).names, ["da": "x"])
        let reorder = try json(ShopOp(opId: "2", kind: .reorder, itemId: "a", position: 3))
        XCTAssertEqual(reorder["position"] as? Int, 3)
        // Round trip (the outbox is persisted in this shape).
        let op = ShopOp(opId: "3", kind: .move, itemId: "a", toListId: "hardware")
        XCTAssertEqual(try JSONDecoder().decode(ShopOp.self, from: JSONEncoder().encode(op)), op)
    }

    func testListStateDecoding() throws {
        let json = try JSONDecoder().decode(JSONValue.self, from: Data("""
        {"list":{"id":"default","name":"Casa","theme":"home"},"revision":42,
         "items":[{"id":"1","catalogId":"banana","name":"Banane","spec":"6","quantity":null,"status":"needed","position":3,"updatedBy":"telegram","updatedAt":"2026-10-21T10:00:00.000Z"},
                  {"id":2,"name":"Odd","status":"archived"},{"name":"no id"}]}
        """.utf8))
        let state = ListState(json: json, fallbackId: "default")
        XCTAssertEqual(state.items.first?.objectId, "banana", "rows from before item objects: the catalog id")
        XCTAssertEqual(state.revision, 42)
        let objectJSON = try JSONDecoder().decode(JSONValue.self, from: Data(#"{"id":"r","itemId":"u_1","catalogId":null,"name":"Dish soap","typedName":"detersivo piatti","status":"needed","item":{"id":"u_1","kind":"custom","names":{"it":"Detersivo piatti","en":"Dish soap"},"needsTranslation":true}}"#.utf8))
        let row = try XCTUnwrap(ShopItem(json: objectJSON, index: 0))
        XCTAssertEqual(row.objectId, "u_1")
        XCTAssertTrue(row.isCustom)
        XCTAssertEqual(row.typedName, "detersivo piatti")
        XCTAssertEqual(row.needsTranslation, true)
        XCTAssertEqual(row.displayName("it"), "Detersivo piatti")
        XCTAssertEqual(row.customObject(lang: "en")?.name, "Dish soap")
        XCTAssertEqual(state.list.theme, .home)
        XCTAssertEqual(state.items.map(\.id), ["1", "2"])
        XCTAssertEqual(state.items[1].status, .removed, "unknown statuses are treated like removed")
        XCTAssertNotNil(state.items[0].updatedDate)
        XCTAssertEqual(ShopList.lists(from: try JSONDecoder().decode(JSONValue.self, from: Data(#"{"lists":[{"id":"x","theme":"boat"}]}"#.utf8))).first?.theme, .other)
    }
}

@MainActor
final class ShoppingV2StoreTests: XCTestCase {
    private var fileURL: URL!

    override func setUp() {
        fileURL = FileManager.default.temporaryDirectory.appendingPathComponent("shopping-\(UUID().uuidString).json")
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: fileURL)
    }

    private func makeStore() -> ShoppingV2Store {
        let client = StubURLProtocol.makeClient()
        return ShoppingV2Store(api: { ShoppingV2Client(client: client) }, fileURL: fileURL)
    }

    private static let state = #"{"list":{"id":"default","name":"Casa","theme":"home"},"revision":1,"items":[{"id":"a","name":"Pane","status":"needed","position":1}]}"#

    private func opsBodies() -> [[String: Any]] {
        StubURLProtocol.requests.filter { $0.url?.path.hasSuffix("/ops") == true }
            .compactMap { try? JSONSerialization.jsonObject(with: $0.httpBody ?? Data()) as? [String: Any] }
    }

    func testOfflineTapsAreQueuedPersistedAndReplayedWithTheSameOpIds() async throws {
        var online = true
        StubURLProtocol.handler = { request in
            guard online else { return .init(error: URLError(.notConnectedToInternet)) }
            switch request.url?.path {
            case "/v1/shopping/catalog": return .init(body: #"{"version":1,"lang":"it","sections":[],"items":[]}"#, headers: ["ETag": "\"v1\""])
            case "/v1/shopping/items": return .init(body: "[]")
            case "/v1/shopping/lists": return .init(body: #"[{"id":"default","name":"Casa","theme":"home","needed":1,"recent":0}]"#)
            case "/v1/shopping/lists/default": return .init(body: Self.state)
            default:
                let body = try? JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: Any]
                let ids = (body?["ops"] as? [[String: Any]])?.compactMap { $0["opId"] as? String } ?? []
                return .init(body: #"{"revision":2,"applied":\#(ids.debugDescription),"duplicates":[],"items":[{"id":"a","name":"Pane","status":"recent","position":1}]}"#)
            }
        }
        let store = makeStore()
        await store.load(lang: "it")
        XCTAssertEqual(store.availability, .available)
        XCTAssertEqual(store.needed.map(\.id), ["a"])

        online = false
        store.toggle(store.needed[0])
        XCTAssertEqual(store.recent.map(\.id), ["a"], "optimistic")
        await store.flush()
        XCTAssertEqual(store.outbox.count, 1, "kept for later")
        XCTAssertNotNil(store.syncProblem)
        let queuedId = try XCTUnwrap(store.outbox.first?.op.opId)

        // A new launch still has the queued op.
        let relaunched = makeStore()
        XCTAssertEqual(relaunched.outbox.map(\.op.opId), [queuedId])
        XCTAssertEqual(relaunched.recent.map(\.id), ["a"])

        online = true
        await relaunched.flush()
        XCTAssertTrue(relaunched.outbox.isEmpty)
        XCTAssertEqual(relaunched.server["default"]?.revision, 2)
        let sent = opsBodies().last
        XCTAssertEqual((sent?["ops"] as? [[String: Any]])?.first?["opId"] as? String, queuedId, "replayed with the same op id")
        XCTAssertEqual(sent?["expectedRevision"] as? Int, 1)
        XCTAssertEqual(StubURLProtocol.requests.last { $0.url?.path.hasSuffix("/ops") == true }?.value(forHTTPHeaderField: "X-Secretary-Client"), "ios")
    }

    func testConflictRefetchesAndRebasesWithNewOpIds() async throws {
        var opsCalls = 0
        StubURLProtocol.handler = { request in
            switch request.url?.path {
            case "/v1/shopping/catalog": return .init(body: #"{"version":1,"lang":"it","sections":[],"items":[]}"#)
            case "/v1/shopping/items": return .init(body: "[]")
            case "/v1/shopping/lists": return .init(body: #"[{"id":"default","name":"Casa","theme":"home"}]"#)
            case "/v1/shopping/lists/default":
                return .init(body: opsCalls == 0 ? Self.state : #"{"list":{"id":"default","name":"Casa","theme":"home"},"revision":5,"items":[{"id":"a","name":"Pane","status":"needed","position":1},{"id":"z","name":"Latte","status":"needed","position":2}]}"#)
            default:
                opsCalls += 1
                if opsCalls == 1 { return .init(status: 409, body: #"{"error":{"code":"revision_conflict","message":"The list changed."}}"#) }
                let body = try? JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: Any]
                let ids = (body?["ops"] as? [[String: Any]])?.compactMap { $0["opId"] as? String } ?? []
                return .init(body: #"{"revision":6,"applied":\#(ids.debugDescription),"duplicates":[],"items":[{"id":"a","name":"Pane","status":"recent","position":1},{"id":"z","name":"Latte","status":"needed","position":2}]}"#)
            }
        }
        let store = makeStore()
        await store.load(lang: "it")
        store.toggle(store.needed[0])
        let firstId = try XCTUnwrap(store.outbox.first?.op.opId)
        await store.flush()
        XCTAssertTrue(store.outbox.isEmpty)
        XCTAssertEqual(opsCalls, 2)
        let bodies = opsBodies()
        let retriedId = (bodies.last?["ops"] as? [[String: Any]])?.first?["opId"] as? String
        XCTAssertNotEqual(retriedId, firstId, "rebased with a new op id")
        XCTAssertEqual(bodies.last?["expectedRevision"] as? Int, 5)
        XCTAssertEqual(store.needed.map(\.id), ["z"])
        XCTAssertEqual(store.recent.map(\.id), ["a"])
    }

    func testUndoAndLegacyFallback() async {
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/shopping/lists" { return .init(status: 404, body: #"{"error":{"code":"not_found","message":"Not found."}}"#) }
            return .init(status: 404, body: #"{"error":{"code":"not_found","message":"Not found."}}"#)
        }
        let store = makeStore()
        await store.load(lang: "it")
        XCTAssertEqual(store.availability, .unavailable, "old servers keep the legacy list")
    }

    func testAddMatchesTheCatalogAndUndoRemoves() async throws {
        StubURLProtocol.handler = { request in
            switch request.url?.path {
            case "/v1/shopping/catalog":
                return .init(body: #"{"version":1,"lang":"it","sections":[{"id":"produce","name":"Frutta","icon":"🥦","order":1}],"items":[{"id":"banana","name":"Banane","names":{},"aliases":["banana"],"section":"produce","icon":"🍌"}]}"#)
            case "/v1/shopping/lists": return .init(body: #"[{"id":"default","name":"Casa","theme":"home"}]"#)
            case "/v1/shopping/lists/default": return .init(body: #"{"list":{"id":"default","name":"Casa","theme":"home"},"revision":1,"items":[]}"#)
            default: return .init(error: URLError(.notConnectedToInternet))
            }
        }
        let store = makeStore()
        await store.load(lang: "it")
        store.add("banana")
        let added = try XCTUnwrap(store.needed.first)
        XCTAssertEqual(added.catalogId, "banana")
        XCTAssertEqual(added.name, "Banane")
        XCTAssertEqual(store.icon(for: added), "🍌")
        XCTAssertEqual(store.undo?.message, "Added Banane")
        store.applyUndo()
        XCTAssertTrue(store.needed.isEmpty)
        XCTAssertNil(store.undo)
    }

    func testRowsWithoutCatalogIdGetTheirIconAndSectionByName() async {
        StubURLProtocol.handler = { request in
            switch request.url?.path {
            case "/v1/shopping/catalog":
                return .init(body: #"{"version":1,"lang":"it","sections":[{"id":"dairy-eggs","name":"Latticini","icon":"🧀","order":3}],"items":[{"id":"milk","name":"Latte","names":{"it":"Latte","en":"Milk","da":"Mælk"},"aliases":["latte intero"],"section":"dairy-eggs","icon":"🥛"},{"id":"yogurt","name":"Yogurt","names":{"en":"Yogurt","da":"Yoghurt"},"aliases":[],"section":"dairy-eggs","icon":"🥛"}]}"#)
            case "/v1/shopping/lists": return .init(body: #"[{"id":"default","name":"Casa","theme":"home"}]"#)
            case "/v1/shopping/lists/default":
                // Rows from before the catalog, as an old Lucina serves them: no catalogId, section or icon.
                return .init(body: #"{"list":{"id":"default","name":"Casa","theme":"home"},"revision":1,"items":[{"id":"a","name":"milk","status":"needed","position":1},{"id":"b","name":"latte intero","status":"needed","position":2},{"id":"c","name":"yogurt","status":"needed","position":3},{"id":"d","name":"Candele","status":"needed","position":4},{"id":"e","name":"latte","status":"needed","position":5,"icon":"🍼","section":"other-shelf"}]}"#)
            default: return .init(error: URLError(.notConnectedToInternet))
            }
        }
        let store = makeStore()
        await store.load(lang: "it")
        let byId = Dictionary(uniqueKeysWithValues: store.needed.map { ($0.id, $0) })
        XCTAssertEqual(byId.count, 5)
        XCTAssertEqual(store.icon(for: byId["a"]!), "🥛")
        XCTAssertEqual(store.section(for: byId["a"]!), "dairy-eggs")
        XCTAssertEqual(store.icon(for: byId["b"]!), "🥛", "aliases resolve too")
        XCTAssertEqual(store.icon(for: byId["c"]!), "🥛")
        XCTAssertEqual(store.icon(for: byId["d"]!), ShoppingCatalog.defaultIcon, "the cart only for unknown free text")
        XCTAssertNil(store.section(for: byId["d"]!))
        XCTAssertEqual(store.icon(for: byId["e"]!), "🍼", "stored values win")
        XCTAssertEqual(store.section(for: byId["e"]!), "other-shelf")
        XCTAssertNil(byId["a"]!.catalogId, "the row itself is not rewritten")
    }

    func testNamesFollowTheUILanguageAndCustomObjectsAreSearchable() async throws {
        StubURLProtocol.handler = { request in
            switch request.url?.path {
            case "/v1/shopping/catalog":
                return .init(body: #"{"version":1,"lang":"en","sections":[],"items":[{"id":"milk","name":"Milk","names":{"it":"Latte","en":"Milk","da":"Mælk"},"aliases":[],"section":"dairy","icon":"🥛"}]}"#)
            case "/v1/shopping/items":
                return .init(body: #"[{"id":"u_01JB8Z6Q3W4E5R6T7Y8U9I0P1A","kind":"custom","names":{"it":"Spugne","en":"Sponges","da":"Svampe"},"aliases":["spugne"],"section":"household","icon":"🧽"}]"#)
            case "/v1/shopping/lists": return .init(body: #"[{"id":"default","name":"Casa","theme":"home"}]"#)
            case "/v1/shopping/lists/default":
                return .init(body: #"{"list":{"id":"default","name":"Casa","theme":"home"},"revision":1,"items":[{"id":"a","itemId":"milk","catalogId":"milk","name":"Latte","typedName":"latte","status":"needed","position":1,"item":{"id":"milk","kind":"catalog","names":{"it":"Latte","en":"Milk","da":"Mælk"}}}]}"#)
            default: return .init(error: URLError(.notConnectedToInternet))
            }
        }
        let store = makeStore()
        await store.load(lang: "en")
        XCTAssertEqual(store.needed.first?.name, "Milk", "the object's name in the UI language")
        XCTAssertTrue(StubURLProtocol.requests.contains { $0.url?.path == "/v1/shopping/lists/default" && $0.url?.query == "lang=en" })
        XCTAssertEqual(store.search.search("svampe").first?.id, "u_01JB8Z6Q3W4E5R6T7Y8U9I0P1A", "custom objects are searchable in any language")
        store.add("spugne")
        let added = try XCTUnwrap(store.needed.first { $0.objectId == "u_01JB8Z6Q3W4E5R6T7Y8U9I0P1A" })
        XCTAssertEqual(added.name, "Sponges")
        XCTAssertEqual(store.outbox.last?.op.itemId, "u_01JB8Z6Q3W4E5R6T7Y8U9I0P1A")
        store.add("MILK")
        XCTAssertEqual(store.needed.filter { $0.objectKey == "milk" }.count, 1, "no duplicate row for the same object")
        await store.loadCatalog(lang: "da")
        XCTAssertEqual(store.needed.first { $0.id == "a" }?.name, "Mælk")
    }

    func testBackgroundRestoreKeepsOpsQueuedBeforeTheFileWasRead() async throws {
        StubURLProtocol.handler = { _ in .init(error: URLError(.notConnectedToInternet)) }
        // The last launch left one op in the outbox (written synchronously by the test store).
        let previous = makeStore()
        previous.perform([ShopOp(kind: .need, itemId: "a")])
        let savedId = try XCTUnwrap(previous.outbox.first?.op.opId)

        // The app's store reads the file off the main thread; a tap (a control at launch) comes first.
        let client = StubURLProtocol.makeClient()
        let store = ShoppingV2Store(api: { ShoppingV2Client(client: client) }, fileURL: fileURL, restoresInBackground: true)
        store.perform([ShopOp(kind: .purchase, itemId: "b")])
        let earlyId = try XCTUnwrap(store.outbox.last?.op.opId)
        await store.flush() // waits for the restore
        XCTAssertEqual(store.outbox.map(\.op.opId), [savedId, earlyId], "the saved ops first, then the early one")
    }

    func testComingBackToTheTabDoesNotRefetchEverything() async {
        StubURLProtocol.handler = { request in
            switch request.url?.path {
            case "/v1/shopping/catalog": return .init(body: #"{"version":1,"lang":"it","sections":[],"items":[]}"#)
            case "/v1/shopping/items": return .init(body: "[]")
            case "/v1/shopping/lists": return .init(body: #"[{"id":"default","name":"Casa","theme":"home"}]"#)
            case "/v1/shopping/lists/default": return .init(body: Self.state)
            default: return .init(error: URLError(.notConnectedToInternet))
            }
        }
        let store = makeStore()
        await store.loadIfStale(lang: "it")
        let afterFirst = StubURLProtocol.requests.count
        XCTAssertGreaterThan(afterFirst, 0)
        XCTAssertEqual(store.needed.map(\.id), ["a"])
        await store.loadIfStale(lang: "it")
        XCTAssertEqual(StubURLProtocol.requests.count, afterFirst, "a second visit within a minute sends nothing")
        await store.load(lang: "it")
        XCTAssertGreaterThan(StubURLProtocol.requests.count, afterFirst, "pull to refresh still reloads")
    }
}
