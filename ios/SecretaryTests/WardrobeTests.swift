import XCTest
@testable import Secretary

final class WardrobeTests: XCTestCase {
    private func json(_ text: String) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8))
    }

    func testWardrobeKeepsOnlyValidOutfits() throws {
        let list = Outfit.list(from: try json("""
        [
          {"id": "uniform", "name": {"it": "Divisa", "en": "Uniform"}, "glb": "lucina.glb", "thumbnail": "outfit-uniform.webp", "tags": ["uniform"]},
          {"id": "gym", "name": "Gym", "glb": "lucina-gym.glb", "tags": ["sport"]},
          {"id": "gym", "glb": "dup.glb"},
          {"id": "Bad", "glb": "x.glb"},
          {"id": "evil", "glb": "../x.glb"},
          {"id": "maid", "glb": "lucina-maid.glb", "thumbnail": "x.json", "tags": ["maid"]}
        ]
        """))
        XCTAssertEqual(list.map(\.id), ["uniform", "gym", "maid"])
        XCTAssertEqual(list[0].names["it"], "Divisa")
        XCTAssertEqual(list[1].names["en"], "Gym")
        XCTAssertNil(list[2].thumbnail)
        XCTAssertEqual(list.map(\.category), [.uniform, .sport, .work])
        XCTAssertTrue(Outfit.list(from: try json("{}")).isEmpty)
    }

    func testSettingsResolveBySection() throws {
        var s = AvatarSettings(json: try json(#"{"outfit": null, "auto": true, "rules": {"gym": "gym", "shopping": "maid"}}"#))
        XCTAssertEqual(s.rules.default, "uniform", "missing rules take the default")
        XCTAssertEqual(s.wanted(in: .gym), "gym")
        XCTAssertEqual(s.wanted(in: .shopping), "maid")
        XCTAssertEqual(s.wanted(in: .default), "uniform")
        s.auto = false
        XCTAssertEqual(s.wanted(in: .gym), "uniform")
        s.outfit = "maid"
        XCTAssertEqual(s.wanted(in: .default), "maid")
        XCTAssertEqual(AvatarSettings.Section(tab: .gym), .gym)
        XCTAssertEqual(AvatarSettings.Section(tab: .weight), .default)
    }

    func testPatchEncodingKeepsNullAndAbsentApart() throws {
        let encode = { (p: AvatarPatch) in String(data: try APIClient.encoder.encode(p), encoding: .utf8) }
        XCTAssertEqual(try encode(AvatarPatch(outfit: .some("maid"))), #"{"outfit":"maid"}"#)
        XCTAssertEqual(try encode(AvatarPatch(outfit: .some(nil), auto: true)), #"{"auto":true,"outfit":null}"#)
        XCTAssertEqual(try encode(AvatarPatch(rules: ["gym": "sport"])), #"{"rules":{"gym":"sport"}}"#)
        let applied = AvatarPatch(outfit: .some(nil), rules: ["shopping": "chef"]).applied(to: AvatarSettings(json: try json(#"{"outfit": "maid"}"#)))
        XCTAssertNil(applied.outfit)
        XCTAssertEqual(applied.rules.shopping, "chef")
    }

    @MainActor
    func testLucinasLiveOutfitChangeAppliesAtOnce() {
        let store = WardrobeStore(api: { AvatarAPI(client: APIClient(baseURL: URL(string: "http://127.0.0.1:9")!, accessKey: nil)) },
                                  defaults: UserDefaults(suiteName: "wardrobe-tests-\(UUID().uuidString)")!)
        var meta = TimelineMeta(kind: "avatar_outfit")
        meta.outfit = "maid"
        let old = TimelineItem(id: "1", ts: Date().addingTimeInterval(-3600), channel: .web, role: .lucina, text: "ok", meta: meta)
        store.consider([old])
        XCTAssertNil(store.settings.outfit, "an old item is history")
        let live = TimelineItem(id: "2", ts: Date(), channel: .web, role: .lucina, text: "ok", meta: meta)
        store.consider([live])
        XCTAssertEqual(store.settings.outfit, "maid")
    }
}
