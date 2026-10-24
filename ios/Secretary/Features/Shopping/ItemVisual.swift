import SwiftUI
import UIKit

/// What a shopping tile shows (docs/design/item-icons.md): a bundled illustration from
/// ItemIcons.xcassets (`item-<catalogId>`, `section-<sectionId>`, synced from assets/icons by
/// ios/scripts/sync-item-icons.sh), an emoji, or the name's first letter. The same order as
/// itemVisual() in web/src/lib/itemArt.ts, so both clients draw a list alike.
enum ItemVisual: Equatable, Sendable {
    /// An asset name in ItemIcons.xcassets.
    case art(String)
    case emoji(String)
    case initial(String)

    static func itemAsset(_ id: String) -> String { "item-\(id)" }
    static func sectionAsset(_ id: String) -> String { "section-\(id)" }

    /// The picture of a row, most specific first, so a list never mixes illustrations with
    /// stray emoji when art exists:
    /// 1. an explicit `svg:<key>` icon;
    /// 2. the art of its catalog item, unless the owner gave the row an emoji of its own;
    /// 3. no catalog id (free text, a custom object): the art of the catalog item its name
    ///    names (`CatalogSearch.guess`), when that item belongs to the row's section, if any;
    /// 4. a specific emoji of the row (not the cart and not just its section's emoji);
    /// 5. the art of its section ("other" excluded);
    /// 6. any remaining emoji other than the cart, else the first letter of the name.
    /// `hasArt` says whether an asset exists (injected so the rule is unit-testable).
    static func resolve(icon rawIcon: String?, catalogId: String?, name: String, section rowSection: String?,
                        catalog: ShoppingCatalog, search: CatalogSearch?, hasArt: (String) -> Bool) -> ItemVisual {
        let trimmed = rawIcon?.trimmingCharacters(in: .whitespacesAndNewlines)
        let given = trimmed?.isEmpty == false ? trimmed : nil
        if let given, given.hasPrefix("svg:") {
            let key = String(given.dropFirst(4))
            if hasArt(itemAsset(key)) { return .art(itemAsset(key)) }
            if hasArt(sectionAsset(key)) { return .art(sectionAsset(key)) }
        }
        let icon = given.flatMap { $0.hasPrefix("svg:") ? nil : $0 }
        var item: CatalogItem?
        if let catalogId {
            item = catalog.item(catalogId)
            let asset = itemAsset(catalogId)
            if hasArt(asset), icon == nil || item == nil || icon == item?.icon { return .art(asset) }
        } else if let guess = search?.guess(name) {
            let fits = rowSection == nil || rowSection == ShoppingCatalog.otherSection || guess.section == nil || guess.section == rowSection
            if fits, hasArt(itemAsset(guess.id)) { return .art(itemAsset(guess.id)) }
        }
        let section = rowSection ?? item?.section
        let sectionEmoji = catalog.section(section)?.icon
        if let icon, icon != ShoppingCatalog.defaultIcon, icon != sectionEmoji { return .emoji(icon) }
        if let section, section != ShoppingCatalog.otherSection, hasArt(sectionAsset(section)) { return .art(sectionAsset(section)) }
        if let icon, icon != ShoppingCatalog.defaultIcon { return .emoji(icon) }
        return .initial(name.trimmingCharacters(in: .whitespacesAndNewlines).first.map { String($0).uppercased() } ?? "?")
    }

    /// A catalog section's picture: its illustration, else its emoji.
    static func section(_ section: CatalogSection, hasArt: (String) -> Bool) -> ItemVisual {
        hasArt(sectionAsset(section.id)) ? .art(sectionAsset(section.id)) : .emoji(section.icon)
    }

    /// Whether ItemIcons.xcassets has the image (UIKit caches the lookups).
    static func bundled(_ asset: String) -> Bool { UIImage(named: asset) != nil }
}

/// An `ItemVisual` at a given icon size: art scaled to fit, an emoji, or a letter in a soft disc.
struct ItemVisualView: View {
    let visual: ItemVisual
    var size: CGFloat = Bring.iconHeight

    var body: some View {
        switch visual {
        case let .art(asset):
            Image(asset)
                .resizable()
                .interpolation(.high)
                .scaledToFit()
                .frame(width: size, height: size)
                .shadow(color: .black.opacity(0.18), radius: 3, y: 2)
        case let .emoji(emoji):
            // The glyph is narrower than its point size: 0.8 of the box matches the art's object.
            Text(emoji)
                .font(.system(size: size * 0.8))
                .frame(width: size, height: size)
        case let .initial(letter):
            Text(letter)
                .font(.system(size: size * 0.42, weight: .bold))
                .frame(width: size * 0.8, height: size * 0.8)
                .background(Circle().fill(.quaternary))
                .overlay(Circle().strokeBorder(.tertiary, lineWidth: 1))
                .frame(width: size, height: size)
        }
    }
}
