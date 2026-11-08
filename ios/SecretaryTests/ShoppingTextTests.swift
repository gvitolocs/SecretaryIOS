import XCTest
@testable import Secretary

/// ShoppingText: the same cases as crates/shopping-text and web/src/lib/shoppingText.ts. Copied from
/// tests/fixtures/shopping-split-cases.json (tests run on the iPhone, which cannot read the repo);
/// keep them equal to the fixture.
final class ShoppingTextTests: XCTestCase {
    private static let known: [String] = ["patatine fritte","ricotta","redbull","red bull","mortadella","baguettes","baguette","latte","uova","farina","sale e pepe","fish and chips","mælk","æg","caffè","pane"]

    private static let cases: [(String, [ItemPart])] = [
        ("to shopping list patatine fritte, ricotta, redbull, mortadella e 4 baguettes", [ItemPart(name: "patatine fritte", quantity: nil, unit: nil), ItemPart(name: "ricotta", quantity: nil, unit: nil), ItemPart(name: "redbull", quantity: nil, unit: nil), ItemPart(name: "mortadella", quantity: nil, unit: nil), ItemPart(name: "baguettes", quantity: 4, unit: nil)]),
        ("add patatine fritte, ricotta, redbull, mortadella e 4 baguettes to the shopping list", [ItemPart(name: "patatine fritte", quantity: nil, unit: nil), ItemPart(name: "ricotta", quantity: nil, unit: nil), ItemPart(name: "redbull", quantity: nil, unit: nil), ItemPart(name: "mortadella", quantity: nil, unit: nil), ItemPart(name: "baguettes", quantity: 4, unit: nil)]),
        ("Aggiungi alla lista della spesa latte, uova e 2 kg di farina", [ItemPart(name: "latte", quantity: nil, unit: nil), ItemPart(name: "uova", quantity: nil, unit: nil), ItemPart(name: "farina", quantity: 2, unit: "kg")]),
        ("Tilføj mælk og 6 æg til indkøbslisten", [ItemPart(name: "mælk", quantity: nil, unit: nil), ItemPart(name: "æg", quantity: 6, unit: nil)]),
        ("latte", [ItemPart(name: "latte", quantity: nil, unit: nil)]),
        ("sale e pepe", [ItemPart(name: "sale e pepe", quantity: nil, unit: nil)]),
        ("fish and chips, beer", [ItemPart(name: "fish and chips", quantity: nil, unit: nil), ItemPart(name: "beer", quantity: nil, unit: nil)]),
        ("Red Bull", [ItemPart(name: "Red Bull", quantity: nil, unit: nil)]),
        ("4x yogurt", [ItemPart(name: "yogurt", quantity: 4, unit: nil)]),
        ("yogurt x4", [ItemPart(name: "yogurt", quantity: 4, unit: nil)]),
        ("4 x yogurt", [ItemPart(name: "yogurt", quantity: 4, unit: nil)]),
        ("7up", [ItemPart(name: "7up", quantity: nil, unit: nil)]),
        ("500 g burro", [ItemPart(name: "burro", quantity: 500, unit: "g")]),
        ("1.5 l latte", [ItemPart(name: "1.5 l latte", quantity: nil, unit: nil)]),
        ("", []),
        (" , ; ", []),
        ("shopping list", [ItemPart(name: "shopping list", quantity: nil, unit: nil)]),
        ("add", [ItemPart(name: "add", quantity: nil, unit: nil)]),
        ("pane;\nlatte", [ItemPart(name: "pane", quantity: nil, unit: nil), ItemPart(name: "latte", quantity: nil, unit: nil)]),
        ("patatine fritte e", [ItemPart(name: "patatine fritte e", quantity: nil, unit: nil)]),
        ("0 uova", [ItemPart(name: "0 uova", quantity: nil, unit: nil)]),
        ("milk & bread", [ItemPart(name: "milk", quantity: nil, unit: nil), ItemPart(name: "bread", quantity: nil, unit: nil)]),
        ("pane + latte", [ItemPart(name: "pane", quantity: nil, unit: nil), ItemPart(name: "latte", quantity: nil, unit: nil)]),
        ("please add milk please", [ItemPart(name: "milk", quantity: nil, unit: nil)]),
        ("metti nella lista 3 limoni", [ItemPart(name: "limoni", quantity: 3, unit: nil)]),
        ("to the list: eggs, flour", [ItemPart(name: "eggs", quantity: nil, unit: nil), ItemPart(name: "flour", quantity: nil, unit: nil)]),
        ("AGGIUNGI caffè", [ItemPart(name: "caffè", quantity: nil, unit: nil)]),
        ("Coca-Cola, Red Bull", [ItemPart(name: "Coca-Cola", quantity: nil, unit: nil), ItemPart(name: "Red Bull", quantity: nil, unit: nil)]),
        ("12 uova", [ItemPart(name: "uova", quantity: 12, unit: nil)]),
        ("uova (bio)", [ItemPart(name: "uova (bio)", quantity: nil, unit: nil)]),
        ("e", [ItemPart(name: "e", quantity: nil, unit: nil)]),
        ("10000 uova", [ItemPart(name: "10000 uova", quantity: nil, unit: nil)]),
        ("x", [ItemPart(name: "x", quantity: nil, unit: nil)]),
        ("2 kg", [ItemPart(name: "2 kg", quantity: nil, unit: nil)]),
        ("latte e", [ItemPart(name: "latte e", quantity: nil, unit: nil)]),
        ("Buy 2 l of milk and bread", [ItemPart(name: "milk", quantity: 2, unit: "l"), ItemPart(name: "bread", quantity: nil, unit: nil)]),
        ("“latte”", [ItemPart(name: "latte", quantity: nil, unit: nil)]),
        ("pane, pane", [ItemPart(name: "pane", quantity: nil, unit: nil), ItemPart(name: "pane", quantity: nil, unit: nil)]),
        ("husk mælk på listen", [ItemPart(name: "mælk", quantity: nil, unit: nil)]),
        ("item1, item2, item3, item4, item5, item6, item7, item8, item9, item10, item11, item12, item13, item14, item15, item16, item17, item18, item19, item20, item21, item22, item23, item24, item25, item26, item27, item28, item29, item30, item31, item32, item33, item34, item35, item36, item37, item38, item39, item40, item41, item42, item43, item44, item45, item46, item47, item48, item49, item50, item51, item52, item53, item54, item55", [ItemPart(name: "item1", quantity: nil, unit: nil), ItemPart(name: "item2", quantity: nil, unit: nil), ItemPart(name: "item3", quantity: nil, unit: nil), ItemPart(name: "item4", quantity: nil, unit: nil), ItemPart(name: "item5", quantity: nil, unit: nil), ItemPart(name: "item6", quantity: nil, unit: nil), ItemPart(name: "item7", quantity: nil, unit: nil), ItemPart(name: "item8", quantity: nil, unit: nil), ItemPart(name: "item9", quantity: nil, unit: nil), ItemPart(name: "item10", quantity: nil, unit: nil), ItemPart(name: "item11", quantity: nil, unit: nil), ItemPart(name: "item12", quantity: nil, unit: nil), ItemPart(name: "item13", quantity: nil, unit: nil), ItemPart(name: "item14", quantity: nil, unit: nil), ItemPart(name: "item15", quantity: nil, unit: nil), ItemPart(name: "item16", quantity: nil, unit: nil), ItemPart(name: "item17", quantity: nil, unit: nil), ItemPart(name: "item18", quantity: nil, unit: nil), ItemPart(name: "item19", quantity: nil, unit: nil), ItemPart(name: "item20", quantity: nil, unit: nil), ItemPart(name: "item21", quantity: nil, unit: nil), ItemPart(name: "item22", quantity: nil, unit: nil), ItemPart(name: "item23", quantity: nil, unit: nil), ItemPart(name: "item24", quantity: nil, unit: nil), ItemPart(name: "item25", quantity: nil, unit: nil), ItemPart(name: "item26", quantity: nil, unit: nil), ItemPart(name: "item27", quantity: nil, unit: nil), ItemPart(name: "item28", quantity: nil, unit: nil), ItemPart(name: "item29", quantity: nil, unit: nil), ItemPart(name: "item30", quantity: nil, unit: nil), ItemPart(name: "item31", quantity: nil, unit: nil), ItemPart(name: "item32", quantity: nil, unit: nil), ItemPart(name: "item33", quantity: nil, unit: nil), ItemPart(name: "item34", quantity: nil, unit: nil), ItemPart(name: "item35", quantity: nil, unit: nil), ItemPart(name: "item36", quantity: nil, unit: nil), ItemPart(name: "item37", quantity: nil, unit: nil), ItemPart(name: "item38", quantity: nil, unit: nil), ItemPart(name: "item39", quantity: nil, unit: nil), ItemPart(name: "item40", quantity: nil, unit: nil), ItemPart(name: "item41", quantity: nil, unit: nil), ItemPart(name: "item42", quantity: nil, unit: nil), ItemPart(name: "item43", quantity: nil, unit: nil), ItemPart(name: "item44", quantity: nil, unit: nil), ItemPart(name: "item45", quantity: nil, unit: nil), ItemPart(name: "item46", quantity: nil, unit: nil), ItemPart(name: "item47", quantity: nil, unit: nil), ItemPart(name: "item48", quantity: nil, unit: nil), ItemPart(name: "item49", quantity: nil, unit: nil), ItemPart(name: "item50", quantity: nil, unit: nil)]),
    ]

    func testSharedCases() {
        let known = Set(Self.known.map(TextFold.fold))
        XCTAssertGreaterThanOrEqual(Self.cases.count, 40)
        for (input, want) in Self.cases {
            XCTAssertEqual(ShoppingText.splitItems(input) { known.contains(TextFold.fold($0)) }, want, input)
        }
    }

    func testCapAndQuantityBounds() {
        let many = (0..<60).map { "a\($0)" }.joined(separator: ", ")
        XCTAssertEqual(ShoppingText.splitItems(many) { _ in false }.count, ShoppingText.maxParts)
        XCTAssertEqual(ShoppingText.splitItems("9999 uova") { _ in false }.first?.quantity, 9999)
        XCTAssertNil(ShoppingText.splitItems("10000 uova") { _ in false }.first?.quantity)
        XCTAssertEqual(ShoppingText.foldWord("Tilføj"), "tilfoej")
        XCTAssertEqual(ShoppingText.foldWord("indkøbslisten."), "indkoebslisten")
    }
}
