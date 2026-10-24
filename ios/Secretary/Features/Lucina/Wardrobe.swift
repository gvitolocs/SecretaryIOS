import Foundation
import Observation
import SwiftUI
import UIKit

// Lucina's wardrobe: which outfit, hairstyle and hair colour her 3D avatar wears (docs/server-api.md, "Avatar
// wardrobe"). The server keeps the choice (GET/PUT /v1/avatar) so the web app, this app
// and Lucina agree; UserDefaults only caches it so the stage starts in the right outfit.
// The stage itself (Resources/Stage/stage.html) swaps the model with the change effect.

/// One outfit from `GET /v1/avatar/wardrobe`.
struct Outfit: Codable, Equatable, Hashable, Identifiable, Sendable {
    let id: String
    var names: [String: String]
    let glb: String
    var thumbnail: String?
    var tags: [String]
    var manifest: String?

    init(id: String, names: [String: String] = [:], glb: String, thumbnail: String? = nil, tags: [String] = [], manifest: String? = nil) {
        self.id = id
        self.names = names
        self.glb = glb
        self.thumbnail = thumbnail
        self.tags = tags
        self.manifest = manifest
    }

    static func isValidId(_ id: String) -> Bool {
        id.range(of: #"^[a-z0-9][a-z0-9_-]{0,39}$"#, options: .regularExpression) != nil
    }

    static func isAsset(_ name: String?, _ exts: [String]) -> Bool {
        guard let name, name.range(of: #"^[a-z0-9][a-z0-9._-]{0,127}$"#, options: .regularExpression) != nil, !name.contains("..") else { return false }
        return exts.contains { name.hasSuffix($0) }
    }

    /// Valid entries of an untrusted list (unique ids, order kept).
    static func list(from json: JSONValue) -> [Outfit] {
        guard case let .array(entries) = json else { return [] }
        var seen = Set<String>()
        var out: [Outfit] = []
        for entry in entries {
            guard let id = entry["id"]?.stringValue, isValidId(id), !seen.contains(id),
                  let glb = entry["glb"]?.stringValue, isAsset(glb, [".glb"]) else { continue }
            var names: [String: String] = [:]
            if case let .object(map)? = entry["name"] {
                for (lang, value) in map { if let text = value.stringValue, !text.isEmpty { names[lang] = String(text.prefix(80)) } }
            } else if let text = entry["name"]?.stringValue, !text.isEmpty {
                names["en"] = String(text.prefix(80))
            }
            var tags: [String] = []
            if case let .array(list)? = entry["tags"] { tags = list.compactMap(\.stringValue).filter { !$0.isEmpty && $0.count <= 32 } }
            let thumbnail = entry["thumbnail"]?.stringValue
            let manifest = entry["manifest"]?.stringValue
            out.append(Outfit(id: id, names: names, glb: glb, thumbnail: isAsset(thumbnail, [".png", ".webp"]) ? thumbnail : nil,
                              tags: tags, manifest: isAsset(manifest, [".json"]) ? manifest : nil))
            seen.insert(id)
        }
        return out
    }

    /// Name in the app's language, then English, then any, then the id.
    var name: String {
        let lang = Locale.current.language.languageCode?.identifier ?? "en"
        return names[lang] ?? names["en"] ?? names.values.sorted().first ?? id
    }

    enum Category: String, CaseIterable, Identifiable {
        case uniform, casual, sport, work, formal, fantasy, other
        var id: String { rawValue }

        var title: LocalizedStringKey {
            switch self {
            case .uniform: return "Uniform"
            case .casual: return "Casual"
            case .sport: return "Sport"
            case .work: return "Work"
            case .formal: return "Formal"
            case .fantasy: return "Fantasy"
            case .other: return "Other"
            }
        }

        var symbol: String {
            switch self {
            case .uniform: return "shield.lefthalf.filled"
            case .casual: return "tshirt"
            case .sport: return "dumbbell"
            case .work: return "frying.pan"
            case .formal: return "sparkles"
            case .fantasy: return "wand.and.stars"
            case .other: return "hanger"
            }
        }

        var tint: Color {
            switch self {
            case .uniform: return .indigo
            case .casual: return .teal
            case .sport: return .green
            case .work: return .orange
            case .formal: return .pink
            case .fantasy: return .cyan
            case .other: return .purple
            }
        }
    }

    private static let tagCategory: [String: Category] = [
        "uniform": .uniform, "default": .uniform, "armor": .uniform,
        "casual": .casual, "everyday": .casual, "home": .casual,
        "sport": .sport, "gym": .sport, "fitness": .sport, "training": .sport,
        "work": .work, "chef": .work, "maid": .work, "kitchen": .work,
        "formal": .formal, "dress": .formal, "party": .formal,
        "fantasy": .fantasy, "costume": .fantasy, "cosplay": .fantasy,
    ]

    var category: Category {
        for tag in tags { if let c = Self.tagCategory[tag.lowercased()] { return c } }
        return Self.tagCategory[id] ?? .other
    }
}


/// A hairstyle part from `GET /v1/avatar/wardrobe` (`natural`, her own hair, has no glb).
struct Hairstyle: Codable, Equatable, Hashable, Identifiable, Sendable {
    static let natural = "natural"

    let id: String
    var names: [String: String]
    var glb: String?
    var thumbnail: String?
    var hideTiara: Bool
    var hideMaterials: [String]

    init(id: String, names: [String: String] = [:], glb: String? = nil, thumbnail: String? = nil, hideTiara: Bool = false,
         hideMaterials: [String] = []) {
        self.id = id
        self.names = names
        self.glb = glb
        self.thumbnail = thumbnail
        self.hideTiara = hideTiara
        self.hideMaterials = hideMaterials
    }

    var name: String {
        let lang = Locale.current.language.languageCode?.identifier ?? "en"
        if id == Self.natural && names.isEmpty { return String(localized: "Her own hair") }
        return names[lang] ?? names["en"] ?? names.values.sorted().first ?? id
    }

    static func list(from json: JSONValue?) -> [Hairstyle] {
        guard case let .array(entries)? = json else { return [] }
        var seen = Set<String>()
        var out: [Hairstyle] = []
        for entry in entries {
            guard let id = entry["id"]?.stringValue, Outfit.isValidId(id), !seen.contains(id) else { continue }
            let glb = entry["glb"]?.stringValue
            guard id == natural || Outfit.isAsset(glb, [".glb"]) else { continue }
            let thumb = entry["thumbnail"]?.stringValue
            out.append(Hairstyle(id: id, names: Outfit.names(entry["name"]), glb: id == natural ? nil : glb,
                                 thumbnail: Outfit.isAsset(thumb, [".png", ".webp"]) ? thumb : nil,
                                 hideTiara: entry["hideTiara"]?.boolValue ?? false,
                                 hideMaterials: entry["hideMaterials"]?.arrayValue?.compactMap(\.stringValue) ?? []))
            seen.insert(id)
        }
        return out
    }

    /// Her own hair first (always offered), then the installed styles.
    static func choices(_ list: [Hairstyle]) -> [Hairstyle] {
        [list.first { $0.id == natural } ?? Hairstyle(id: natural)] + list.filter { $0.id != natural }
    }
}

/// A hair colour from `GET /v1/avatar/wardrobe`.
struct HairColor: Codable, Equatable, Hashable, Identifiable, Sendable {
    let id: String
    var names: [String: String]
    /// `#rrggbb`.
    let hex: String

    var name: String {
        let lang = Locale.current.language.languageCode?.identifier ?? "en"
        return names[lang] ?? names["en"] ?? names.values.sorted().first ?? id
    }

    var color: Color {
        let v = Int(hex.dropFirst(), radix: 16) ?? 0
        return Color(red: Double((v >> 16) & 0xff) / 255, green: Double((v >> 8) & 0xff) / 255, blue: Double(v & 0xff) / 255)
    }

    static func list(from json: JSONValue?) -> [HairColor] {
        guard case let .array(entries)? = json else { return [] }
        var seen = Set<String>()
        var out: [HairColor] = []
        for entry in entries {
            guard let id = entry["id"]?.stringValue, Outfit.isValidId(id), !seen.contains(id),
                  let hex = entry["hex"]?.stringValue, hex.range(of: #"^#[0-9a-fA-F]{6}$"#, options: .regularExpression) != nil else { continue }
            out.append(HairColor(id: id, names: Outfit.names(entry["name"]), hex: hex.lowercased()))
            seen.insert(id)
        }
        return out
    }
}

extension Outfit {
    static func names(_ json: JSONValue?) -> [String: String] {
        var names: [String: String] = [:]
        if case let .object(map)? = json {
            for (lang, value) in map { if let text = value.stringValue, !text.isEmpty { names[lang] = String(text.prefix(80)) } }
        } else if let text = json?.stringValue, !text.isEmpty {
            names["en"] = String(text.prefix(80))
        }
        return names
    }
}

/// `GET /v1/avatar/wardrobe`: `{outfits, hairstyles, colors}` (a bare array is the outfits).
struct WardrobeFile: Codable, Equatable, Sendable {
    var outfits: [Outfit] = []
    var hairstyles: [Hairstyle] = []
    var colors: [HairColor] = []

    init(outfits: [Outfit] = [], hairstyles: [Hairstyle] = [], colors: [HairColor] = []) {
        self.outfits = outfits
        self.hairstyles = hairstyles
        self.colors = colors
    }

    init(json: JSONValue) {
        if case .array = json {
            outfits = Outfit.list(from: json)
            return
        }
        outfits = json["outfits"].map(Outfit.list(from:)) ?? []
        hairstyles = Hairstyle.list(from: json["hairstyles"])
        colors = HairColor.list(from: json["colors"])
    }
}

/// `GET /v1/avatar`.
struct AvatarSettings: Codable, Equatable, Sendable {
    struct Rule: Codable, Equatable, Sendable {
        var outfit: String
        var hair: String
    }

    struct Rules: Codable, Equatable, Sendable {
        var gym: Rule
        var shopping: Rule
        var `default`: Rule
    }

    var outfit: String?
    var hair: String?
    var hairColor: String?
    var auto: Bool
    var rules: Rules
    var updatedMs: Double
    var updatedBy: String

    static let defaultRules = Rules(gym: Rule(outfit: "gym", hair: "ponytail"),
                                    shopping: Rule(outfit: "chef", hair: Hairstyle.natural),
                                    default: Rule(outfit: "uniform", hair: Hairstyle.natural))
    static let defaults = AvatarSettings(outfit: nil, hair: nil, hairColor: nil, auto: true, rules: defaultRules, updatedMs: 0, updatedBy: "")

    init(outfit: String?, hair: String?, hairColor: String?, auto: Bool, rules: Rules, updatedMs: Double, updatedBy: String) {
        self.outfit = outfit
        self.hair = hair
        self.hairColor = hairColor
        self.auto = auto
        self.rules = rules
        self.updatedMs = updatedMs
        self.updatedBy = updatedBy
    }

    /// Lenient: anything missing or malformed takes the default; a rule may be an outfit string.
    init(json: JSONValue) {
        let d = Self.defaults
        let id: (JSONValue?) -> String? = { v in v?.stringValue.flatMap { Outfit.isValidId($0) ? $0 : nil } }
        let rule: (JSONValue?, Rule) -> Rule = { v, fallback in
            if let outfit = id(v) { return Rule(outfit: outfit, hair: Hairstyle.natural) }
            return Rule(outfit: id(v?["outfit"]) ?? fallback.outfit, hair: id(v?["hair"]) ?? fallback.hair)
        }
        outfit = id(json["outfit"])
        hair = id(json["hair"])
        hairColor = id(json["hairColor"])
        auto = json["auto"]?.boolValue ?? d.auto
        rules = Rules(gym: rule(json["rules"]?["gym"], d.rules.gym),
                      shopping: rule(json["rules"]?["shopping"], d.rules.shopping),
                      default: rule(json["rules"]?["default"], d.rules.default))
        updatedMs = json["updatedMs"]?.numberValue ?? 0
        updatedBy = json["updatedBy"]?.stringValue ?? ""
    }

    enum Section: String, CaseIterable, Identifiable {
        case gym, shopping, `default`
        var id: String { rawValue }

        var title: LocalizedStringKey {
            switch self {
            case .gym: return "Gym"
            case .shopping: return "Shopping"
            case .default: return "Everywhere else"
            }
        }

        var symbol: String {
            switch self {
            case .gym: return "dumbbell"
            case .shopping: return "cart"
            case .default: return "sparkles"
            }
        }

        init(tab: AppModel.Tab) {
            switch tab {
            case .gym: self = .gym
            case .shopping: self = .shopping
            default: self = .default
            }
        }
    }

    func rule(_ section: Section) -> Rule {
        switch section {
        case .gym: return rules.gym
        case .shopping: return rules.shopping
        case .default: return rules.default
        }
    }

    /// The rule in force: the section's with auto, else the default one.
    func active(_ section: Section) -> Rule { auto ? rule(section) : rules.default }

    /// The pinned outfit, else the rule's.
    func wanted(in section: Section) -> String { outfit ?? active(section).outfit }

    /// The pinned hairstyle, else the rule's.
    func wantedHair(in section: Section) -> String { hair ?? active(section).hair }
}

/// A `PUT /v1/avatar` patch: absent fields keep their value; `.some(nil)` unpins.
struct AvatarPatch: Encodable, Equatable, Sendable {
    var outfit: String??
    var hair: String??
    var hairColor: String??
    var auto: Bool?
    /// Per section: `["outfit": id]` and/or `["hair": id]`.
    var rules: [String: [String: String]]?

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        for (value, key) in [(outfit, CodingKeys.outfit), (hair, .hair), (hairColor, .hairColor)] {
            switch value {
            case .none: break
            case .some(.none): try c.encodeNil(forKey: key)
            case let .some(.some(id)): try c.encode(id, forKey: key)
            }
        }
        try c.encodeIfPresent(auto, forKey: .auto)
        try c.encodeIfPresent(rules, forKey: .rules)
    }

    func applied(to s: AvatarSettings) -> AvatarSettings {
        var s = s
        if let outfit { s.outfit = outfit }
        if let hair { s.hair = hair }
        if let hairColor { s.hairColor = hairColor }
        if let auto { s.auto = auto }
        for (section, patch) in rules ?? [:] {
            func update(_ rule: inout AvatarSettings.Rule) {
                if let o = patch["outfit"] { rule.outfit = o }
                if let h = patch["hair"] { rule.hair = h }
            }
            switch section {
            case "gym": update(&s.rules.gym)
            case "shopping": update(&s.rules.shopping)
            case "default": update(&s.rules.default)
            default: break
            }
        }
        return s
    }

    private enum CodingKeys: String, CodingKey { case outfit, hair, hairColor, auto, rules }
}

struct AvatarAPI: Sendable {
    let client: APIClient

    func settings() async throws -> AvatarSettings {
        AvatarSettings(json: try await client.get("/v1/avatar") as JSONValue)
    }

    func wardrobe() async throws -> WardrobeFile {
        WardrobeFile(json: try await client.get("/v1/avatar/wardrobe") as JSONValue)
    }

    func update(_ patch: AvatarPatch) async throws -> AvatarSettings {
        let value: JSONValue = try await client.send("PUT", "/v1/avatar", body: patch, headers: ["X-Secretary-Client": "ios"])
        return AvatarSettings(json: value)
    }

    /// Thumbnails are private assets: fetched with the device token.
    func asset(_ name: String) async throws -> Data {
        try await client.send("GET", "/v1/assets/\(name)", body: nil, timeout: 30).data
    }
}

@Observable
@MainActor
final class WardrobeStore {
    static let cacheKey = "secretary.avatar.v2"
    /// A live `avatar_outfit` item older than this is history, not news.
    static let liveItemSeconds: TimeInterval = 600

    private(set) var settings: AvatarSettings
    private(set) var wardrobe: WardrobeFile
    private(set) var loading = false
    private(set) var saving = false
    var error: String?
    /// Which section is on screen (from the selected tab).
    var section: AvatarSettings.Section = .default
    var presented = false

    @ObservationIgnored private let api: () -> AvatarAPI
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var writeSeq = 0
    @ObservationIgnored private var lastLoad = Date.distantPast
    @ObservationIgnored private var images: [String: UIImage] = [:]

    init(api: @escaping () -> AvatarAPI, defaults: UserDefaults = .standard) {
        self.api = api
        self.defaults = defaults
        if let data = defaults.data(forKey: Self.cacheKey), let cached = try? JSONDecoder().decode(Cache.self, from: data) {
            settings = cached.settings
            wardrobe = cached.wardrobe
        } else {
            settings = .defaults
            wardrobe = WardrobeFile()
        }
    }

    private struct Cache: Codable { let settings: AvatarSettings; let wardrobe: WardrobeFile }

    private func persist() {
        if let data = try? JSONEncoder().encode(Cache(settings: settings, wardrobe: wardrobe)) { defaults.set(data, forKey: Self.cacheKey) }
    }

    var outfits: [Outfit] { wardrobe.outfits }
    var hairstyles: [Hairstyle] { Hairstyle.choices(wardrobe.hairstyles) }
    var colors: [HairColor] { wardrobe.colors }

    /// The outfit to wear now: the wanted one, else the default rule's, else the first; nil without a wardrobe.
    var current: Outfit? {
        let wanted = settings.wanted(in: section)
        return outfits.first { $0.id == wanted } ?? outfits.first { $0.id == settings.rules.default.outfit } ?? outfits.first
    }

    /// The hairstyle to show now: the wanted one when installed, else her own hair.
    var currentHair: String {
        let wanted = settings.wantedHair(in: section)
        return wardrobe.hairstyles.contains { $0.id == wanted && $0.glb != nil } ? wanted : Hairstyle.natural
    }

    /// The hair colour to show now (nil: natural).
    var currentHairColor: String? {
        guard let id = settings.hairColor, colors.contains(where: { $0.id == id }) else { return nil }
        return id
    }

    func load(force: Bool = false) async {
        guard !loading, force || Date().timeIntervalSince(lastLoad) > 30 else { return }
        loading = true
        defer { loading = false }
        async let s = try? api().settings()
        async let w = try? api().wardrobe()
        let (newSettings, newWardrobe) = await (s, w)
        if let newSettings, !saving { settings = newSettings }
        if let newWardrobe { wardrobe = newWardrobe }
        if newSettings != nil || newWardrobe != nil {
            lastLoad = Date()
            persist()
        }
    }

    /// Optimistic: the stage changes at once; a refused write puts it back.
    @discardableResult
    func update(_ patch: AvatarPatch) async -> Bool {
        let before = settings
        writeSeq += 1
        let seq = writeSeq
        settings = patch.applied(to: before)
        error = nil
        saving = true
        defer { if seq == writeSeq { saving = false } }
        do {
            let saved = try await api().update(patch)
            if seq == writeSeq { settings = saved }
            persist()
            return true
        } catch {
            guard seq == writeSeq else { return false }
            settings = before
            let apiError = APIError.wrap(error)
            self.error = apiError.code.hasPrefix("unknown_")
                ? String(localized: "That outfit isn't in the wardrobe any more.")
                : apiError.isConnectivity ? String(localized: "Can't reach the server, so the outfit wasn't saved.")
                : String(localized: "The outfit wasn't saved. Try again.")
            return false
        }
    }

    func wear(_ id: String) async { await update(AvatarPatch(outfit: .some(id))) }
    func style(_ hair: String) async { await update(AvatarPatch(hair: .some(hair))) }
    func tint(_ color: String?) async { await update(AvatarPatch(hairColor: .some(color))) }

    /// Lucina changed her look (a timeline item): show it now, then read the truth.
    func consider(_ items: [TimelineItem]) {
        guard let item = items.last(where: { $0.role == .lucina && $0.meta?.kind == "avatar_outfit" }),
              Date().timeIntervalSince(item.ts) < Self.liveItemSeconds else { return }
        if !saving {
            var patch = AvatarPatch()
            if let id = item.meta?.outfit, Outfit.isValidId(id) { patch.outfit = .some(id) }
            if let id = item.meta?.hair, Outfit.isValidId(id) { patch.hair = .some(id) }
            if patch == AvatarPatch() { patch = AvatarPatch(outfit: .some(nil), hair: .some(nil), auto: true) }
            settings = patch.applied(to: settings)
        }
        Task { await load(force: true) }
    }

    /// A thumbnail, fetched once per session.
    func image(named name: String?) async -> UIImage? {
        guard let name else { return nil }
        if let cached = images[name] { return cached }
        guard let data = try? await api().asset(name), let image = UIImage(data: data) else { return nil }
        images[name] = image
        return image
    }
}

// MARK: - Views

/// The tile picture: the thumbnail, or a symbol on a tinted gradient.
struct TileArt: View {
    @Environment(WardrobeStore.self) private var store
    let thumbnail: String?
    let symbol: String
    let tint: Color
    var compact = false
    @State private var image: UIImage?

    var body: some View {
        ZStack {
            LinearGradient(colors: [tint.opacity(0.35), tint.opacity(0.08)], startPoint: .top, endPoint: .bottom)
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Image(systemName: symbol)
                    .font(compact ? .title3 : .largeTitle)
                    .foregroundStyle(tint)
            }
        }
        .task(id: thumbnail) { image = await store.image(named: thumbnail) }
        .accessibilityHidden(true)
    }
}

struct OutfitArt: View {
    let outfit: Outfit
    var compact = false

    var body: some View {
        TileArt(thumbnail: outfit.thumbnail, symbol: outfit.category.symbol, tint: outfit.category.tint, compact: compact)
    }
}

struct HairArt: View {
    let hair: Hairstyle
    var compact = false

    var body: some View {
        TileArt(thumbnail: hair.thumbnail, symbol: hair.id == Hairstyle.natural ? "person.crop.circle" : "comb",
                tint: hair.id == Hairstyle.natural ? .cyan : .purple, compact: compact)
    }
}

/// The Wardrobe, after Custom Cast's costume screen: round tabs (Outfit, Hair, Colour) over
/// a grid of tiles with a name ribbon; the one she wears is framed and she changes on tap.
/// Lucina stays visible above the sheet as the live preview.
struct WardrobeView: View {
    enum Kind: String, CaseIterable, Identifiable {
        case outfit, hair, color
        var id: String { rawValue }

        var title: LocalizedStringKey {
            switch self {
            case .outfit: return "Outfit"
            case .hair: return "Hair"
            case .color: return "Colour"
            }
        }

        var symbol: String {
            switch self {
            case .outfit: return "hanger"
            case .hair: return "comb"
            case .color: return "drop"
            }
        }
    }

    @Environment(WardrobeStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var kind: Kind = .outfit
    @State private var category: Outfit.Category?

    private var categories: [Outfit.Category] {
        let present = Set(store.outfits.map(\.category))
        return Outfit.Category.allCases.filter { present.contains($0) }
    }

    private var shown: [Outfit] {
        guard let category else { return store.outfits }
        return store.outfits.filter { $0.category == category }
    }

    private var pinned: Bool { store.settings.outfit != nil || store.settings.hair != nil }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    nowCard
                    Toggle(isOn: Binding(get: { store.settings.auto }, set: { value in Task { await store.update(AvatarPatch(auto: value)) } })) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Change by section").font(.headline)
                            Text(pinned ? "A pinned outfit is worn everywhere until you unpin it."
                                 : store.settings.auto ? "Gym clothes and a ponytail in the Gym, her shopping outfit in Shopping, the default everywhere else."
                                 : "She always wears the default outfit.")
                                .font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                    .tint(Theme.accent)
                    .accessibilityIdentifier("wardrobeAuto")

                    kindTabs
                    switch kind {
                    case .outfit:
                        if store.outfits.isEmpty {
                            ContentUnavailableView {
                                Label("No outfits yet", systemImage: "hanger")
                            } description: {
                                Text("They appear here once the wardrobe is installed on the server.")
                            }
                        } else {
                            if categories.count > 1 { categoryChips }
                            outfitGrid
                        }
                    case .hair:
                        hairGrid
                    case .color:
                        swatches
                    }
                    if !store.outfits.isEmpty { rules }
                    if let error = store.error {
                        Text(error).font(.footnote).foregroundStyle(.red)
                    }
                    Text("You can also ask her: “put on your gym clothes”.")
                        .font(.footnote).foregroundStyle(.secondary).frame(maxWidth: .infinity)
                }
                .padding(16)
            }
            .background(Theme.page)
            .navigationTitle("Wardrobe")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }.accessibilityIdentifier("wardrobeDone")
                }
            }
            .task { await store.load(force: true) }
        }
    }

    private var nowCard: some View {
        HStack(spacing: 12) {
            if let outfit = store.current {
                OutfitArt(outfit: outfit, compact: true)
                    .frame(width: 52, height: 52)
                    .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
            }
            VStack(alignment: .leading, spacing: 2) {
                Text("Wearing").font(.caption2.weight(.semibold)).textCase(.uppercase).tracking(1.2).foregroundStyle(.secondary)
                Text(store.current?.name ?? "—").font(.title3.weight(.semibold))
                HStack(spacing: 6) {
                    Text(store.hairstyles.first { $0.id == store.currentHair }?.name ?? "")
                    if let color = store.colors.first(where: { $0.id == store.currentHairColor }) {
                        Circle().fill(color.color).frame(width: 10, height: 10)
                    }
                }
                .font(.footnote).foregroundStyle(.secondary)
            }
            Spacer()
            if pinned {
                Button {
                    Task { await store.update(AvatarPatch(outfit: .some(nil), hair: .some(nil))) }
                } label: {
                    Label("Pinned", systemImage: "pin.fill")
                        .font(.footnote.weight(.semibold))
                        .padding(.horizontal, 10).padding(.vertical, 6)
                        .background(Color.orange.opacity(0.15), in: Capsule())
                }
                .tint(.orange)
                .accessibilityHint("Unpins the outfit and follows the section again")
                .accessibilityIdentifier("wardrobeUnpin")
            }
        }
        .padding(12)
        .background(
            Theme.accentSoft,
            in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous)
        )
        .accessibilityElement(children: .combine)
    }

    /// Custom Cast style: round buttons, an icon over a short label.
    private var kindTabs: some View {
        HStack(spacing: 18) {
            ForEach(Kind.allCases) { k in
                let on = kind == k
                Button {
                    kind = k
                } label: {
                    VStack(spacing: 6) {
                        Image(systemName: k.symbol)
                            .font(.title3)
                            .foregroundStyle(on ? .white : .primary)
                            .frame(width: 54, height: 54)
                            .background(on ? Theme.accentFill : Theme.card, in: Circle())
                        Text(k.title).font(.caption2.weight(.semibold)).textCase(.uppercase).foregroundStyle(on ? Theme.accent : .secondary)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(on ? [.isSelected, .isButton] : .isButton)
                .accessibilityIdentifier("wardrobeKind-\(k.rawValue)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var categoryChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                chip(nil, title: "All", symbol: "square.grid.2x2")
                ForEach(categories) { c in chip(c, title: c.title, symbol: c.symbol) }
            }
        }
    }

    private func chip(_ c: Outfit.Category?, title: LocalizedStringKey, symbol: String) -> some View {
        let on = category == c
        return Button {
            category = c
        } label: {
            Label(title, systemImage: symbol)
                .font(.footnote.weight(.semibold))
                .padding(.horizontal, 12).padding(.vertical, 7)
                .background(on ? AnyShapeStyle(Theme.accent.opacity(0.18)) : AnyShapeStyle(Theme.card), in: Capsule())
                .overlay(Capsule().strokeBorder(on ? Theme.accent.opacity(0.6) : .clear))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }

    private func tile<Art: View>(_ name: String, on: Bool, label: Text, id: String, action: @escaping () -> Void, @ViewBuilder art: () -> Art) -> some View {
        Button(action: action) {
            art()
                .aspectRatio(3 / 4, contentMode: .fit)
                .overlay(alignment: .top) {
                    Text(name)
                        .font(.caption.weight(.semibold))
                        .lineLimit(1)
                        .padding(.horizontal, 8).padding(.vertical, 4)
                        .frame(maxWidth: .infinity)
                        .background(on ? AnyShapeStyle(Theme.accentFill) : AnyShapeStyle(.regularMaterial), in: Capsule())
                        .foregroundStyle(on ? .white : .primary)
                        .padding(6)
                }
                .overlay(alignment: .bottomTrailing) {
                    if on {
                        Image(systemName: "checkmark.circle.fill")
                            .font(.title3).foregroundStyle(.white, Theme.accentFill)
                            .padding(6)
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile, style: .continuous)
                        .strokeBorder(on ? Theme.accent : Theme.separator, lineWidth: on ? 3 : 1)
                )
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityAddTraits(on ? .isSelected : [])
        .accessibilityIdentifier(id)
    }

    private let columns = [GridItem(.adaptive(minimum: 100), spacing: 12)]

    private var outfitGrid: some View {
        LazyVGrid(columns: columns, spacing: 12) {
            ForEach(shown) { outfit in
                tile(outfit.name, on: store.current?.id == outfit.id, label: Text("Put on \(outfit.name)"), id: "outfit-\(outfit.id)") {
                    Task { await store.wear(outfit.id) }
                } art: {
                    OutfitArt(outfit: outfit)
                }
            }
        }
    }

    private var hairGrid: some View {
        VStack(alignment: .leading, spacing: 10) {
            LazyVGrid(columns: columns, spacing: 12) {
                ForEach(store.hairstyles) { hair in
                    tile(hair.name, on: store.currentHair == hair.id, label: Text("Style her hair: \(hair.name)"), id: "hair-\(hair.id)") {
                        Task { await store.style(hair.id) }
                    } art: {
                        HairArt(hair: hair)
                    }
                }
            }
            if store.hairstyles.count == 1 {
                Text("Hairstyles appear here once they are installed on the server.").font(.footnote).foregroundStyle(.secondary)
            }
        }
    }

    private var swatches: some View {
        VStack(alignment: .leading, spacing: 10) {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 72), spacing: 10)], spacing: 14) {
                swatch(nil, name: String(localized: "Natural"), fill: AnyShapeStyle(AngularGradient(colors: [.indigo, .blue, .indigo], center: .center)))
                ForEach(store.colors) { c in swatch(c.id, name: c.name, fill: AnyShapeStyle(c.color.gradient)) }
            }
            if store.colors.isEmpty {
                Text("Hair colours appear here once they are installed on the server.").font(.footnote).foregroundStyle(.secondary)
            }
        }
    }

    private func swatch(_ id: String?, name: String, fill: AnyShapeStyle) -> some View {
        let on = store.currentHairColor == id
        return Button {
            Task { await store.tint(id) }
        } label: {
            VStack(spacing: 6) {
                Circle().fill(fill)
                    .frame(width: 52, height: 52)
                    .overlay(Circle().strokeBorder(.white.opacity(0.5), lineWidth: 1))
                    .padding(3)
                    .overlay(Circle().strokeBorder(on ? Theme.accent : .clear, lineWidth: 2.5))
                Text(name).font(.caption.weight(.semibold)).foregroundStyle(on ? .primary : .secondary)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Hair colour: \(name)"))
        .accessibilityAddTraits(on ? .isSelected : [])
        .accessibilityIdentifier("hairColor-\(id ?? "natural")")
    }

    private var rules: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Outfit per section").font(.caption2.weight(.semibold)).textCase(.uppercase).tracking(1.2).foregroundStyle(.secondary)
            ForEach(AvatarSettings.Section.allCases) { section in
                VStack(alignment: .leading, spacing: 6) {
                    Label(section.title, systemImage: section.symbol).font(.subheadline.weight(.semibold))
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(store.outfits) { outfit in
                                pick(selected: store.settings.rule(section).outfit == outfit.id,
                                     label: Text("\(Text(section.title)): \(outfit.name)"), round: false) {
                                    Task { await store.update(AvatarPatch(rules: [section.rawValue: ["outfit": outfit.id]])) }
                                } art: { OutfitArt(outfit: outfit, compact: true) }
                            }
                        }
                    }
                    if store.hairstyles.count > 1 {
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack(spacing: 8) {
                                ForEach(store.hairstyles) { hair in
                                    pick(selected: store.settings.rule(section).hair == hair.id,
                                         label: Text("\(Text(section.title)) hair: \(hair.name)"), round: true) {
                                        Task { await store.update(AvatarPatch(rules: [section.rawValue: ["hair": hair.id]])) }
                                    } art: { HairArt(hair: hair, compact: true) }
                                }
                            }
                        }
                    }
                }
                .padding(12)
                .background(Theme.card, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                .opacity(!store.settings.auto && section != .default ? 0.55 : 1)
            }
        }
    }

    private func pick<Art: View>(selected: Bool, label: Text, round: Bool, action: @escaping () -> Void, @ViewBuilder art: () -> Art) -> some View {
        let shape = RoundedRectangle(cornerRadius: round ? 22 : 12, style: .continuous)
        return Button(action: action) {
            art()
                .frame(width: round ? 40 : 48, height: round ? 40 : 48)
                .clipShape(shape)
                .overlay(shape.strokeBorder(selected ? Theme.accent : .clear, lineWidth: 2.5))
                .opacity(selected ? 1 : 0.7)
        }
        .buttonStyle(.plain)
        .accessibilityLabel(label)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }
}
