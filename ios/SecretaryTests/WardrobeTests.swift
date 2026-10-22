import XCTest
@testable import Secretary

final class WardrobeTests: XCTestCase {
    private func json(_ text: String) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8))
    }

    func testWardrobeKeepsOnlyValidEntries() throws {
        let file = WardrobeFile(json: try json("""
        {"outfits": [
          {"id": "uniform", "name": {"it": "Divisa", "en": "Uniform"}, "glb": "lucina.glb", "thumbnail": "outfit-uniform.webp", "tags": ["uniform"]},
          {"id": "gym", "name": "Gym", "glb": "lucina-gym.glb", "tags": ["sport"]},
          {"id": "gym", "glb": "dup.glb"},
          {"id": "Bad", "glb": "x.glb"},
          {"id": "evil", "glb": "../x.glb"},
          {"id": "maid", "glb": "lucina-maid.glb", "thumbnail": "x.json", "tags": ["maid"]}
        ],
        "hairstyles": [
          {"id": "natural"},
          {"id": "ponytail", "name": {"en": "Ponytail"}, "glb": "lucina-hair-ponytail.glb", "hideTiara": true},
          {"id": "noglb", "name": "x"}
        ],
        "colors": [{"id": "pink", "hex": "#FF8AC8"}, {"id": "red", "hex": "red"}]}
        """))
        XCTAssertEqual(file.outfits.map(\.id), ["uniform", "gym", "maid"])
        XCTAssertEqual(file.outfits[0].names["it"], "Divisa")
        XCTAssertNil(file.outfits[2].thumbnail)
        XCTAssertEqual(file.outfits.map(\.category), [.uniform, .sport, .work])
        XCTAssertEqual(file.hairstyles.map(\.id), ["natural", "ponytail"])
        XCTAssertTrue(file.hairstyles[1].hideTiara)
        XCTAssertEqual(file.colors.map(\.hex), ["#ff8ac8"])
        XCTAssertEqual(Hairstyle.choices([]).map(\.id), ["natural"])
        // The older bare array is the outfits.
        XCTAssertEqual(WardrobeFile(json: try json(#"[{"id": "gym", "glb": "lucina-gym.glb"}]"#)).outfits.map(\.id), ["gym"])
    }

    func testSettingsResolveBySection() throws {
        var s = AvatarSettings(json: try json(#"{"outfit": null, "auto": true, "rules": {"gym": {"outfit": "gym"}, "shopping": "maid"}}"#))
        XCTAssertEqual(s.rules.gym.hair, "ponytail", "a missing hair takes the default")
        XCTAssertEqual(s.rules.shopping, .init(outfit: "maid", hair: "natural"), "an outfit string is a rule")
        XCTAssertEqual(s.wanted(in: .gym), "gym")
        XCTAssertEqual(s.wantedHair(in: .gym), "ponytail")
        XCTAssertEqual(s.wanted(in: .default), "uniform")
        s.auto = false
        XCTAssertEqual(s.wanted(in: .gym), "uniform")
        XCTAssertEqual(s.wantedHair(in: .gym), "natural")
        s.outfit = "maid"
        s.hair = "bun"
        XCTAssertEqual(s.wanted(in: .default), "maid")
        XCTAssertEqual(s.wantedHair(in: .shopping), "bun")
        XCTAssertEqual(AvatarSettings.Section(tab: .gym), .gym)
        XCTAssertEqual(AvatarSettings.Section(tab: .weight), .default)
    }

    func testPatchEncodingKeepsNullAndAbsentApart() throws {
        let encode = { (p: AvatarPatch) in String(data: try APIClient.encoder.encode(p), encoding: .utf8) }
        XCTAssertEqual(try encode(AvatarPatch(outfit: .some("maid"))), #"{"outfit":"maid"}"#)
        XCTAssertEqual(try encode(AvatarPatch(outfit: .some(nil), hair: .some(nil), auto: true)), #"{"auto":true,"hair":null,"outfit":null}"#)
        XCTAssertEqual(try encode(AvatarPatch(hairColor: .some("pink"))), #"{"hairColor":"pink"}"#)
        XCTAssertEqual(try encode(AvatarPatch(rules: ["gym": ["hair": "bun"]])), #"{"rules":{"gym":{"hair":"bun"}}}"#)
        let applied = AvatarPatch(outfit: .some(nil), rules: ["shopping": ["outfit": "chef"], "gym": ["hair": "bun"]])
            .applied(to: AvatarSettings(json: try json(#"{"outfit": "maid"}"#)))
        XCTAssertNil(applied.outfit)
        XCTAssertEqual(applied.rules.shopping.outfit, "chef")
        XCTAssertEqual(applied.rules.gym, .init(outfit: "gym", hair: "bun"))
    }

    @MainActor
    func testLucinasLiveLookChangeAppliesAtOnce() {
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
        var hairMeta = TimelineMeta(kind: "avatar_outfit")
        hairMeta.hair = "ponytail"
        store.consider([TimelineItem(id: "3", ts: Date(), channel: .web, role: .lucina, text: "ok", meta: hairMeta)])
        XCTAssertEqual(store.settings.hair, "ponytail")
        XCTAssertEqual(store.currentHair, "natural", "not installed: her own hair")
        store.consider([TimelineItem(id: "4", ts: Date(), channel: .web, role: .lucina, text: "ok", meta: TimelineMeta(kind: "avatar_outfit"))])
        XCTAssertNil(store.settings.outfit)
        XCTAssertNil(store.settings.hair)
    }
}
