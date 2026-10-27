import AppIntents
import SwiftUI
import WidgetKit

/// The shopping list: tick items off right on the widget, and the deals nearby for list items.
struct ShoppingWidget: Widget {
    struct Provider: AppIntentTimelineProvider {
        typealias Entry = SectionEntry<ShoppingSection>
        typealias Intent = ShoppingWidgetConfiguration

        func placeholder(in context: Context) -> Entry {
            Entry(date: Date(), section: ShoppingSection(
                fetchedAt: Date(), listId: "default", listName: String(localized: "Shopping"),
                items: [.init(id: "1", name: "Milk", spec: "", icon: "🥛"), .init(id: "2", name: "Eggs", spec: "10", icon: "🥚"),
                        .init(id: "3", name: "Bread", spec: "", icon: "🍞"), .init(id: "4", name: "Bananas", spec: "", icon: "🍌")],
                deals: [.init(item: "Milk", name: "Arla letmælk 1 l", store: "Netto", priceDkk: 8.5, discountPct: 30)]), state: .ok)
        }

        func snapshot(for configuration: Intent, in context: Context) async -> Entry {
            let (section, state) = await load(configuration, preview: context.isPreview)
            return Entry(date: Date(), section: section ?? placeholder(in: context).section, state: context.isPreview ? .ok : state)
        }

        func timeline(for configuration: Intent, in context: Context) async -> Timeline<Entry> {
            let (section, state) = await load(configuration, preview: false)
            return Timeline(entries: [Entry(date: Date(), section: section, state: state)], policy: SectionLoader.policy(minutes: 30))
        }

        private func load(_ configuration: Intent, preview: Bool) async -> (ShoppingSection?, LoadState) {
            let wanted = configuration.list?.id
            let (section, state) = await SectionLoader.load(ShoppingSection.self, preview: preview) { fetcher, previous in
                try await fetcher.shopping(listId: wanted, previous: previous)
            }
            // The cache holds one list: another widget's list means fetching this one.
            if let wanted, let section, section.listId != wanted, let fetcher = WidgetFetcher.fromBridge(),
               let fresh = try? await fetcher.shopping(listId: wanted, previous: nil) {
                return (fresh, .ok)
            }
            return (section, state)
        }
    }

    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: WidgetKind.shopping.rawValue, intent: ShoppingWidgetConfiguration.self, provider: Provider()) { entry in
            ShoppingWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Shopping list")
        .description("Tick items off without opening the app, and see the deals for what's on your list.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge, .accessoryCircular, .accessoryRectangular, .accessoryInline])
    }
}

struct ShoppingWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: SectionEntry<ShoppingSection>

    private var items: [ShoppingSection.Item] { entry.section?.items ?? [] }

    var body: some View {
        switch family {
        case .accessoryCircular:
            Gauge(value: 0) {
                Image(systemName: "cart.fill")
            } currentValueLabel: {
                Text("\(items.count)")
            }
            .gaugeStyle(.accessoryCircularCapacity)
            .widgetURL(WidgetRoute.url("shopping"))
        case .accessoryInline:
            Text("\(Image(systemName: "cart")) \(String(localized: "\(items.count) to buy"))").widgetURL(WidgetRoute.url("shopping"))
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 1) {
                Label(String(localized: "\(items.count) to buy"), systemImage: "cart.fill").font(.headline).widgetAccentable()
                Text(items.prefix(4).map { "\($0.emoji) \($0.name)" }.joined(separator: ", ")).font(.caption).lineLimit(2).privacySensitive()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .widgetURL(WidgetRoute.url("shopping"))
        case .systemSmall:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                header
                list(limit: 4, compact: true)
            }
            .widgetURL(WidgetRoute.url("shopping"))
        case .systemMedium:
            HStack(alignment: .top, spacing: LucinaTokens.Space.md) {
                VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                    header
                    list(limit: 4, compact: true)
                }
                if let deals = entry.section?.deals, !deals.isEmpty {
                    dealsColumn(deals, limit: 3).frame(width: 118)
                }
            }
            .widgetURL(WidgetRoute.url("shopping"))
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
                header
                let deals = entry.section?.deals ?? []
                list(limit: deals.isEmpty ? 8 : 6, compact: false)
                if !deals.isEmpty {
                    Divider()
                    dealsColumn(deals, limit: 2)
                }
            }
            .widgetURL(WidgetRoute.url("shopping"))
        }
    }

    private var header: some View {
        HStack {
            WidgetHeader(title: LocalizedStringKey(entry.section?.listName ?? String(localized: "Shopping")), symbol: "cart.fill",
                         trailing: items.isEmpty ? nil : "\(items.count)")
            Link(destination: WidgetRoute.url("shopping/add")) { CircleAction(symbol: "plus", size: 22) }
        }
    }

    @ViewBuilder private func list(limit: Int, compact: Bool) -> some View {
        if entry.state == .signedOut {
            WidgetMessage.signedOut()
        } else if items.isEmpty {
            WidgetMessage(symbol: entry.state == .offline ? "wifi.slash" : "checkmark.circle",
                          text: entry.state == .offline ? "Lucina is unreachable." : "Nothing to buy.")
        } else {
            VStack(alignment: .leading, spacing: compact ? 3 : 5) {
                ForEach(items.prefix(limit)) { item in row(item, compact: compact) }
                if items.count > limit {
                    Text("+\(items.count - limit) more").font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
                }
            }
            Spacer(minLength: 0)
        }
    }

    /// One item: its catalogue emoji and name; a tap ticks it off (the circle says so).
    private func row(_ item: ShoppingSection.Item, compact: Bool) -> some View {
        Button(intent: CheckShoppingItemIntent(listId: entry.section?.listId ?? "default", itemId: item.id)) {
            HStack(spacing: compact ? LucinaTokens.Space.xs : LucinaTokens.Space.sm) {
                Image(systemName: "circle").font(compact ? .caption2 : .footnote).foregroundStyle(LucinaTokens.Colors.accent).widgetAccentable()
                Text(verbatim: item.emoji)
                    .font(.system(size: compact ? LucinaTokens.FontSize.footnote : LucinaTokens.FontSize.callout))
                    .frame(width: compact ? 18 : 22)
                    .accessibilityHidden(true)
                Text(item.name).font(compact ? LucinaTokens.Fonts.footnote : LucinaTokens.Fonts.callout)
                    .foregroundStyle(LucinaTokens.Colors.text).lineLimit(1)
                if !item.spec.isEmpty {
                    Text(item.spec).font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1)
                }
                if hasDeal(item) {
                    Image(systemName: "tag.fill").font(.caption2).foregroundStyle(LucinaTokens.Colors.positive)
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Tick off \(item.name)"))
    }

    private func hasDeal(_ item: ShoppingSection.Item) -> Bool {
        entry.section?.deals.contains { $0.item.localizedCaseInsensitiveCompare(item.name) == .orderedSame } ?? false
    }

    private func dealsColumn(_ deals: [ShoppingSection.Deal], limit: Int) -> some View {
        Link(destination: WidgetRoute.url("shopping/map")) {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                WidgetHeader(title: "Deals", symbol: "tag.fill", tint: LucinaTokens.Colors.positive)
                ForEach(deals.prefix(limit)) { deal in
                    VStack(alignment: .leading, spacing: 0) {
                        Text(deal.displayName).font(LucinaTokens.Fonts.caption2.weight(.semibold)).foregroundStyle(LucinaTokens.Colors.text).lineLimit(1)
                        Text([deal.store, WidgetFormat.price(deal.priceDkk), deal.discountPct.map { "-\(Int($0))%" }]
                            .compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · "))
                            .font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
            }
        }
    }
}
