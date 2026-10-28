import Foundation

/// The emoji in front of each shopping-widget row, from the app's item catalogue
/// (`GET /v1/shopping/catalog`, the same icons the Shopping tab uses: `ShoppingV2Store.icon(for:)`).
/// Only what the rows need is kept (item and section emoji, the UI language's names), refreshed
/// daily with the catalogue's ETag. A row with nothing else gets its category's emoji, else the cart.
struct ShoppingEmojiSection: WidgetSection {
    static let key = "shoppingEmoji"
    static let defaultEmoji = "🛒"

    /// The catalogue's sections, when it has not been loaded yet (the ids of docs/catalog.md).
    static let categoryEmoji: [String: String] = [
        "produce": "🥦", "bakery": "🥖", "dairy-eggs": "🧀", "deli": "🥪", "meat-fish": "🥩", "frozen": "🧊",
        "pasta-rice-grains": "🍝", "canned-jars": "🥫", "condiments-spices": "🧂", "breakfast-spreads": "🍯",
        "snacks-sweets": "🍫", "drinks": "🥤", "household": "🧽", "personal-care": "🧴", "baby": "🍼", "pets": "🐾",
        "other": "🛒",
    ]

    var fetchedAt: Date
    var lang: String
    var etag: String?
    /// Catalogue item id → its emoji.
    var items: [String: String] = [:]
    /// Catalogue item id → its section id.
    var itemSections: [String: String] = [:]
    /// Section id → its emoji.
    var sections: [String: String] = [:]
    /// A folded name or alias (the UI language) → the item's emoji, for free-text rows.
    var names: [String: String] = [:]

    static let empty = ShoppingEmojiSection(fetchedAt: .distantPast, lang: "")

    init(fetchedAt: Date, lang: String, etag: String? = nil) {
        self.fetchedAt = fetchedAt
        self.lang = lang
        self.etag = etag
    }

    /// From the catalogue JSON (`{version, lang, sections, items}`); bad entries are skipped.
    init(catalog json: JSONValue, lang: String, etag: String?, fetchedAt: Date) {
        self.init(fetchedAt: fetchedAt, lang: lang, etag: etag)
        for section in json["sections"]?.arrayValue ?? [] {
            guard let id = section["id"]?.nonEmptyString, let icon = section["icon"]?.nonEmptyString, Self.isEmoji(icon) else { continue }
            sections[id] = icon
        }
        for item in json["items"]?.arrayValue ?? [] {
            guard let id = item["id"]?.nonEmptyString else { continue }
            if let section = item["section"]?.nonEmptyString { itemSections[id] = section }
            guard let icon = item["icon"]?.nonEmptyString, Self.isEmoji(icon) else { continue }
            items[id] = icon
            var words = [item["name"]?.nonEmptyString, item["names"]?[lang]?.nonEmptyString]
            words += (item["aliases"]?.arrayValue ?? []).map(\.nonEmptyString)
            for word in words.compactMap({ $0 }) where names[Self.fold(word)] == nil { names[Self.fold(word)] = icon }
        }
    }

    /// A row's emoji: its own, else its catalogue item's (its id, an `svg:<id>` icon, its name),
    /// else its section's, else the cart.
    func emoji(icon rawIcon: String?, catalogId: String?, name: String, section: String?) -> String {
        let icon = rawIcon?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let icon, icon != Self.defaultEmoji, Self.isEmoji(icon) { return icon }
        let key = icon.flatMap { $0.hasPrefix("svg:") ? String($0.dropFirst(4)) : nil }
        for id in [catalogId, key].compactMap({ $0 }) {
            if let emoji = items[id] { return emoji }
        }
        if catalogId == nil, let emoji = names[Self.fold(name)] { return emoji }
        if let section = section ?? catalogId.flatMap({ itemSections[$0] }), section != "other",
           let emoji = sections[section] ?? Self.categoryEmoji[section] {
            return emoji
        }
        return Self.defaultEmoji
    }

    /// The row's illustration in ItemIcons.xcassets, in ItemVisual's order (the widget has no
    /// ItemVisual): an `svg:<key>` icon; its catalogue item's art unless the owner picked another
    /// emoji; else, when the row has no emoji of its own beyond its section's or the cart, its
    /// section's art ("other" excluded). The widget draws it only when the asset exists.
    func art(icon rawIcon: String?, catalogId: String?, section rowSection: String?, picked: Bool) -> String? {
        let icon = rawIcon.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.flatMap { $0.isEmpty ? nil : $0 }
        if let icon, icon.hasPrefix("svg:") { return "item-\(icon.dropFirst(4))" }
        if let catalogId, !(picked && icon != nil && icon != items[catalogId]) { return "item-\(catalogId)" }
        guard let section = rowSection ?? catalogId.flatMap({ itemSections[$0] }), section != "other" else { return nil }
        if let icon, icon != Self.defaultEmoji, icon != (sections[section] ?? Self.categoryEmoji[section]) { return nil }
        return "section-\(section)"
    }

    /// One emoji (with its variation selector, skin tone or joiners), not a word or an `svg:` key.
    static func isEmoji(_ text: String) -> Bool {
        guard text.count == 1, let first = text.unicodeScalars.first else { return false }
        return first.properties.isEmojiPresentation || (first.properties.isEmoji && text.unicodeScalars.count > 1)
    }

    /// Lowercased, without diacritics or surrounding spaces (names match exactly, nothing fuzzy).
    static func fold(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines).folding(options: [.caseInsensitive, .diacriticInsensitive], locale: nil)
    }
}
