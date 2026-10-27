import XCTest
@testable import Secretary

/// The shopping widget's emoji: the row's own, else the catalogue item's, else its category's.
final class ShoppingEmojiTests: XCTestCase {
    private let table = ShoppingEmojiSection(catalog: try! JSONDecoder().decode(JSONValue.self, from: Data("""
    {"version":1,"lang":"it",
     "sections":[{"id":"bakery","name":"Pane","icon":"🥖","order":1},{"id":"dairy-eggs","name":"Latticini","icon":"🧀","order":2},
                 {"id":"household","name":"Casa","icon":"🧽","order":3},{"id":"other","name":"Altro","icon":"🛒","order":4}],
     "items":[
       {"id":"milk","name":"Latte","names":{"it":"Latte","en":"Milk","da":"Mælk"},"aliases":["latte intero"],"section":"dairy-eggs","icon":"🥛"},
       {"id":"rye-bread","name":"Pane di segale","names":{"en":"Rye bread","da":"Rugbrød"},"aliases":[],"section":"bakery","icon":"🍞"},
       {"id":"candles","name":"Candele","names":{"en":"Candles"},"aliases":[],"section":"household","icon":"🕯️"},
       {"id":"svg-only","name":"Strano","names":{},"aliases":[],"section":"bakery","icon":"svg:svg-only"}
     ]}
    """.utf8)), lang: "it", etag: "\"abc\"", fetchedAt: Date())

    func testOrder() {
        XCTAssertEqual(table.emoji(icon: "🍌", catalogId: "milk", name: "Banane", section: nil), "🍌", "the row's own emoji")
        XCTAssertEqual(table.emoji(icon: nil, catalogId: "milk", name: "Latte", section: nil), "🥛", "its catalogue item")
        XCTAssertEqual(table.emoji(icon: "🛒", catalogId: "candles", name: "Candele", section: nil), "🕯️", "the cart is no answer")
        XCTAssertEqual(table.emoji(icon: "svg:rye-bread", catalogId: nil, name: "x", section: nil), "🍞", "an svg key names an item")
        XCTAssertEqual(table.emoji(icon: nil, catalogId: nil, name: "  latte intero ", section: nil), "🥛", "free text by alias")
        XCTAssertEqual(table.emoji(icon: nil, catalogId: "svg-only", name: "Strano", section: nil), "🥖", "the item's section")
        XCTAssertEqual(table.emoji(icon: nil, catalogId: nil, name: "Spugne", section: "household"), "🧽", "the row's section")
        XCTAssertEqual(table.emoji(icon: nil, catalogId: nil, name: "Pollo", section: "meat-fish"), "🥩", "built-in category")
        XCTAssertEqual(table.emoji(icon: nil, catalogId: nil, name: "Boh", section: "other"), "🛒")
        XCTAssertEqual(ShoppingEmojiSection.empty.emoji(icon: nil, catalogId: "milk", name: "Latte", section: "dairy-eggs"), "🧀",
                       "no catalogue yet: the category")
    }

    func testEmojiDetection() {
        for emoji in ["🍌", "🕯️", "🧑🏽‍🍳", "🇩🇰", "☕"] { XCTAssertTrue(ShoppingEmojiSection.isEmoji(emoji), emoji) }
        for text in ["", "a", "1", "svg:milk", "🍌🍌", "ok"] { XCTAssertFalse(ShoppingEmojiSection.isEmoji(text), text) }
    }

    func testNeededRowsCarryTheirEmoji() throws {
        let rows = try JSONDecoder().decode(JSONValue.self, from: Data("""
        [{"id":"1","catalogId":"milk","name":"Latte","icon":"🥛","status":"needed","position":2},
         {"id":"2","name":"Candele","status":"needed","position":1},
         {"id":"3","name":"Sapone","section":"household","status":"needed","position":3},
         {"id":"4","name":"Pane","status":"recent","position":0}]
        """.utf8))
        let items = WidgetFetcher.neededItems(rows, emoji: table)
        XCTAssertEqual(items.map(\.name), ["Candele", "Latte", "Sapone"])
        XCTAssertEqual(items.map(\.emoji), ["🕯️", "🥛", "🧽"])
    }
}
