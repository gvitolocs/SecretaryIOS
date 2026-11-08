import AppIntents
import SwiftUI
import WidgetKit

/// The owner's pending approvals (a root plan's reason and its commands) on the Lock Screen and Home Screen.
/// Deny runs here, in the extension (no Face ID needed); Approve opens Secretary's prompt, which asks for Face ID.
/// The server's approval push wakes the app, which refreshes this widget at once; otherwise it looks every 5 minutes
/// while something is pending.
struct ApprovalsWidget: Widget {
    struct Provider: TimelineProvider {
        typealias Entry = SectionEntry<ApprovalsSection>

        func placeholder(in context: Context) -> Entry {
            Entry(date: Date(), section: ApprovalsSection(fetchedAt: Date(), items: [
                .init(id: "apr_1", requester: "nezopt-upgrade", summary: "Run 3 commands as root on nezopt: upgrade window start",
                      details: "systemctl stop lucina-rs\nlvcreate -s -n root-snap ubuntu-vg/ubuntu-lv\ndo-release-upgrade -f DistUpgradeViewNonInteractive",
                      sensitive: true, payloadHash: "sha256:0", expiresAt: Date().addingTimeInterval(240)),
            ]), state: .ok)
        }

        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(ApprovalsSection.self, preview: context.isPreview) { f, _ in try await f.approvals() }
                completion(Entry(date: Date(), section: section ?? placeholder(in: context).section, state: context.isPreview ? .ok : state))
            }
        }

        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(ApprovalsSection.self) { f, _ in try await f.approvals() }
                let live = section?.live() ?? []
                // A second entry at the first expiry takes the stale request off without a fetch.
                var entries = [Entry(date: Date(), section: section, state: state,
                                     relevance: live.isEmpty ? nil : TimelineEntryRelevance(score: 100))]
                if let expiry = live.compactMap(\.expiresAt).min(), expiry > Date() {
                    entries.append(Entry(date: expiry, section: section, state: state))
                }
                completion(Timeline(entries: entries, policy: SectionLoader.policy(minutes: live.isEmpty ? 30 : 5)))
            }
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.approvals.rawValue, provider: Provider()) { entry in
            ApprovalsWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Approvals")
        .description("What is waiting for your OK, like root commands on nezopt. Deny here; Approve asks for Face ID.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge, .accessoryCircular, .accessoryRectangular, .accessoryInline])
    }
}

struct ApprovalsWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: SectionEntry<ApprovalsSection>

    private var items: [ApprovalsSection.Item] { entry.section?.live(at: entry.date) ?? [] }
    private var url: URL { items.first.map { WidgetRoute.url("approvals/\(WidgetRoute.segment($0.id))") } ?? WidgetRoute.url("approvals") }

    var body: some View {
        switch family {
        case .accessoryCircular:
            ZStack {
                AccessoryWidgetBackground()
                VStack(spacing: 0) {
                    Image(systemName: items.isEmpty ? "checkmark.shield" : "lock.shield").font(.title3)
                    if !items.isEmpty { Text("\(items.count)").font(.headline) }
                }
            }
            .widgetAccentable()
            .widgetURL(url)
        case .accessoryInline:
            Text("\(Image(systemName: "lock.shield")) \(inlineText)").widgetURL(url)
        case .accessoryRectangular:
            rectangular.widgetURL(url)
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                WidgetHeader(title: "Approvals", symbol: "lock.shield",
                             tint: items.isEmpty ? LucinaTokens.Colors.accent : LucinaTokens.Colors.warning,
                             trailing: items.count > 1 ? String(localized: "\(items.count) waiting") : nil)
                content
            }
            .widgetURL(url)
        }
    }

    private var inlineText: String {
        guard let first = items.first else { return String(localized: "No approvals waiting") }
        return items.count == 1 ? first.reason : String(localized: "\(items.count) approvals waiting")
    }

    /// The Lock Screen's rectangle: the reason, how many commands, and Deny / Approve.
    private var rectangular: some View {
        VStack(alignment: .leading, spacing: 1) {
            if let first = items.first {
                Text(first.reason).font(.headline).lineLimit(1).widgetAccentable()
                Text(countLine(first)).font(.caption).lineLimit(1)
                HStack(spacing: 6) {
                    Button(intent: DenyApprovalIntent(first)) {
                        Label("Deny", systemImage: "xmark").font(.caption.weight(.semibold))
                    }
                    Button(intent: OpenSecretaryIntent("approvals/\(WidgetRoute.segment(first.id))")) {
                        Label("Approve", systemImage: "faceid").font(.caption.weight(.semibold))
                    }
                }
                .buttonStyle(.plain)
            } else {
                Label("Approvals", systemImage: "checkmark.shield").font(.headline).widgetAccentable()
                Text(entry.state == .signedOut ? "Open Secretary and sign in." : "Nothing waiting.").font(.caption)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func countLine(_ item: ApprovalsSection.Item) -> String {
        let n = item.lines.count
        let commands = n == 1 ? String(localized: "1 command") : String(localized: "\(n) commands")
        let more = items.count > 1 ? " · " + String(localized: "+\(items.count - 1) more") : ""
        if let expires = item.expiresAt { return "\(commands) · \(expires.formatted(.relative(presentation: .numeric)))\(more)" }
        return commands + more
    }

    @ViewBuilder private var content: some View {
        if entry.state == .signedOut {
            WidgetMessage.signedOut()
        } else if let first = items.first {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                Text(first.reason).font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                    .foregroundStyle(LucinaTokens.Colors.text).lineLimit(family == .systemSmall ? 2 : 3)
                Text(countLine(first)).font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1)
                if family != .systemSmall {
                    let shown = family == .systemLarge ? 10 : 3
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(Array(first.lines.prefix(shown).enumerated()), id: \.offset) { _, line in
                            Text(line).font(.system(.caption2, design: .monospaced)).foregroundStyle(LucinaTokens.Colors.textSecondary)
                                .lineLimit(1).truncationMode(.middle)
                        }
                        if first.lines.count > shown {
                            Text("+\(first.lines.count - shown) more").font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textTertiary)
                        }
                    }
                    .privacySensitive()
                }
                Spacer(minLength: 0)
                HStack(spacing: LucinaTokens.Space.sm) {
                    Button(intent: DenyApprovalIntent(first)) {
                        Label("Deny", systemImage: "xmark").frame(maxWidth: .infinity)
                    }
                    .tint(LucinaTokens.Colors.negative)
                    Button(intent: OpenSecretaryIntent("approvals/\(WidgetRoute.segment(first.id))")) {
                        Label("Approve", systemImage: "faceid").frame(maxWidth: .infinity)
                    }
                    .tint(LucinaTokens.Colors.accent)
                }
                .font(LucinaTokens.Fonts.caption.weight(.semibold))
                .buttonStyle(.borderedProminent)
                .labelStyle(.titleAndIcon)
            }
        } else {
            WidgetMessage(symbol: entry.state == .offline ? "wifi.slash" : "checkmark.shield",
                          text: entry.state == .offline ? "Secretary is unreachable." : "Nothing waiting for your OK.")
        }
    }
}

/// Deny from the widget: no Face ID (the contract never asks it for a deny). The request leaves the widget at once;
/// a failure puts it back.
struct DenyApprovalIntent: AppIntent {
    static let title: LocalizedStringResource = "Deny an approval"
    static let isDiscoverable = false

    @Parameter(title: "Approval") var approvalId: String
    @Parameter(title: "Payload hash") var payloadHash: String

    init() {}

    init(_ item: ApprovalsSection.Item) {
        approvalId = item.id
        payloadHash = item.payloadHash
    }

    func perform() async throws -> some IntentResult {
        let trace = DebugEvents.intent("deny_approval", ["id": approvalId]); defer { trace.end() }
        let before = WidgetStore.read(ApprovalsSection.self)
        WidgetStore.update(ApprovalsSection.self) { section in
            section.items.removeAll { $0.id == approvalId }
            section.fetchedAt = Date()
        }
        do {
            guard let fetcher = WidgetFetcher.fromBridge() else { throw WidgetIntentError.signedOut }
            try await fetcher.denyApproval(approvalId, payloadHash: payloadHash)
        } catch {
            if let before { WidgetStore.update(ApprovalsSection.self) { $0 = before } }
            throw error
        }
        return .result()
    }
}
