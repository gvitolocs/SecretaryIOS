import Foundation

// Shopping v2 (docs/shopping-v2.md, docs/server-api.md "Shopping v2"): catalog tiles,
// several lists, and an idempotent op log. Every row points to an item object (a catalog
// item or a custom object of the owner, with names in it/en/da); rows show the object's
// name in the UI language. Pure types and logic live here (decoding,
// text folding, catalog search, the local copy of the op semantics and the 409 rebase),
// mirroring web/src/lib/shopping{Catalog,Ops}.ts so both clients behave the same.

// MARK: - Catalog

struct CatalogSection: Codable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    let name: String
    let icon: String
    let order: Int
}

struct CatalogItem: Codable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    let name: String
    let names: [String: String]
    let aliases: [String]
    let section: String?
    let icon: String
    /// A custom item object of the owner (`GET /v1/shopping/items`), not a catalog entry.
    var custom: Bool? = nil

    func displayName(_ lang: String) -> String { names[lang] ?? name }

    var isCustom: Bool { custom == true }

    /// A custom object (`{id, kind: "custom", names, aliases, section, icon}`) as a search entry.
    static func custom(json: JSONValue, lang: String) -> CatalogItem? {
        guard json["kind"]?.stringValue == "custom", let id = json["id"]?.nonEmptyString else { return nil }
        var names: [String: String] = [:]
        for (key, value) in json["names"]?.objectValue ?? [:] { if let text = value.nonEmptyString { names[key] = text } }
        guard let name = names[lang] ?? names["it"] ?? names["en"] ?? names["da"] else { return nil }
        return CatalogItem(id: id, name: name, names: names, aliases: (json["aliases"]?.arrayValue ?? []).compactMap(\.nonEmptyString),
                           section: json["section"]?.nonEmptyString, icon: json["icon"]?.nonEmptyString ?? ShoppingCatalog.defaultIcon,
                           custom: true)
    }
}

struct ShoppingCatalog: Codable, Equatable, Sendable {
    var version: Int
    var lang: String
    var sections: [CatalogSection]
    var items: [CatalogItem]

    static let empty = ShoppingCatalog(version: 0, lang: "it", sections: [], items: [])
    static let otherSection = "other"
    static let defaultIcon = "🛒"

    init(version: Int, lang: String, sections: [CatalogSection], items: [CatalogItem]) {
        self.version = version
        self.lang = lang
        self.sections = sections
        self.items = items
    }

    /// Defensive parse of `GET /v1/shopping/catalog`: bad entries are skipped.
    init(json: JSONValue) {
        let sections: [CatalogSection] = (json["sections"]?.arrayValue ?? []).enumerated().compactMap { index, raw in
            guard let id = raw["id"]?.nonEmptyString else { return nil }
            let order = raw["order"]?.numberValue.map { Int($0) } ?? index + 1
            return CatalogSection(id: id, name: raw["name"]?.nonEmptyString ?? id, icon: raw["icon"]?.nonEmptyString ?? "", order: order)
        }
        let items: [CatalogItem] = (json["items"]?.arrayValue ?? []).compactMap { raw in
            guard let id = raw["id"]?.nonEmptyString, let name = raw["name"]?.nonEmptyString else { return nil }
            var names: [String: String] = [:]
            for (key, value) in raw["names"]?.objectValue ?? [:] { if let text = value.nonEmptyString { names[key] = text } }
            return CatalogItem(id: id, name: name, names: names,
                               aliases: (raw["aliases"]?.arrayValue ?? []).compactMap(\.nonEmptyString),
                               section: raw["section"]?.nonEmptyString, icon: raw["icon"]?.nonEmptyString ?? Self.defaultIcon)
        }
        self.init(version: json["version"]?.numberValue.map { Int($0) } ?? 1, lang: json["lang"]?.nonEmptyString ?? "it",
                  sections: sections.sorted { $0.order < $1.order }, items: items)
    }

    func item(_ id: String?) -> CatalogItem? {
        guard let id else { return nil }
        return items.first { $0.id == id }
    }

    func section(_ id: String?) -> CatalogSection? {
        guard let id else { return nil }
        return sections.first { $0.id == id }
    }
}

enum TextFold {
    /// Special letters folded before diacritics are stripped (docs/catalog.md, "Search normalisation"),
    /// so "Rugbrød" is "rugbroed" and "Blåbær" is "blaabaer", as on the server and the web.
    private static let special: [Character: String] = [
        "æ": "ae", "ø": "oe", "å": "aa", "œ": "oe", "ß": "ss", "ð": "d", "þ": "th", "ł": "l", "đ": "d", "ı": "i",
    ]

    /// Lowercase, fold special letters, strip diacritics, collapse punctuation and spaces.
    static func fold(_ text: String) -> String {
        var mapped = ""
        for character in text.lowercased() {
            if let replacement = special[character] { mapped += replacement } else { mapped.append(character) }
        }
        let lowered = mapped.folding(options: [.diacriticInsensitive], locale: nil)
        var out = ""
        var pendingSpace = false
        for character in lowered {
            for scalarCharacter in String(character) {
                if scalarCharacter.isLetter || scalarCharacter.isNumber || scalarCharacter == "%" {
                    if pendingSpace && !out.isEmpty { out.append(" ") }
                    pendingSpace = false
                    out.append(scalarCharacter)
                } else {
                    pendingSpace = true
                }
            }
        }
        return out
    }

    /// One active row per list and (item object id, else folded name).
    static func itemKey(catalogId: String?, name: String) -> String {
        if let catalogId, !catalogId.isEmpty { return "c:\(catalogId)" }
        return "n:\(fold(name))"
    }

    static func isUUID(_ text: String?) -> Bool {
        guard let text else { return false }
        return text.count == 36 && UUID(uuidString: text) != nil
    }
}

/// Local catalog search: exact, prefix and word-start matches on the name first, then the
/// same on aliases (other languages count as aliases), then substrings. Ties go to the
/// boost (recently used), then the shorter name, then the alphabet.
struct CatalogSearch: Sendable {
    private struct Entry: Sendable {
        let item: CatalogItem
        let name: String
        let aliases: [String]
    }

    private let entries: [Entry]
    /// Folded names in every language, then folded aliases -> item (first item wins).
    private let byName: [String: CatalogItem]
    private let byAlias: [String: CatalogItem]
    private let byId: [String: CatalogItem]
    let lang: String

    /// The catalog plus the owner's custom objects (`custom`), in `lang`.
    init(catalog: ShoppingCatalog, lang: String, custom: [CatalogItem] = []) {
        self.lang = lang
        var catalog = catalog
        catalog.items += custom.filter { item in !catalog.items.contains { $0.id == item.id } }
        var byId: [String: CatalogItem] = [:]
        for item in catalog.items where byId[item.id] == nil { byId[item.id] = item }
        self.byId = byId
        var byName: [String: CatalogItem] = [:]
        var byAlias: [String: CatalogItem] = [:]
        for item in catalog.items {
            for name in [item.name] + Array(item.names.values) {
                let key = TextFold.fold(name)
                if !key.isEmpty, byName[key] == nil { byName[key] = item }
            }
        }
        for item in catalog.items {
            for alias in item.aliases {
                let key = TextFold.fold(alias)
                if !key.isEmpty, byAlias[key] == nil { byAlias[key] = item }
            }
        }
        self.byName = byName
        self.byAlias = byAlias
        entries = catalog.items.map { item in
            let primary = TextFold.fold(item.displayName(lang))
            var aliases = item.aliases.map(TextFold.fold)
            aliases += item.names.values.map(TextFold.fold)
            aliases.append(TextFold.fold(item.name))
            var seen = Set<String>()
            let unique = aliases.filter { !$0.isEmpty && $0 != primary && seen.insert($0).inserted }
            return Entry(item: item, name: primary, aliases: unique)
        }
    }

    private static func words(_ text: String) -> [Substring] { text.split(separator: " ") }

    /// Lower is better; nil is no match.
    private static func tier(_ text: String, _ query: String, _ queryWords: [Substring]) -> Int? {
        if text == query { return 0 }
        if text.hasPrefix(query) { return 1 }
        let textWords = words(text)
        if textWords.contains(where: { $0.hasPrefix(query) }) { return 2 }
        if queryWords.count > 1, queryWords.allSatisfy({ word in textWords.contains { $0.hasPrefix(word) } }) { return 3 }
        if text.contains(query) { return 6 }
        return nil
    }

    func search(_ text: String, limit: Int = 24, boost: (String) -> Double = { _ in 0 }) -> [CatalogItem] {
        let query = TextFold.fold(text)
        guard !query.isEmpty else { return [] }
        let queryWords = Self.words(query)
        var hits: [(item: CatalogItem, tier: Int, boost: Double, name: String)] = []
        for entry in entries {
            var best = Self.tier(entry.name, query, queryWords)
            for alias in entry.aliases {
                guard let t = Self.tier(alias, query, queryWords) else { continue }
                let shifted = t == 6 ? 7 : t + 3
                if best == nil || shifted < best! { best = shifted }
            }
            guard let best else { continue }
            hits.append((entry.item, best, boost(entry.item.id), entry.name))
        }
        hits.sort { a, b in
            if a.tier != b.tier { return a.tier < b.tier }
            if a.boost != b.boost { return a.boost > b.boost }
            if a.name.count != b.name.count { return a.name.count < b.name.count }
            return a.name < b.name
        }
        return hits.prefix(limit).map(\.item)
    }

    /// A catalog item or custom object by id.
    func item(_ id: String?) -> CatalogItem? {
        guard let id else { return nil }
        return byId[id]
    }

    /// The catalog item whose folded name, else alias, equals the text.
    func exactMatch(_ text: String) -> CatalogItem? {
        let query = TextFold.fold(text)
        guard !query.isEmpty else { return nil }
        return entries.first { $0.name == query }?.item ?? entries.first { $0.aliases.contains(query) }?.item
    }

    /// The catalog item for a row with no catalog id (added before the catalog, or served by an
    /// old Lucina): a name in any language (`exact`), else an alias, after folding. No plural or
    /// prefix guessing; the same rule as Lucina's read path and the web client.
    func resolve(_ name: String) -> (item: CatalogItem, exact: Bool)? {
        let query = TextFold.fold(name)
        guard !query.isEmpty else { return nil }
        if let item = byName[query] { return (item, true) }
        if let item = byAlias[query] { return (item, false) }
        return nil
    }

    /// The catalog item (never a custom object) whose picture a free-text row shows: only one
    /// whose name or alias the text is exactly, after folding. Never a part of the text
    /// ("Prosciutto crudo di Parma" has no prosciutto art): items are labels
    /// (docs/design/shopping-labels.md). Mirrors itemVisual in web/src/lib/itemArt.ts.
    func pictureItem(_ name: String) -> CatalogItem? {
        let query = TextFold.fold(name)
        guard !query.isEmpty else { return nil }
        let catalogOnly = { (item: CatalogItem?) -> CatalogItem? in item.flatMap { $0.isCustom ? nil : $0 } }
        return catalogOnly(byName[query]) ?? catalogOnly(byAlias[query])
    }
}

// MARK: - Lists and items

enum ListTheme: String, Codable, CaseIterable, Sendable {
    case home, grocery, work, holiday, garden, hardware, baby, other

    var emoji: String {
        switch self {
        case .home: return "🏠"
        case .grocery: return "🛒"
        case .work: return "💼"
        case .holiday: return "🏖️"
        case .garden: return "🌱"
        case .hardware: return "🔧"
        case .baby: return "🍼"
        case .other: return "📝"
        }
    }

    var title: String {
        switch self {
        case .home: return String(localized: "Home")
        case .grocery: return String(localized: "Groceries")
        case .work: return String(localized: "Work")
        case .holiday: return String(localized: "Holiday")
        case .garden: return String(localized: "Garden")
        case .hardware: return String(localized: "Hardware")
        case .baby: return String(localized: "Baby")
        case .other: return String(localized: "Other")
        }
    }
}

struct ShopList: Codable, Identifiable, Equatable, Hashable, Sendable {
    let id: String
    var name: String
    var theme: ListTheme
    var needed: Int
    var recent: Int
    var updatedAt: String?

    static let defaultId = "default"

    init(id: String, name: String, theme: ListTheme, needed: Int = 0, recent: Int = 0, updatedAt: String? = nil) {
        self.id = id
        self.name = name
        self.theme = theme
        self.needed = needed
        self.recent = recent
        self.updatedAt = updatedAt
    }

    init?(json: JSONValue) {
        guard let id = json["id"]?.nonEmptyString else { return nil }
        func count(_ value: JSONValue?) -> Int { max(0, Int(value?.numberValue ?? 0)) }
        self.init(id: id, name: json["name"]?.nonEmptyString ?? id, theme: ListTheme(rawValue: json["theme"]?.stringValue ?? "") ?? .other,
                  needed: count(json["needed"]), recent: count(json["recent"]), updatedAt: json["updatedAt"]?.nonEmptyString)
    }

    /// A bare array, or `{lists: [...]}` defensively.
    static func lists(from json: JSONValue) -> [ShopList] {
        (json.arrayValue ?? json["lists"]?.arrayValue ?? []).compactMap(ShopList.init(json:))
    }
}

struct ShopItem: Codable, Identifiable, Equatable, Hashable, Sendable {
    enum Status: String, Codable, Sendable { case needed, recent, removed }

    let id: String
    /// The item object (`item.id`): a catalog id or `u_<ULID>`; nil until the server named it.
    var objectId: String?
    /// `catalog` or `custom`.
    var kind: String?
    /// The object's names by language (it, en, da).
    var names: [String: String]?
    /// The text as typed.
    var typedName: String?
    var needsTranslation: Bool?
    var catalogId: String?
    /// The server's name; views get the UI-language name from `ShoppingV2Store.display`.
    var name: String
    var spec: String
    var quantity: Double?
    var unit: String?
    var section: String?
    var icon: String?
    /// The server says `icon` is the row's own, hand-picked emoji (else it is the catalog's).
    var iconPicked: Bool?
    var status: Status
    var position: Double
    var updatedAt: String
    var updatedBy: String

    var isActive: Bool { status != .removed }
    var isCustom: Bool { kind == "custom" && objectId != nil }
    /// The identity of the row's object: its object id, else its catalog id.
    var objectKey: String? { objectId ?? catalogId }

    /// The object's name in `lang`, else the server's name.
    func displayName(_ lang: String) -> String { names?[lang] ?? name }

    init(id: String, objectId: String? = nil, kind: String? = nil, names: [String: String]? = nil, typedName: String? = nil,
         needsTranslation: Bool? = nil, catalogId: String? = nil, name: String, spec: String = "", quantity: Double? = nil, unit: String? = nil,
         section: String? = nil, icon: String? = nil, status: Status = .needed, position: Double = 0,
         updatedAt: String = "", updatedBy: String = "") {
        self.id = id
        self.objectId = objectId ?? catalogId
        self.kind = kind ?? (catalogId != nil ? "catalog" : nil)
        self.names = names
        self.typedName = typedName
        self.needsTranslation = needsTranslation
        self.catalogId = catalogId
        self.name = name
        self.spec = spec
        self.quantity = quantity
        self.unit = unit
        self.section = section
        self.icon = icon
        self.status = status
        self.position = position
        self.updatedAt = updatedAt
        self.updatedBy = updatedBy
    }

    init?(json: JSONValue, index: Int) {
        let id: String?
        switch json["id"] {
        case let .string(value) where !value.isEmpty: id = value
        case let .number(value): id = JSONValue.number(value).displayText
        default: id = nil
        }
        let object = json["item"]
        var names: [String: String] = [:]
        for (key, value) in object?["names"]?.objectValue ?? [:] { if let text = value.nonEmptyString { names[key] = text } }
        guard let id, let name = json["name"]?.nonEmptyString ?? names.values.first else { return nil }
        let status: Status
        switch json["status"]?.stringValue {
        case "needed": status = .needed
        case "recent": status = .recent
        default: status = json["status"] == nil ? .needed : .removed // unknown statuses: like removed
        }
        let kind = object?["kind"]?.stringValue
        self.init(id: id, objectId: json["itemId"]?.nonEmptyString ?? object?["id"]?.nonEmptyString,
                  kind: kind == "catalog" || kind == "custom" ? kind : nil, names: names.isEmpty ? nil : names,
                  typedName: json["typedName"]?.nonEmptyString, needsTranslation: object?["needsTranslation"]?.boolValue,
                  catalogId: json["catalogId"]?.nonEmptyString, name: name, spec: json["spec"]?.stringValue ?? "",
                  quantity: json["quantity"]?.numberValue.flatMap { $0 > 0 ? $0 : nil }, unit: json["unit"]?.nonEmptyString,
                  section: json["section"]?.nonEmptyString, icon: json["icon"]?.nonEmptyString, status: status,
                  position: json["position"]?.numberValue ?? Double(index),
                  updatedAt: json["updatedAt"]?.nonEmptyString ?? "", updatedBy: json["updatedBy"]?.nonEmptyString ?? "")
        iconPicked = json["iconPicked"]?.boolValue
    }

    static func items(from json: JSONValue?) -> [ShopItem] {
        (json?.arrayValue ?? []).enumerated().compactMap { ShopItem(json: $1, index: $0) }
    }

    /// The custom object of a row, as a search entry.
    func customObject(lang: String) -> CatalogItem? {
        guard isCustom, let objectId, let names, !names.isEmpty else { return nil }
        return CatalogItem(id: objectId, name: names[lang] ?? name, names: names, aliases: typedName.map { [$0] } ?? [],
                           section: section, icon: icon ?? ShoppingCatalog.defaultIcon, custom: true)
    }

    var updatedDate: Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: updatedAt) ?? ISO8601DateFormatter().date(from: updatedAt)
    }

    var quantityText: String? {
        guard let quantity else { return nil }
        let number = quantity.rounded() == quantity ? String(Int(quantity)) : String(quantity)
        return [number, unit].compactMap { $0 }.joined(separator: " ")
    }
}

struct ListState: Codable, Equatable, Sendable {
    var list: ShopList
    var revision: Int
    var items: [ShopItem]

    init(list: ShopList, revision: Int, items: [ShopItem]) {
        self.list = list
        self.revision = revision
        self.items = items
    }

    init(json: JSONValue, fallbackId: String) {
        let list = json["list"].flatMap(ShopList.init(json:)) ?? ShopList(id: fallbackId, name: fallbackId, theme: .other)
        self.init(list: list, revision: Int(json["revision"]?.numberValue ?? 0), items: ShopItem.items(from: json["items"]))
    }
}

struct OpsResponse: Equatable, Sendable {
    let revision: Int
    let applied: [String]
    let duplicates: [String]
    let items: [ShopItem]?

    init(json: JSONValue) {
        revision = Int(json["revision"]?.numberValue ?? 0)
        applied = (json["applied"]?.arrayValue ?? []).compactMap(\.stringValue)
        duplicates = (json["duplicates"]?.arrayValue ?? []).compactMap(\.stringValue)
        items = json["items"]?.arrayValue != nil ? ShopItem.items(from: json["items"]) : nil
    }
}

// MARK: - Ops

/// One op (`POST /v1/shopping/lists/{id}/ops`). Encoded in the wire shape, which is also
/// how the outbox is persisted.
struct ShopOp: Codable, Equatable, Sendable {
    enum Kind: String, Codable, Sendable {
        case add, need, purchase, remove
        case setSpec = "set_spec", setQuantity = "set_quantity", setSection = "set_section"
        case rename, move, clearRecent = "clear_recent", reorder
        case editItem = "edit_item"
    }

    var opId: String
    var kind: Kind
    /// The row (most ops), or the item object (`add`, `edit_item`). Old outboxes kept an add's
    /// client row id here (a UUID); see `rowId`.
    var itemId: String?
    /// `add`: client-chosen id of the optimistic row (local; the server picks its own).
    var rowId: String?
    /// `add`: typed text, resolved by the server in any language.
    var text: String?
    var catalogId: String?
    var name: String?
    /// `edit_item`: a custom object's names (any of it, en, da).
    var names: [String: String]?
    /// `edit_item`: one emoji; "" clears it.
    var icon: String?
    var spec: String?
    var quantity: Double?
    var unit: String?
    var section: String?
    var toListId: String?
    var position: Double?

    init(opId: String = UUID().uuidString.lowercased(), kind: Kind, itemId: String? = nil, catalogId: String? = nil, name: String? = nil,
         spec: String? = nil, quantity: Double? = nil, unit: String? = nil, section: String? = nil, toListId: String? = nil,
         position: Double? = nil) {
        self.opId = opId
        self.kind = kind
        self.itemId = itemId
        self.catalogId = catalogId
        self.name = name
        self.spec = spec
        self.quantity = quantity
        self.unit = unit
        self.section = section
        self.toListId = toListId
        self.position = position
    }

    /// Add a known object (catalog item or custom object) on the row `rowId`; legacy fields
    /// keep servers from before item objects working.
    static func add(object: CatalogItem, rowId: String, name: String, spec: String? = nil) -> ShopOp {
        var op = ShopOp(kind: .add, itemId: object.id, catalogId: object.isCustom ? nil : object.id, name: name,
                        spec: spec?.isEmpty == true ? nil : spec, section: object.section)
        op.rowId = rowId
        return op
    }

    /// Add typed text (the server names new objects).
    static func add(text: String, rowId: String) -> ShopOp {
        var op = ShopOp(kind: .add, name: text)
        op.text = text
        op.rowId = rowId
        return op
    }

    static func editItem(_ objectId: String, names: [String: String]? = nil, section: String? = nil, icon: String? = nil) -> ShopOp {
        var op = ShopOp(kind: .editItem, itemId: objectId, section: section)
        op.names = names
        op.icon = icon
        return op
    }

    /// The optimistic row of an add (old outboxes: a UUID in `itemId`).
    var addRowId: String? { rowId ?? (TextFold.isUUID(itemId) ? itemId : nil) }
    /// The object an add names: `itemId` (when not a row UUID), else `catalogId`.
    var addObjectId: String? { (TextFold.isUUID(itemId) ? nil : itemId) ?? catalogId }
    /// The typed text of an add.
    var addText: String { (text ?? name ?? "").trimmingCharacters(in: .whitespacesAndNewlines) }

    private enum Keys: String, CodingKey {
        case opId, type, itemId, rowId, text, catalogId, name, names, icon, spec, quantity, unit, section, toListId, position
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        opId = try c.decode(String.self, forKey: .opId)
        kind = try c.decode(Kind.self, forKey: .type)
        itemId = try c.decodeIfPresent(String.self, forKey: .itemId)
        rowId = try c.decodeIfPresent(String.self, forKey: .rowId)
        text = try c.decodeIfPresent(String.self, forKey: .text)
        catalogId = try c.decodeIfPresent(String.self, forKey: .catalogId)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        names = try c.decodeIfPresent([String: String].self, forKey: .names)
        icon = try c.decodeIfPresent(String.self, forKey: .icon)
        spec = try c.decodeIfPresent(String.self, forKey: .spec)
        quantity = try c.decodeIfPresent(Double.self, forKey: .quantity)
        unit = try c.decodeIfPresent(String.self, forKey: .unit)
        section = try c.decodeIfPresent(String.self, forKey: .section)
        toListId = try c.decodeIfPresent(String.self, forKey: .toListId)
        position = try c.decodeIfPresent(Double.self, forKey: .position)
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        try c.encode(opId, forKey: .opId)
        try c.encode(kind, forKey: .type)
        switch kind {
        case .add:
            // `itemId`: the object; `rowId`: the client's optimistic row (ignored by the server).
            try c.encodeIfPresent(itemId, forKey: .itemId)
            try c.encodeIfPresent(rowId, forKey: .rowId)
            try c.encodeIfPresent(text, forKey: .text)
            try c.encodeIfPresent(catalogId, forKey: .catalogId)
            try c.encodeIfPresent(name, forKey: .name)
            try c.encodeIfPresent(spec, forKey: .spec)
            try c.encodeIfPresent(quantity, forKey: .quantity)
            try c.encodeIfPresent(unit, forKey: .unit)
            try c.encodeIfPresent(section, forKey: .section)
        case .need, .purchase, .remove:
            try c.encodeIfPresent(itemId, forKey: .itemId)
        case .setSpec:
            try c.encodeIfPresent(itemId, forKey: .itemId)
            try c.encode(spec ?? "", forKey: .spec)
        case .setQuantity:
            try c.encodeIfPresent(itemId, forKey: .itemId)
            if let quantity { try c.encode(quantity, forKey: .quantity) } else { try c.encodeNil(forKey: .quantity) }
            if let unit { try c.encode(unit, forKey: .unit) } else { try c.encodeNil(forKey: .unit) }
        case .setSection:
            try c.encodeIfPresent(itemId, forKey: .itemId)
            if let section { try c.encode(section, forKey: .section) } else { try c.encodeNil(forKey: .section) }
        case .rename:
            try c.encodeIfPresent(itemId, forKey: .itemId)
            try c.encodeIfPresent(name, forKey: .name)
        case .move:
            try c.encodeIfPresent(itemId, forKey: .itemId)
            try c.encodeIfPresent(toListId, forKey: .toListId)
        case .clearRecent:
            break
        case .editItem:
            try c.encodeIfPresent(itemId, forKey: .itemId)
            try c.encodeIfPresent(names, forKey: .names)
            try c.encodeIfPresent(section, forKey: .section)
            try c.encodeIfPresent(icon, forKey: .icon)
        case .reorder:
            try c.encodeIfPresent(itemId, forKey: .itemId)
            try c.encode(Int(position ?? 0), forKey: .position)
        }
    }

    /// The row this op targets, if any.
    var target: String? {
        switch kind {
        case .clearRecent, .editItem: return nil
        case .add: return addRowId
        default: return itemId
        }
    }
}

/// The client copy of the server's op semantics, for optimistic UI and outbox rebasing:
/// op-id dedup, one active row per (list, catalog id or folded name), `add` revives a
/// recent row, and a removed row never comes back.
enum ShopOps {
    struct Context {
        var listId: String
        var now: String
        var by: String = "ios"
        var newId: () -> String = { UUID().uuidString.lowercased() }
        /// Catalog items and known custom objects by id.
        var catalog: (String) -> CatalogItem? = { _ in nil }
        /// Typed text to a known object id (any language).
        var resolve: (String) -> String? = { _ in nil }
        /// The UI language (a custom rename edits this language's name).
        var lang: String = "it"
    }

    /// One active row per (list, item object); a row or text with no object yet matches by
    /// folded name in any language.
    static func sameRow(_ item: ShopItem, objectId: String?, name: String) -> Bool {
        let own = item.objectKey
        if let objectId, own == objectId { return true }
        if objectId != nil, own != nil { return false }
        let query = TextFold.fold(name)
        guard !query.isEmpty else { return false }
        let candidates = [item.name, item.typedName ?? ""] + Array((item.names ?? [:]).values)
        return candidates.contains { !$0.isEmpty && TextFold.fold($0) == query }
    }

    struct Result: Equatable {
        var items: [ShopItem]
        var applied: [String]
        var duplicates: [String]
    }

    private static func clip(_ text: String) -> String { String(text.trimmingCharacters(in: .whitespacesAndNewlines).prefix(200)) }

    private static func nextPosition(_ items: [ShopItem]) -> Double {
        (items.filter { $0.status == .needed }.map(\.position).max() ?? 0) + 1
    }

    static func apply(_ items: [ShopItem], _ ops: [ShopOp], context: Context, seen: inout Set<String>) -> Result {
        var out = items
        var applied: [String] = []
        var duplicates: [String] = []
        for op in ops {
            if seen.contains(op.opId) {
                duplicates.append(op.opId)
                continue
            }
            seen.insert(op.opId)
            applyOne(&out, op, context)
            applied.append(op.opId)
        }
        return Result(items: out, applied: applied, duplicates: duplicates)
    }

    static func apply(_ items: [ShopItem], _ ops: [ShopOp], context: Context) -> Result {
        var seen = Set<String>()
        return apply(items, ops, context: context, seen: &seen)
    }

    @discardableResult
    private static func applyOne(_ items: inout [ShopItem], _ op: ShopOp, _ context: Context) -> Bool {
        func activeIndex(_ id: String?) -> Int? {
            guard let id else { return nil }
            return items.firstIndex { $0.id == id && $0.isActive }
        }
        func touch(_ index: Int) {
            items[index].updatedAt = context.now
            items[index].updatedBy = context.by
        }
        switch op.kind {
        case .add:
            let text = clip(op.addText)
            let objectId = op.addObjectId ?? (text.isEmpty ? nil : context.resolve(text))
            let hint = objectId.flatMap(context.catalog)
            let name = hint?.displayName(context.lang) ?? text
            if name.isEmpty && objectId == nil { return false }
            if let index = activeIndex(op.addRowId)
                ?? items.firstIndex(where: { $0.isActive && sameRow($0, objectId: objectId, name: text.isEmpty ? name : text) }) {
                if items[index].status == .recent {
                    items[index].status = .needed
                    items[index].position = nextPosition(items)
                }
                if let spec = op.spec { items[index].spec = clip(spec) }
                if let quantity = op.quantity { items[index].quantity = quantity > 0 ? quantity : nil }
                if let unit = op.unit { items[index].unit = unit.isEmpty ? nil : clip(unit) }
                if let section = op.section { items[index].section = section }
                touch(index)
                return true
            }
            let id = op.addRowId.flatMap { candidate in items.contains { $0.id == candidate } ? nil : candidate } ?? context.newId()
            let kind = hint.map { $0.isCustom ? "custom" : "catalog" } ?? objectId.map { $0.hasPrefix("u_") ? "custom" : "catalog" }
            items.append(ShopItem(id: id, objectId: objectId, kind: kind, names: hint?.names, typedName: text.isEmpty ? name : text,
                                  catalogId: kind == "catalog" ? objectId : nil, name: name.isEmpty ? (objectId ?? "") : name,
                                  spec: op.spec.map(clip) ?? "", quantity: op.quantity.flatMap { $0 > 0 ? $0 : nil },
                                  unit: op.unit.flatMap { $0.isEmpty ? nil : clip($0) }, section: op.section ?? hint?.section,
                                  icon: hint?.icon, status: .needed, position: nextPosition(items),
                                  updatedAt: context.now, updatedBy: context.by))
            return true
        case .editItem:
            var changed = false
            for index in items.indices where items[index].isActive && items[index].objectKey == op.itemId {
                if let edits = op.names {
                    var names = items[index].names ?? [:]
                    for (lang, value) in edits where !clip(value).isEmpty { names[lang] = clip(value) }
                    items[index].names = names
                    if let local = names[context.lang] { items[index].name = local }
                    items[index].needsTranslation = false
                }
                if let section = op.section { items[index].section = section }
                if let icon = op.icon { items[index].icon = icon.isEmpty ? nil : icon }
                touch(index)
                changed = true
            }
            return changed
        case .need:
            guard let index = activeIndex(op.itemId), items[index].status != .needed else { return false }
            items[index].status = .needed
            items[index].position = nextPosition(items)
            touch(index)
            return true
        case .purchase:
            guard let index = activeIndex(op.itemId), items[index].status != .recent else { return false }
            items[index].status = .recent
            touch(index)
            return true
        case .remove, .move:
            guard let index = activeIndex(op.itemId) else { return false }
            if op.kind == .move && op.toListId == context.listId { return false }
            items[index].status = .removed
            touch(index)
            return true
        case .setSpec:
            guard let index = activeIndex(op.itemId) else { return false }
            items[index].spec = clip(op.spec ?? "")
            touch(index)
            return true
        case .setQuantity:
            guard let index = activeIndex(op.itemId) else { return false }
            items[index].quantity = op.quantity.flatMap { $0 > 0 ? $0 : nil }
            items[index].unit = op.unit.flatMap { $0.isEmpty ? nil : clip($0) }
            touch(index)
            return true
        case .setSection:
            guard let index = activeIndex(op.itemId) else { return false }
            items[index].section = op.section
            touch(index)
            return true
        case .rename:
            let name = clip(op.name ?? "")
            guard let index = activeIndex(op.itemId), !name.isEmpty else { return false }
            let target = context.resolve(name)
            if let target, target != items[index].objectKey {
                // Another object's name: the row now shows that object.
                let hint = context.catalog(target)
                items[index].objectId = target
                items[index].kind = hint.map { $0.isCustom ? "custom" : "catalog" }
                items[index].catalogId = hint?.isCustom == false ? target : nil
                items[index].names = hint?.names
                items[index].name = hint?.displayName(context.lang) ?? name
                items[index].typedName = name
                if let icon = hint?.icon { items[index].icon = icon }
            } else if items[index].isCustom {
                var names = items[index].names ?? [:]
                names[context.lang] = name
                items[index].names = names
                items[index].name = name
            } else if target == nil {
                // A catalog row renamed to new text becomes a new custom object (named by the server).
                items[index].objectId = nil
                items[index].kind = nil
                items[index].catalogId = nil
                items[index].names = nil
                items[index].name = name
                items[index].typedName = name
            }
            touch(index)
            return true
        case .reorder:
            guard let index = activeIndex(op.itemId), let position = op.position else { return false }
            items[index].position = position
            touch(index)
            return true
        case .clearRecent:
            var changed = false
            for index in items.indices where items[index].status == .recent {
                items[index].status = .removed
                touch(index)
                changed = true
            }
            return changed
        }
    }

    /// After a 409: give every pending op a new id, and drop ops on rows that no longer
    /// exist and ops that would change nothing any more.
    static func rebase(_ fresh: [ShopItem], _ pending: [ShopOp], context: Context) -> [ShopOp] {
        var items = fresh
        var out: [ShopOp] = []
        for op in pending {
            if op.kind != .add, let target = op.target, !items.contains(where: { $0.id == target && $0.isActive }) { continue }
            var next = op
            next.opId = context.newId()
            let before = signature(items)
            let result = apply(items, [next], context: context)
            guard signature(result.items) != before else { continue }
            items = result.items
            out.append(next)
        }
        return out
    }

    private static func signature(_ items: [ShopItem]) -> [ShopItem] {
        items.map { item in
            var copy = item
            copy.updatedAt = ""
            copy.updatedBy = ""
            return copy
        }
    }
}
