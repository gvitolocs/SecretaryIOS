import XCTest
@testable import Secretary

/// ItemVisual: the same picture order as itemVisual() in web/src/lib/itemArt.ts.
final class ItemVisualTests: XCTestCase {
    private let catalog = ShoppingCatalog(json: try! JSONDecoder().decode(JSONValue.self, from: Data("""
    {"version":1,"lang":"it",
     "sections":[{"id":"bakery","name":"Pane","icon":"🥖","order":1},{"id":"dairy-eggs","name":"Latticini","icon":"🧀","order":2},
                 {"id":"deli","name":"Salumi","icon":"🥪","order":3},{"id":"household","name":"Casa","icon":"🧽","order":4},
                 {"id":"personal-care","name":"Cura","icon":"🧴","order":5},{"id":"other","name":"Altro","icon":"🛒","order":6}],
     "items":[
       {"id":"milk","name":"Latte","names":{"it":"Latte","en":"Milk","da":"Mælk"},"aliases":["latte intero"],"section":"dairy-eggs","icon":"🥛"},
       {"id":"rye-bread","name":"Pane di segale","names":{"en":"Rye bread","da":"Rugbrød"},"aliases":["pane nero"],"section":"bakery","icon":"🍞"},
       {"id":"prosciutto","name":"Prosciutto crudo","names":{"en":"Prosciutto"},"aliases":["crudo"],"section":"deli","icon":"🍖"},
       {"id":"ham","name":"Prosciutto cotto","names":{"en":"Ham"},"aliases":["cotto"],"section":"deli","icon":"🍖"},
       {"id":"candles","name":"Candele","names":{"en":"Candles"},"aliases":[],"section":"household","icon":"🕯️"},
       {"id":"te","name":"Tè","names":{"en":"Tea"},"aliases":[],"section":"dairy-eggs","icon":"🍵"}
     ]}
    """.utf8)))

    /// Art exists for every catalog item and section of the fixture.
    private func hasArt(_ asset: String) -> Bool {
        let ids = Set(catalog.items.map { ItemVisual.itemAsset($0.id) } + catalog.sections.map { ItemVisual.sectionAsset($0.id) })
        return ids.contains(asset)
    }

    private func visual(_ icon: String?, _ catalogId: String? = nil, _ name: String, section: String? = nil,
                        picked: Bool = false, search: Bool = true) -> ItemVisual {
        ItemVisual.resolve(icon: icon, catalogId: catalogId, name: name, section: section, iconPicked: picked, catalog: catalog,
                           search: search ? CatalogSearch(catalog: catalog, lang: "it") : nil, hasArt: hasArt)
    }

    func testCatalogRowsShowTheirOwnArt() {
        XCTAssertEqual(visual("🍖", "prosciutto", "Prosciutto crudo"), .art("item-prosciutto"))
        XCTAssertEqual(visual("🍖", "ham", "Prosciutto cotto"), .art("item-ham"))
        XCTAssertEqual(visual(nil, "milk", "Latte"), .art("item-milk"))
        XCTAssertEqual(visual("svg:candles", nil, "x"), .art("item-candles"))
    }

    func testAnEmojiThePickedOwnerKeeps() {
        XCTAssertEqual(visual("🍒", "milk", "Latte", picked: true), .emoji("🍒"))
        // Not hand-picked: a stored emoji that differs from this catalog's loses to the art.
        XCTAssertEqual(visual("🍒", "milk", "Latte"), .art("item-milk"))
        XCTAssertEqual(visual("🍗", "prosciutto", "Prosciutto crudo"), .art("item-prosciutto"))
    }

    func testFreeTextAndCustomItemsFindArtOnlyByAnExactName() {
        XCTAssertEqual(visual(nil, nil, "Latte"), .art("item-milk"), "exact name")
        XCTAssertEqual(visual(nil, nil, "MÆLK"), .art("item-milk"), "a name in another language")
        XCTAssertEqual(visual(nil, nil, "Latte intero"), .art("item-milk"), "an alias")
    }

    /// docs/design/shopping-labels.md: items are labels; a part of a name never lends its art.
    func testNoArtFromAPartOfTheName() {
        XCTAssertEqual(visual("🍞", nil, "Pane di segale danese", section: "bakery"), .emoji("🍞"))
        XCTAssertEqual(visual(nil, nil, "Prosciutto crudo di Parma"), .initial("P"))
        XCTAssertEqual(visual("🕯️", nil, "Candele profumate", section: "household"), .emoji("🕯️"))
        XCTAssertEqual(visual(nil, nil, "Organic milk"), .initial("O"))
        XCTAssertEqual(visual(nil, nil, "Øko mælk"), .initial("Ø"))
        XCTAssertEqual(visual(nil, nil, "to shopping list patatine fritte, ricotta, redbull, mortadella e 4 baguettes",
                              section: "other"), .initial("T"))
    }

    func testNoArtBorrowedFromAnotherSectionOrTooShortAWord() {
        XCTAssertEqual(visual("🫧", nil, "Latte detergente", section: "personal-care"), .emoji("🫧"))
        XCTAssertEqual(visual("🧴", nil, "Latte detergente", section: "personal-care"), .art("section-personal-care"),
                       "the section's own emoji says no more than the section art")
        XCTAssertEqual(visual(nil, nil, "Te verde"), .initial("T"), "a two-letter word never matches")
    }

    func testFallbacksLookIntentional() {
        XCTAssertEqual(visual("🧯", nil, "Estintore", section: "household"), .emoji("🧯"))
        XCTAssertEqual(visual("🛒", nil, "Qualcosa", section: "household"), .art("section-household"))
        XCTAssertEqual(visual(nil, nil, "qwerty", section: "other"), .initial("Q"))
        XCTAssertEqual(visual("🛒", nil, "  ünbekannt"), .initial("Ü"))
        XCTAssertEqual(visual("🍌", nil, "Banane", search: false), .emoji("🍌"), "before the catalog loads")
    }

    func testSectionPicture() {
        XCTAssertEqual(ItemVisual.section(catalog.sections[0], hasArt: hasArt), .art("section-bakery"))
        XCTAssertEqual(ItemVisual.section(CatalogSection(id: "nope", name: "x", icon: "🔧", order: 9), hasArt: hasArt), .emoji("🔧"))
    }

    /// The app bundle carries the synced illustrations (ios/scripts/sync-item-icons.sh).
    func testBundledArt() {
        XCTAssertTrue(ItemVisual.bundled("item-prosciutto"))
        XCTAssertTrue(ItemVisual.bundled("item-ham"))
        XCTAssertTrue(ItemVisual.bundled("section-deli"))
        XCTAssertFalse(ItemVisual.bundled("item-no-such-thing"))
    }
}
