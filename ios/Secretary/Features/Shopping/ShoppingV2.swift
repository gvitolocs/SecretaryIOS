import Foundation

// Shopping v2 (docs/shopping-v2.md, docs/server-api.md "Shopping v2"): catalog tiles,
// several lists, and an idempotent op log. Pure types and logic live here (decoding,
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

    func displayName(_ lang: String) -> String { names[lang] ?? name }
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

    /// One active row per list and (catalog id, else folded name).
    static func itemKey(catalogId: String?, name: String) -> String {
        if let catalogId, !catalogId.isEmpty { return "c:\(catalogId)" }
        return "n:\(fold(name))"
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
    let lang: String

    init(catalog: ShoppingCatalog, lang: String) {
        self.lang = lang
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
    var catalogId: String?
    var name: String
    var spec: String
    var quantity: Double?
    var unit: String?
    var section: String?
    var icon: String?
    var status: Status
    var position: Double
    var updatedAt: String
    var updatedBy: String

    var isActive: Bool { status != .removed }

    init(id: String, catalogId: String? = nil, name: String, spec: String = "", quantity: Double? = nil, unit: String? = nil,
         section: String? = nil, icon: String? = nil, status: Status = .needed, position: Double = 0,
         updatedAt: String = "", updatedBy: String = "") {
        self.id = id
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
        guard let id, let name = json["name"]?.nonEmptyString else { return nil }
        let status: Status
        switch json["status"]?.stringValue {
        case "needed": status = .needed
        case "recent": status = .recent
        default: status = json["status"] == nil ? .needed : .removed // unknown statuses: like removed
        }
        self.init(id: id, catalogId: json["catalogId"]?.nonEmptyString, name: name, spec: json["spec"]?.stringValue ?? "",
                  quantity: json["quantity"]?.numberValue.flatMap { $0 > 0 ? $0 : nil }, unit: json["unit"]?.nonEmptyString,
                  section: json["section"]?.nonEmptyString, icon: json["icon"]?.nonEmptyString, status: status,
                  position: json["position"]?.numberValue ?? Double(index),
                  updatedAt: json["updatedAt"]?.nonEmptyString ?? "", updatedBy: json["updatedBy"]?.nonEmptyString ?? "")
    }

    static func items(from json: JSONValue?) -> [ShopItem] {
        (json?.arrayValue ?? []).enumerated().compactMap { ShopItem(json: $1, index: $0) }
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
    }

    var opId: String
    var kind: Kind
    var itemId: String?
    var catalogId: String?
    var name: String?
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

    static func add(itemId: String, catalogId: String?, name: String, spec: String? = nil, section: String? = nil) -> ShopOp {
        ShopOp(kind: .add, itemId: itemId, catalogId: catalogId, name: name, spec: spec?.isEmpty == true ? nil : spec, section: section)
    }

    private enum Keys: String, CodingKey {
        case opId, type, itemId, catalogId, name, spec, quantity, unit, section, toListId, position
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        opId = try c.decode(String.self, forKey: .opId)
        kind = try c.decode(Kind.self, forKey: .type)
        itemId = try c.decodeIfPresent(String.self, forKey: .itemId)
        catalogId = try c.decodeIfPresent(String.self, forKey: .catalogId)
        name = try c.decodeIfPresent(String.self, forKey: .name)
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
            // `itemId` on add is the client-chosen id for a new row (contract extension the web client uses too).
            try c.encodeIfPresent(itemId, forKey: .itemId)
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
        case .reorder:
            try c.encodeIfPresent(itemId, forKey: .itemId)
            try c.encode(Int(position ?? 0), forKey: .position)
        }
    }

    /// The row this op targets, if any.
    var target: String? { kind == .clearRecent ? nil : itemId }
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
        var catalog: (String) -> CatalogItem? = { _ in nil }
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
            let hint = op.catalogId.flatMap(context.catalog)
            let name = clip(op.name ?? hint?.name ?? "")
            if name.isEmpty && op.catalogId == nil { return false }
            let key = TextFold.itemKey(catalogId: op.catalogId, name: name)
            if let index = activeIndex(op.itemId)
                ?? items.firstIndex(where: { $0.isActive && TextFold.itemKey(catalogId: $0.catalogId, name: $0.name) == key }) {
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
            let id = op.itemId.flatMap { candidate in items.contains { $0.id == candidate } ? nil : candidate } ?? context.newId()
            items.append(ShopItem(id: id, catalogId: op.catalogId, name: name.isEmpty ? (op.catalogId ?? "") : name,
                                  spec: op.spec.map(clip) ?? "", quantity: op.quantity.flatMap { $0 > 0 ? $0 : nil },
                                  unit: op.unit.flatMap { $0.isEmpty ? nil : clip($0) }, section: op.section ?? hint?.section,
                                  icon: hint?.icon, status: .needed, position: nextPosition(items),
                                  updatedAt: context.now, updatedBy: context.by))
            return true
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
            items[index].name = name
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
