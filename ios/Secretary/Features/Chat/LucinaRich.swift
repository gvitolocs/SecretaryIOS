import SwiftUI

// Lucina message format v1 (docs/lucina-message-format.md): short bubbles, typed
// widgets and buttons. The message comes from the network (chat response or timeline
// meta), so `RichMessage.parse` validates it again and drops what does not fit.

struct RichAction: Codable, Equatable, Hashable, Sendable, Identifiable {
    enum Kind: String, Codable, Sendable { case reply, route, confirm, dismiss, secret }
    let id: String
    let label: String
    let emoji: String?
    let kind: Kind
    /// `reply` and `confirm`: what the button sends (or prefills).
    let text: String?
    let prefill: Bool
    /// `route`: an app path such as `/shopping` or `/notes/<slug>`.
    let path: String?
    /// `confirm`: `yes`, `no` or `edit`.
    let answer: String?
    /// `secret`: names only, never a value (SecretCard.swift).
    var secret: SecretAction?

    /// Sends a message right away (not a prefill, route or dismiss).
    var sendsMessage: Bool {
        switch kind {
        case .reply: return !prefill
        case .confirm: return answer != "edit"
        default: return false
        }
    }
}

enum RichBlock: Codable, Equatable, Hashable, Sendable {
    struct CheckItem: Codable, Equatable, Hashable, Sendable { let text: String; let done: Bool }
    struct Stat: Codable, Equatable, Hashable, Sendable { let label: String; let value: String; let delta: String? }
    struct ListItem: Codable, Equatable, Hashable, Sendable { let emoji: String?; let text: String; let meta: String? }

    case checklist(title: String?, items: [CheckItem])
    case card(emoji: String?, title: String, subtitle: String?, body: String?, tone: String)
    case stat([Stat])
    case list(title: String?, items: [ListItem])
    case table(title: String?, columns: [String], rows: [[String]])
    case steps(title: String?, items: [String])
    case quote(text: String, cite: String?)
    case link(url: URL, title: String)
    /// A place on the map (Features/Places/Place.swift): at most `Place.maxPerMessage` per message.
    case place(Place)
}

struct RichMessage: Codable, Equatable, Hashable, Sendable {
    static let maxBubbles = 4, maxBlocks = 8, maxActions = 4, listVisible = 5

    let bubbles: [String]
    let lang: String?
    let blocks: [RichBlock]
    let actions: [RichAction]

    // MARK: Validation

    static func text(_ v: JSONValue?, _ max: Int) -> String {
        var s: String
        switch v {
        case let .string(x)?: s = x
        case let .number(n)?: s = n == n.rounded() ? String(Int(n)) : String(n)
        default: return ""
        }
        s = s.replacingOccurrences(of: "\r\n", with: "\n")
        s = String(s.unicodeScalars.filter { $0 == "\n" || $0 == "\t" || !CharacterSet.controlCharacters.contains($0) })
        s = s.trimmingCharacters(in: .whitespacesAndNewlines)
        return s.count > max ? String(s.prefix(max - 1)).trimmingCharacters(in: .whitespaces) + "…" : s
    }

    static func line(_ v: JSONValue?, _ max: Int) -> String {
        text(v, max).split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }.joined(separator: " ")
    }

    private static func emoji(_ v: JSONValue?) -> String? {
        let s = line(v, 16)
        return s.isEmpty || s.unicodeScalars.contains(where: { CharacterSet.alphanumerics.contains($0) && $0.isASCII }) ? nil : s
    }

    private static func array(_ v: JSONValue?) -> [JSONValue] {
        if case let .array(a)? = v { return a }
        return []
    }

    private static func itemText(_ v: JSONValue, _ max: Int) -> String {
        if case .string = v { return text(v, max) }
        return text(v["text"], max)
    }

    private static func optional(_ s: String) -> String? { s.isEmpty ? nil : s }

    static func block(_ v: JSONValue) -> RichBlock? {
        let title = optional(line(v["title"], 120))
        switch v["type"]?.stringValue {
        case "checklist":
            let items = array(v["items"]).compactMap { i -> RichBlock.CheckItem? in
                let t = itemText(i, 400)
                return t.isEmpty ? nil : .init(text: t, done: i["done"] == .bool(true))
            }.prefix(12)
            return items.isEmpty ? nil : .checklist(title: title, items: Array(items))
        case "card":
            let body = text(v["body"], 600)
            guard title != nil || !body.isEmpty else { return nil }
            let tone = ["success", "warning"].contains(v["tone"]?.stringValue ?? "") ? v["tone"]!.stringValue! : "info"
            return .card(emoji: emoji(v["emoji"]), title: title ?? line(.string(body), 60), subtitle: optional(line(v["subtitle"], 120)),
                         body: title != nil ? optional(body) : nil, tone: tone)
        case "stat":
            let source = v["items"].map(array) ?? [v]
            let items = source.compactMap { i -> RichBlock.Stat? in
                let label = line(i["label"], 40), value = line(i["value"], 40)
                return label.isEmpty || value.isEmpty ? nil : .init(label: label, value: value, delta: optional(line(i["delta"], 40)))
            }.prefix(4)
            return items.isEmpty ? nil : .stat(Array(items))
        case "list":
            let items = array(v["items"]).compactMap { i -> RichBlock.ListItem? in
                let t = itemText(i, 400)
                return t.isEmpty ? nil : .init(emoji: emoji(i["emoji"]), text: t, meta: optional(line(i["meta"], 80)))
            }.prefix(20)
            return items.isEmpty ? nil : .list(title: title, items: Array(items))
        case "table":
            let columns = Array(array(v["columns"]).map { line($0, 80) }.prefix(4))
            guard columns.contains(where: { !$0.isEmpty }) else { return nil }
            let rows = array(v["rows"]).compactMap { r -> [String]? in
                guard case let .array(cells) = r else { return nil }
                let row = columns.indices.map { $0 < cells.count ? line(cells[$0], 80) : "" }
                return row.contains(where: { !$0.isEmpty }) ? row : nil
            }.prefix(6)
            return rows.isEmpty ? nil : .table(title: title, columns: columns, rows: Array(rows))
        case "steps":
            let items = array(v["items"]).map { itemText($0, 400) }.filter { !$0.isEmpty }.prefix(8)
            return items.isEmpty ? nil : .steps(title: title, items: Array(items))
        case "quote":
            let t = text(v["text"], 600)
            return t.isEmpty ? nil : .quote(text: t, cite: optional(line(v["cite"], 120)))
        case "link_preview":
            let raw = line(v["url"], 2000)
            guard let url = URL(string: raw), ["http", "https"].contains(url.scheme?.lowercased() ?? ""), url.host != nil else { return nil }
            return .link(url: url, title: title ?? url.host ?? raw)
        case "place":
            return Place.parse(v).map(RichBlock.place)
        default:
            return nil
        }
    }

    /// The server's limit for an in-app map route (lucina-store `MAX_MAP_ROUTE`).
    static let mapRouteMax = 600

    /// A plain app path such as `/shopping` or `/notes/<slug>`.
    static func isAppRoute(_ path: String) -> Bool {
        if path.hasPrefix("/"), CalendarLink.parse(path) != nil { return true }
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/_-.%")
        return path.hasPrefix("/") && !path.hasPrefix("//") && !path.contains("..") && path.count <= 121
            && path.unicodeScalars.allSatisfy(allowed.contains)
    }

    /// Lucina's in-app map or navigation for a place (`/map/place?…[&navigate=1]`, the "Naviga"
    /// buttons): the server's `valid_map_route` rule, and the query must describe a place.
    static func isMapRoute(_ path: String) -> Bool {
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789%=&_.~-")
        guard path.count <= mapRouteMax, path.hasPrefix("/map/place?") else { return false }
        let query = path.dropFirst("/map/place?".count)
        return !query.isEmpty && query.unicodeScalars.allSatisfy(allowed.contains) && Place.fromRoute(path) != nil
    }

    static func action(_ v: JSONValue, index: Int) -> RichAction? {
        guard let kind = v["kind"]?.stringValue.flatMap(RichAction.Kind.init(rawValue:)) else { return nil }
        let label = line(v["label"], 40)
        guard !label.isEmpty else { return nil }
        let rawId = v["id"]?.stringValue ?? ""
        let validId = (1...48).contains(rawId.count) && rawId.unicodeScalars.allSatisfy {
            CharacterSet.alphanumerics.contains($0) && $0.isASCII || "_.:-".unicodeScalars.contains($0)
        }
        let id = validId ? rawId : "a\(index + 1)"
        let p = v["payload"] ?? .object([:])
        let t = optional(text(p["text"], 500))
        switch kind {
        case .reply:
            guard let t else { return nil }
            return RichAction(id: id, label: label, emoji: emoji(v["emoji"]), kind: kind, text: t, prefill: p["prefill"] == .bool(true), path: nil, answer: nil)
        case .route:
            let path = line(p["path"], mapRouteMax)
            guard isAppRoute(path) || isMapRoute(path) else { return nil }
            return RichAction(id: id, label: label, emoji: emoji(v["emoji"]), kind: kind, text: nil, prefill: false, path: path, answer: nil)
        case .confirm:
            guard let answer = p["answer"]?.stringValue, ["yes", "no", "edit"].contains(answer), answer == "edit" || t != nil else { return nil }
            return RichAction(id: id, label: label, emoji: emoji(v["emoji"]), kind: kind, text: t, prefill: false, path: nil, answer: answer)
        case .dismiss:
            return RichAction(id: id, label: label, emoji: emoji(v["emoji"]), kind: kind, text: nil, prefill: false, path: nil, answer: nil)
        case .secret:
            guard let secret = SecretAction.parse(p) else { return nil }
            return RichAction(id: id, label: label, emoji: emoji(v["emoji"]), kind: kind, text: nil, prefill: false, path: nil, answer: nil, secret: secret)
        }
    }

    /// A v1 message from a chat response or a timeline item's meta, or nil.
    static func parse(_ v: JSONValue?) -> RichMessage? {
        guard let v, v["format"]?.stringValue == "v1" else { return nil }
        let bubbles = array(v["bubbles"]).map { text($0, 900) }.filter { !$0.isEmpty }.prefix(maxBubbles)
        guard !bubbles.isEmpty else { return nil }
        var places = 0
        let blocks = array(v["blocks"]).compactMap(block).filter { b in
            guard case .place = b else { return true }
            places += 1
            return places <= Place.maxPerMessage
        }.prefix(maxBlocks)
        var actions: [RichAction] = []
        for raw in array(v["actions"]) {
            guard let a = action(raw, index: actions.count), !actions.contains(where: { $0.id == a.id }) else { continue }
            actions.append(a)
            if actions.count >= maxActions { break }
        }
        let lang = v["lang"]?.stringValue.flatMap { ["it", "en", "da"].contains($0) ? $0 : nil }
        return RichMessage(bubbles: Array(bubbles), lang: lang, blocks: Array(blocks), actions: actions)
    }

    // MARK: Text

    /// Inline markdown marks removed (speech, previews).
    static func stripMarkdown(_ s: String) -> String {
        var out = s
        let rules: [(String, String)] = [
            (#"!?\[([^\]]*)\]\([^)\s]+\)"#, "$1"),
            (#"\*\*([^*\n]+)\*\*"#, "$1"),
            (#"__([^_\n]+)__"#, "$1"),
            (#"(^|[\s(])\*([^*\n]+)\*(?=$|[\s).,;:!?])"#, "$1$2"),
            (#"(^|[\s(])_([^_\n]+)_(?=$|[\s).,;:!?])"#, "$1$2"),
            (#"~~([^~\n]+)~~"#, "$1"),
            (#"`([^`\n]+)`"#, "$1"),
        ]
        for (pattern, template) in rules {
            out = out.replacingOccurrences(of: pattern, with: template, options: .regularExpression)
        }
        return out
    }

    /// What her voice reads: the bubbles only.
    var speechText: String {
        Self.stripMarkdown(bubbles.joined(separator: " "))
            .split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    /// Typing pause before a bubble: longer bubbles take a little longer (350-900 ms).
    static func typingDelay(_ bubble: String) -> Duration {
        .milliseconds(min(900, 350 + Int(Double(bubble.count) * 2.2)))
    }
}

// MARK: - Views

/// Chat markdown (the Notes renderer, `NoteText.inline`) with a cache: parsing and the link
/// detector are too slow to repeat for every row on every transcript update.
@MainActor
enum ChatMarkdown {
    private static var cache: [String: AttributedString] = [:]
    private static var order: [String] = []
    private static let limit = 400

    static func inline(_ text: String) -> AttributedString {
        if let hit = cache[text] { return hit }
        let value = NoteText.inline(text)
        cache[text] = value
        order.append(text)
        if order.count > limit {
            cache[order.removeFirst()] = nil
        }
        return value
    }
}

/// One Lucina reply in format v1: bubbles (one by one with a typing pause when it just
/// arrived), then widgets, then buttons.
struct RichReplyView: View {
    @Environment(ChatStore.self) private var store
    @Environment(AppModel.self) private var app
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let item: TimelineItem
    let rich: RichMessage
    /// A button the owner already answered by typing its text.
    let inferredChoice: String?
    @State private var shown: Int?
    @State private var typing = false

    private var visible: Int { shown ?? rich.bubbles.count }

    /// A request in the secret queue: its card says it all, and the bubble stays for older clients.
    private var secretRequest: (id: String, name: String)? {
        for a in rich.actions {
            if a.kind == .secret, let id = a.secret?.request { return (id, a.secret?.name ?? a.label) }
        }
        return nil
    }

    var body: some View {
        if let secretRequest {
            SecretRequestCard(item: item, request: secretRequest.id, fallbackName: secretRequest.name)
                .frame(maxWidth: 520, alignment: .leading)
        } else {
            reply
        }
    }

    private var reply: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(rich.bubbles.prefix(visible).enumerated()), id: \.offset) { _, bubble in
                Text(ChatMarkdown.inline(bubble))
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .tint(Theme.accent)
                    .background(Theme.card, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                    .transition(.opacity.combined(with: .move(edge: .bottom)))
                    .accessibilityIdentifier("lucinaBubble")
            }
            if typing {
                TypingDots()
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                    .background(Theme.card, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                    .accessibilityLabel("Lucina is typing")
            }
            if visible >= rich.bubbles.count {
                ForEach(Array(rich.blocks.enumerated()), id: \.offset) { _, block in
                    RichBlockView(block: block)
                }
                if !rich.actions.isEmpty {
                    RichActionsView(item: item, actions: rich.actions, inferredChoice: inferredChoice)
                }
            }
        }
        .frame(maxWidth: 520, alignment: .leading)
        .task(id: item.id) { await reveal() }
    }

    private func reveal() async {
        guard store.consumeReveal(item.id), !reduceMotion, rich.bubbles.count > 1 else { return }
        shown = 1
        for index in 1..<rich.bubbles.count {
            try? await Task.sleep(for: .milliseconds(160))
            withAnimation(.easeOut(duration: 0.2)) { typing = true }
            try? await Task.sleep(for: RichMessage.typingDelay(rich.bubbles[index]))
            withAnimation(.easeOut(duration: 0.25)) {
                typing = false
                shown = index + 1
            }
        }
        shown = nil
    }
}

private struct TypingDots: View {
    var body: some View {
        TimelineView(.periodic(from: .now, by: 0.35)) { context in
            let phase = Int(context.date.timeIntervalSinceReferenceDate / 0.35) % 3
            HStack(spacing: 4) {
                ForEach(0..<3) { index in
                    Circle().frame(width: 7, height: 7).opacity(index == phase ? 1 : 0.35)
                }
            }
            .foregroundStyle(Theme.accent)
        }
    }
}

struct RichBlockView: View {
    let block: RichBlock
    @State private var expanded = false

    var body: some View {
        if case let .place(place) = block {
            PlaceCard(place: place)
        } else {
            styled
        }
    }

    private var styled: some View {
        content
            .font(.subheadline)
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(background, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(tint.opacity(0.28)))
    }

    private var tint: Color {
        if case let .card(_, _, _, _, tone) = block {
            return tone == "warning" ? .orange : tone == "success" ? .green : Theme.accent
        }
        return Theme.accent
    }

    private var background: AnyShapeStyle {
        if case .card = block { return AnyShapeStyle(tint.opacity(0.08)) }
        return AnyShapeStyle(Theme.card)
    }

    @ViewBuilder private func title(_ t: String?) -> some View {
        if let t {
            Text(t.uppercased()).font(.caption2.weight(.semibold)).tracking(1.2).foregroundStyle(.secondary)
                .accessibilityAddTraits(.isHeader)
        }
    }

    @ViewBuilder private var content: some View {
        switch block {
        case let .checklist(t, items):
            VStack(alignment: .leading, spacing: 8) {
                title(t)
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Image(systemName: item.done ? "checkmark.square.fill" : "square")
                            .foregroundStyle(item.done ? Color.green : Color.secondary)
                        Text(ChatMarkdown.inline(item.text)).foregroundStyle(item.done ? .primary : .secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(Text("\(item.done ? String(localized: "Done") : String(localized: "To do")): \(RichMessage.stripMarkdown(item.text))"))
                }
            }
        case let .card(emoji, t, subtitle, body, _):
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    if let emoji { Text(emoji).accessibilityHidden(true) }
                    VStack(alignment: .leading, spacing: 2) {
                        Text(t).font(.headline)
                        if let subtitle { Text(subtitle).font(.caption).foregroundStyle(.secondary) }
                    }
                }
                if let body { Text(ChatMarkdown.inline(body)).fixedSize(horizontal: false, vertical: true) }
            }
            .accessibilityElement(children: .combine)
        case let .stat(items):
            HStack(spacing: 8) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, s in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(s.label).font(.caption2).foregroundStyle(.secondary)
                        Text(s.value).font(.title3.bold()).minimumScaleFactor(0.7).lineLimit(1)
                        if let d = s.delta { Text(d).font(.caption2).foregroundStyle(d.hasPrefix("+") ? Color.green : Theme.accent) }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityElement(children: .combine)
                }
            }
        case let .list(t, items):
            VStack(alignment: .leading, spacing: 8) {
                title(t)
                ForEach(Array((expanded ? items : Array(items.prefix(RichMessage.listVisible))).enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(item.emoji ?? "•").foregroundStyle(Theme.accent).accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(ChatMarkdown.inline(item.text)).fixedSize(horizontal: false, vertical: true)
                            if let meta = item.meta { Text(meta).font(.caption).foregroundStyle(.secondary) }
                        }
                    }
                }
                if items.count > RichMessage.listVisible {
                    Button(expanded ? String(localized: "Show less") : String(localized: "Show \(items.count - RichMessage.listVisible) more")) {
                        withAnimation { expanded.toggle() }
                    }
                    .font(.caption.weight(.semibold))
                }
            }
        case let .table(t, columns, rows):
            VStack(alignment: .leading, spacing: 8) {
                title(t)
                Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                    GridRow { ForEach(Array(columns.enumerated()), id: \.offset) { _, c in Text(c).font(.caption.weight(.semibold)).foregroundStyle(.secondary) } }
                    Divider()
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        GridRow { ForEach(Array(row.enumerated()), id: \.offset) { _, cell in Text(cell) } }
                    }
                }
            }
        case let .steps(t, items):
            VStack(alignment: .leading, spacing: 8) {
                title(t)
                ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(verbatim: "\(index + 1)").font(.caption.bold()).foregroundStyle(Theme.accent)
                            .frame(width: 20, height: 20).background(Theme.accent.opacity(0.14), in: Circle())
                        Text(ChatMarkdown.inline(item)).fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        case let .quote(text, cite):
            VStack(alignment: .leading, spacing: 4) {
                Text(ChatMarkdown.inline(text)).italic()
                if let cite { Text("— \(cite)").font(.caption).foregroundStyle(.secondary) }
            }
            .padding(.leading, 10)
            .overlay(alignment: .leading) { RoundedRectangle(cornerRadius: 2).fill(Color.purple.opacity(0.7)).frame(width: 3) }
        case let .link(url, t):
            Link(destination: url) {
                HStack(spacing: 10) {
                    Image(systemName: "link").foregroundStyle(Theme.accent)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(t).font(.subheadline.weight(.semibold)).foregroundStyle(.primary)
                        Text(url.host ?? "").font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        case .place:
            EmptyView()
        }
    }
}

struct RichActionsView: View {
    @Environment(ChatStore.self) private var store
    @Environment(AppModel.self) private var app
    let item: TimelineItem
    let actions: [RichAction]
    let inferredChoice: String?
    @State private var secretSheet: IdentifiedSecretAction?

    private struct IdentifiedSecretAction: Identifiable {
        let id: String
        let secret: SecretAction
    }

    private var chosen: String? { store.chosenAction(for: item.id) ?? inferredChoice }

    var body: some View {
        FlowRow(spacing: 8) {
            ForEach(actions) { action in
                let isChosen = chosen == action.id
                Button { run(action) } label: {
                    HStack(spacing: 6) {
                        if isChosen {
                            Image(systemName: "checkmark").font(.caption.bold()).foregroundStyle(.green)
                        } else if let emoji = action.emoji {
                            Text(emoji)
                        }
                        Text(action.label).font(.subheadline.weight(.semibold))
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 9)
                    .background(isChosen ? Color.green.opacity(0.16) : Theme.accent.opacity(0.10), in: Capsule())
                    .overlay(Capsule().strokeBorder(isChosen ? Color.green : Theme.accent.opacity(0.4)))
                }
                .buttonStyle(.plain)
                .disabled(chosen != nil)
                .opacity(chosen != nil && !isChosen ? 0.45 : 1)
                .accessibilityLabel(isChosen ? Text("\(action.label), chosen") : Text(action.label))
                .accessibilityIdentifier("richAction-\(action.id)")
            }
        }
        .sheet(item: $secretSheet) { sheet in
            SecretActionSheet(action: sheet.secret) { store.choose(sheet.id, for: item.id) }
        }
    }

    private func run(_ action: RichAction) {
        guard chosen == nil else { return }
        switch action.kind {
        case .reply:
            if action.prefill { store.requestPrefill(action.text ?? "") } else if let t = action.text { Task { await store.send(t) } }
        case .confirm:
            if action.answer == "edit" { store.requestPrefill(action.text ?? "") } else if let t = action.text { Task { await store.send(t) } }
        case .route:
            app.open(route: action.path ?? "/")
        case .dismiss:
            break
        case .secret:
            // Chosen once it is done: cancelling the sheet leaves the button.
            if let secret = action.secret { secretSheet = IdentifiedSecretAction(id: action.id, secret: secret) }
            return
        }
        store.choose(action.id, for: item.id)
    }
}

extension AppModel {
    /// In-app navigation for a `route` button (web paths mapped to tabs).
    func open(route path: String) {
        if let text = ChatComposeRoute.text(path) {
            selectedTab = .chat
            chat.requestPrefill(text)
            return
        }
        if let place = Place.fromRoute(path) {
            showPlace(place, navigate: PlaceLinks.navigates(path))
            return
        }
        // `/server-health…` and the catalog's outage push `/agents/map/<component>`.
        if let link = ServerLink.parse(path) {
            openServer(link)
            return
        }
        let first = path.split(separator: "?", maxSplits: 1).first?.split(separator: "/").first.map(String.init) ?? ""
        switch first {
        case "calendar": openCalendar(NotificationRoute.calendar(path))
        case "shopping": selectedTab = .shopping
        case "gym", "weight": selectedTab = .fitness
        case "lucina", "avatar": selectedTab = .lucina
        case "wardrobe":
            selectedTab = .lucina
            wardrobe.presented = true
        // `decisions` opens the list, `decisions/<id>` one decision (as Lucina's push does).
        case "decisions":
            let id = path.split(separator: "/").dropFirst().first.map(String.init)
            openDecision(id)
        case "settings", "links": showSettings = true
        case "notes":
            selectedTab = .plugins
            showNotes = true
        case "tasks":
            selectedTab = .plugins
            showTasks = true
        case "countdowns":
            selectedTab = .plugins
            showCountdowns = true
        case "memos": openMemo(nil)
        case "media", "films":
            selectedTab = .plugins
            showMedia = true
            let parts = path.split(separator: "/").map { String($0).removingPercentEncoding ?? String($0) }
            if parts.count >= 3, parts[0] == "films", parts[1] == "play" { openFilmID = parts[2] }
        case "plugins": selectedTab = .plugins
        case "computer":
            selectedTab = .plugins
            showComputer = true
        case "callers":
            selectedTab = .plugins
            showCallers = true
        // `/instagram/<username>` and `/pixiv/<id>` open a profile or a work, as on the web.
        case "instagram":
            selectedTab = .plugins
            openInstagramUser = PluginRoute.instagramUser(path)
            showInstagram = true
        case "pixiv":
            selectedTab = .plugins
            openPixivWork = PluginRoute.pixivWork(path)
            showPixiv = true
        case "nclient":
            selectedTab = .plugins
            showNClient = AdultPlugins.isShown
        case "notebooks": showNotebooks = true
        case "gallery": openGallery(path)
        case "drive": openDrive(DriveLink.parse(path) ?? .root)
        default: selectedTab = .chat
        }
        if NotificationRoute.opensDealsMap(path) { showDealsMap = true }
    }

    /// Lucina's own map for a place (never another maps app); `navigate` starts the navigation.
    func showPlace(_ place: Place, navigate: Bool = false) {
        openPlaceNavigates = navigate
        openPlace = place
    }

    /// `/gallery`, `/gallery/album/<id>` (what Lucina found is in `sys-lucina`), `/gallery/search/<words>`.
    private func openGallery(_ path: String) {
        let parts = path.split(separator: "/").map { String($0).removingPercentEncoding ?? String($0) }
        var filter = GalleryAPI.Filter(scope: gallery.scope)
        // What Lucina found can be anywhere (a film, a shared file): her album is not limited to the phones.
        if parts.count >= 3, parts[1] == "album" { filter.album = parts[2]; filter.scope = nil }
        if parts.count >= 3, parts[1] == "search" { filter.query = parts[2...].joined(separator: " ") }
        gallery.filter = filter
        selectedTab = .plugins
        showGallery = true
    }
}

/// Wraps its children onto new lines (buttons).
struct FlowRow: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0, y: CGFloat = 0, rowHeight: CGFloat = 0, widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                y += rowHeight + spacing
                x = 0
                rowHeight = 0
            }
            x += size.width + spacing
            widest = max(widest, x - spacing)
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: min(widest, width), height: y + rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX, y = bounds.minY, rowHeight: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                y += rowHeight + spacing
                x = bounds.minX
                rowHeight = 0
            }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}
