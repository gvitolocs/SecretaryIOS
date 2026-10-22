import Foundation
import Observation
import SwiftUI
import UIKit

// Lucina's wardrobe: which outfit her 3D avatar wears (docs/server-api.md, "Avatar
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

    private static func isAsset(_ name: String?, _ exts: [String]) -> Bool {
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

/// `GET /v1/avatar`.
struct AvatarSettings: Codable, Equatable, Sendable {
    struct Rules: Codable, Equatable, Sendable {
        var gym: String
        var shopping: String
        var `default`: String
    }

    var outfit: String?
    var auto: Bool
    var rules: Rules
    var updatedMs: Double
    var updatedBy: String

    static let defaults = AvatarSettings(outfit: nil, auto: true, rules: Rules(gym: "gym", shopping: "chef", default: "uniform"),
                                         updatedMs: 0, updatedBy: "")

    init(outfit: String?, auto: Bool, rules: Rules, updatedMs: Double, updatedBy: String) {
        self.outfit = outfit
        self.auto = auto
        self.rules = rules
        self.updatedMs = updatedMs
        self.updatedBy = updatedBy
    }

    /// Lenient: anything missing or malformed takes the default.
    init(json: JSONValue) {
        let d = Self.defaults
        let id: (JSONValue?) -> String? = { v in v?.stringValue.flatMap { Outfit.isValidId($0) ? $0 : nil } }
        outfit = id(json["outfit"])
        auto = json["auto"]?.boolValue ?? d.auto
        rules = Rules(gym: id(json["rules"]?["gym"]) ?? d.rules.gym,
                      shopping: id(json["rules"]?["shopping"]) ?? d.rules.shopping,
                      default: id(json["rules"]?["default"]) ?? d.rules.default)
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

    func rule(_ section: Section) -> String {
        switch section {
        case .gym: return rules.gym
        case .shopping: return rules.shopping
        case .default: return rules.default
        }
    }

    /// The pin, else the section's rule (auto) or the default rule.
    func wanted(in section: Section) -> String {
        outfit ?? (auto ? rule(section) : rules.default)
    }
}

/// A `PUT /v1/avatar` patch: absent fields keep their value; `.some(nil)` unpins.
struct AvatarPatch: Encodable, Equatable, Sendable {
    var outfit: String??
    var auto: Bool?
    var rules: [String: String]?

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch outfit {
        case .none: break
        case .some(.none): try c.encodeNil(forKey: .outfit)
        case let .some(.some(id)): try c.encode(id, forKey: .outfit)
        }
        try c.encodeIfPresent(auto, forKey: .auto)
        try c.encodeIfPresent(rules, forKey: .rules)
    }

    func applied(to s: AvatarSettings) -> AvatarSettings {
        var s = s
        if let outfit { s.outfit = outfit }
        if let auto { s.auto = auto }
        if let rules {
            if let v = rules["gym"] { s.rules.gym = v }
            if let v = rules["shopping"] { s.rules.shopping = v }
            if let v = rules["default"] { s.rules.default = v }
        }
        return s
    }

    private enum CodingKeys: String, CodingKey { case outfit, auto, rules }
}

struct AvatarAPI: Sendable {
    let client: APIClient

    func settings() async throws -> AvatarSettings {
        AvatarSettings(json: try await client.get("/v1/avatar") as JSONValue)
    }

    func wardrobe() async throws -> [Outfit] {
        Outfit.list(from: try await client.get("/v1/avatar/wardrobe") as JSONValue)
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
    static let cacheKey = "secretary.avatar.v1"
    /// A live `avatar_outfit` item older than this is history, not news.
    static let liveItemSeconds: TimeInterval = 600

    private(set) var settings: AvatarSettings
    private(set) var outfits: [Outfit]
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
            outfits = cached.outfits
        } else {
            settings = .defaults
            outfits = []
        }
    }

    private struct Cache: Codable { let settings: AvatarSettings; let outfits: [Outfit] }

    private func persist() {
        if let data = try? JSONEncoder().encode(Cache(settings: settings, outfits: outfits)) { defaults.set(data, forKey: Self.cacheKey) }
    }

    /// The outfit to wear now: the wanted one, else the default rule, else the first; nil without a wardrobe.
    var current: Outfit? {
        let wanted = settings.wanted(in: section)
        return outfits.first { $0.id == wanted } ?? outfits.first { $0.id == settings.rules.default } ?? outfits.first
    }

    func load(force: Bool = false) async {
        guard !loading, force || Date().timeIntervalSince(lastLoad) > 30 else { return }
        loading = true
        defer { loading = false }
        async let s = try? api().settings()
        async let w = try? api().wardrobe()
        let (newSettings, newOutfits) = await (s, w)
        if let newSettings, !saving { settings = newSettings }
        if let newOutfits { outfits = newOutfits }
        if newSettings != nil || newOutfits != nil {
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
            self.error = apiError.code == "unknown_outfit"
                ? String(localized: "That outfit isn't in the wardrobe any more.")
                : apiError.isConnectivity ? String(localized: "Can't reach the server, so the outfit wasn't saved.")
                : String(localized: "The outfit wasn't saved. Try again.")
            return false
        }
    }

    func wear(_ id: String) async { await update(AvatarPatch(outfit: .some(id))) }

    /// Lucina changed outfit (a timeline item): show it now, then read the truth.
    func consider(_ items: [TimelineItem]) {
        guard let item = items.last(where: { $0.role == .lucina && $0.meta?.kind == "avatar_outfit" }),
              Date().timeIntervalSince(item.ts) < Self.liveItemSeconds else { return }
        if !saving {
            if let id = item.meta?.outfit, Outfit.isValidId(id) {
                settings = AvatarPatch(outfit: .some(id)).applied(to: settings)
            } else {
                settings = AvatarPatch(outfit: .some(nil), auto: true).applied(to: settings)
            }
        }
        Task { await load(force: true) }
    }

    /// A thumbnail, fetched once per session.
    func image(for outfit: Outfit) async -> UIImage? {
        guard let name = outfit.thumbnail else { return nil }
        if let cached = images[name] { return cached }
        guard let data = try? await api().asset(name), let image = UIImage(data: data) else { return nil }
        images[name] = image
        return image
    }
}

// MARK: - Views

/// The tile picture: the thumbnail, or a symbol on a tinted gradient.
struct OutfitArt: View {
    @Environment(WardrobeStore.self) private var store
    let outfit: Outfit
    var compact = false
    @State private var image: UIImage?

    var body: some View {
        ZStack {
            LinearGradient(colors: [outfit.category.tint.opacity(0.35), outfit.category.tint.opacity(0.08)], startPoint: .top, endPoint: .bottom)
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Image(systemName: outfit.category.symbol)
                    .font(compact ? .title3 : .largeTitle)
                    .foregroundStyle(outfit.category.tint)
            }
        }
        .task(id: outfit.thumbnail) { image = await store.image(for: outfit) }
        .accessibilityHidden(true)
    }
}

/// The Wardrobe, after Custom Cast's costume screen: round category tabs over a grid of
/// outfit tiles with a name ribbon; the one she wears is framed and she changes on tap.
/// Lucina stays visible above the sheet as the live preview.
struct WardrobeView: View {
    @Environment(WardrobeStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var category: Outfit.Category?

    private var categories: [Outfit.Category] {
        let present = Set(store.outfits.map(\.category))
        return Outfit.Category.allCases.filter { present.contains($0) }
    }

    private var shown: [Outfit] {
        guard let category else { return store.outfits }
        return store.outfits.filter { $0.category == category }
    }

    var body: some View {
        @Bindable var store = store
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    nowCard
                    Toggle(isOn: Binding(get: { store.settings.auto }, set: { value in Task { await store.update(AvatarPatch(auto: value)) } })) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Change by section").font(.headline)
                            Text(store.settings.outfit != nil ? "A pinned outfit is worn everywhere until you unpin it."
                                 : store.settings.auto ? "Gym clothes in the Gym, her shopping outfit in Shopping, the default everywhere else."
                                 : "She always wears the default outfit.")
                                .font(.footnote).foregroundStyle(.secondary)
                        }
                    }
                    .tint(Theme.accent)
                    .accessibilityIdentifier("wardrobeAuto")

                    if store.outfits.isEmpty {
                        ContentUnavailableView {
                            Label("No outfits yet", systemImage: "hanger")
                        } description: {
                            Text("They appear here once the wardrobe is installed on the server.")
                        }
                    } else {
                        categoryTabs
                        grid
                        rules
                    }
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
            }
            Spacer()
            if store.settings.outfit != nil {
                Button {
                    Task { await store.update(AvatarPatch(outfit: .some(nil))) }
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
            LinearGradient(colors: [.cyan.opacity(0.14), .purple.opacity(0.12), .pink.opacity(0.1)], startPoint: .leading, endPoint: .trailing),
            in: RoundedRectangle(cornerRadius: Theme.cornerRadius, style: .continuous)
        )
        .accessibilityElement(children: .combine)
    }

    private var categoryTabs: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 14) {
                tab(nil, title: "All", symbol: "square.grid.2x2")
                ForEach(categories) { c in tab(c, title: c.title, symbol: c.symbol) }
            }
            .padding(.vertical, 2)
        }
    }

    private func tab(_ c: Outfit.Category?, title: LocalizedStringKey, symbol: String) -> some View {
        let on = category == c
        return Button {
            category = c
        } label: {
            VStack(spacing: 6) {
                Image(systemName: symbol)
                    .font(.title3)
                    .foregroundStyle(on ? .white : .primary)
                    .frame(width: 54, height: 54)
                    .background(on ? AnyShapeStyle(LinearGradient(colors: [.cyan, .indigo, .purple], startPoint: .topLeading, endPoint: .bottomTrailing))
                                : AnyShapeStyle(Theme.card), in: Circle())
                    .shadow(color: on ? .purple.opacity(0.35) : .clear, radius: 8, y: 3)
                Text(title).font(.caption2.weight(.semibold)).textCase(.uppercase).foregroundStyle(on ? Theme.accent : .secondary)
            }
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }

    private var grid: some View {
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 100), spacing: 12)], spacing: 12) {
            ForEach(shown) { outfit in
                let on = store.current?.id == outfit.id
                Button {
                    Task { await store.wear(outfit.id) }
                } label: {
                    OutfitArt(outfit: outfit)
                        .aspectRatio(3 / 4, contentMode: .fit)
                        .overlay(alignment: .top) {
                            Text(outfit.name)
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
                        .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
                        .overlay(
                            RoundedRectangle(cornerRadius: 16, style: .continuous)
                                .strokeBorder(on ? AnyShapeStyle(LinearGradient(colors: [.cyan, .purple, .pink], startPoint: .topLeading, endPoint: .bottomTrailing))
                                              : AnyShapeStyle(Color.primary.opacity(0.08)), lineWidth: on ? 3 : 1)
                        )
                        .shadow(color: on ? .purple.opacity(0.3) : .clear, radius: 10)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text("Put on \(outfit.name)"))
                .accessibilityAddTraits(on ? .isSelected : [])
                .accessibilityIdentifier("outfit-\(outfit.id)")
            }
        }
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
                                let selected = store.settings.rule(section) == outfit.id
                                Button {
                                    Task { await store.update(AvatarPatch(rules: [section.rawValue: outfit.id])) }
                                } label: {
                                    OutfitArt(outfit: outfit, compact: true)
                                        .frame(width: 48, height: 48)
                                        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous)
                                            .strokeBorder(selected ? Theme.accent : .clear, lineWidth: 2.5))
                                        .opacity(selected ? 1 : 0.7)
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel(Text("\(Text(section.title)): \(outfit.name)"))
                                .accessibilityAddTraits(selected ? .isSelected : [])
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
}
