import Foundation

/// A shopping item read defensively from Lucina's loosely typed JSON
/// (`name`, `rawName`, `product` as a string or `{name}`, `quantity`, `status`).
struct ShoppingItem: Identifiable, Equatable, Sendable {
    /// The server id, or a synthetic `local-N` id when the item has none.
    let id: String
    /// False when the server gave no id, so the item cannot be toggled.
    let hasId: Bool
    var name: String
    var quantity: String?
    var category: String?
    /// The raw status string (`needed` / `purchased` on today's server).
    var status: String?
    var purchased: Bool

    static let doneStatuses: Set<String> = ["purchased", "done", "bought", "checked", "complete", "completed", "closed"]

    init(id: String, hasId: Bool = true, name: String, quantity: String? = nil, category: String? = nil,
         status: String?, purchased: Bool) {
        self.id = id
        self.hasId = hasId
        self.name = name
        self.quantity = quantity
        self.category = category
        self.status = status
        self.purchased = purchased
    }

    init?(json: JSONValue, index: Int) {
        guard let object = json.objectValue else { return nil }
        let rawId: String? = ["id", "uuid", "itemId", "_id"].lazy.compactMap { key -> String? in
            switch object[key] {
            case let .string(value) where !value.isEmpty: return value
            case let .number(value): return value.rounded() == value ? String(Int64(value)) : String(value)
            default: return nil
            }
        }.first
        id = rawId ?? "local-\(index)"
        hasId = rawId != nil

        name = object["name"]?.nonEmptyString
            ?? object["rawName"]?.nonEmptyString
            ?? object["product"]?.nonEmptyString
            ?? object["product"]?["name"]?.nonEmptyString
            ?? object["title"]?.nonEmptyString
            ?? "Unnamed item"

        var quantity: String?
        switch object["quantity"] ?? object["qty"] ?? object["amount"] {
        case let .number(value): quantity = JSONValue.number(value).displayText
        case let .string(value): quantity = value.trimmingCharacters(in: .whitespaces).isEmpty ? nil : value.trimmingCharacters(in: .whitespaces)
        default: quantity = nil
        }
        if let q = quantity, let unit = object["unit"]?.nonEmptyString { quantity = "\(q) \(unit)" }
        self.quantity = quantity ?? object["product"]?["quantity"]?.nonEmptyString
        category = object["category"]?.nonEmptyString

        let status = object["status"]?.nonEmptyString
        self.status = status
        if let checked = object["checked"]?.boolValue {
            purchased = checked
        } else if let status {
            purchased = Self.doneStatuses.contains(status.lowercased())
        } else if let bought = object["bought"]?.boolValue {
            purchased = bought
        } else {
            purchased = object["done"]?.boolValue ?? false
        }
    }

    /// Accepts the contract's bare array, or `{items: [...]}` defensively.
    static func list(from value: JSONValue) -> [ShoppingItem] {
        let array = value.arrayValue ?? value["items"]?.arrayValue ?? []
        return array.enumerated().compactMap { ShoppingItem(json: $1, index: $0) }
    }

    /// Merges a PATCH answer (Lucina returns only `{id, product, status}`, with `product`
    /// set to the raw name) into the item we already show, keeping the richer fields.
    func merged(with update: ShoppingItem?) -> ShoppingItem {
        guard let update, update.id == id else { return self }
        var merged = self
        merged.status = update.status ?? status
        merged.purchased = update.purchased
        return merged
    }
}
