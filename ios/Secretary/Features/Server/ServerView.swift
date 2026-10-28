import Charts
import SwiftUI

// The Server plugin (owner, 2026-10-28): the homelab's hosts, what needs attention, the service tree and the
// incident and deploy timeline, from `GET /v1/system/overview`. Refreshed every 60 s while on screen, when the app
// comes back, and on pull to refresh (`fresh=1`). The last data stays up while a refresh runs or fails.

enum ServerColors {
    static func state(_ s: ServerHealth) -> Color {
        switch s {
        case .ok: Theme.positive
        case .warn: Theme.warning
        case .crit: Theme.negative
        case .idle: Theme.textSecondary
        case .unknown: Theme.textTertiary
        }
    }

    /// An incident's severity (`low` … `critical`).
    static func severity(_ s: String?) -> Color {
        switch s ?? "" {
        case "critical", "high": Theme.negative
        case "medium": Theme.warning
        default: Theme.textSecondary
        }
    }

    static func disk(_ usedPct: Double) -> Color {
        usedPct >= 90 ? Theme.negative : usedPct >= 80 ? Theme.warning : Theme.accent
    }
}

extension ServerText {
    /// `low`, `medium`, `high`, `critical` in words.
    static func criticality(_ raw: String) -> String {
        switch raw {
        case "low": String(localized: "Low")
        case "medium": String(localized: "Medium")
        case "high": String(localized: "High")
        case "critical": String(localized: "Critical")
        default: raw
        }
    }

    static func kind(_ event: ServerEvent) -> String {
        switch event.kind {
        case "incident": String(localized: "Incident")
        case "deploy": String(localized: "Deploy")
        case "decision": String(localized: "Decision")
        default: String(localized: "Change")
        }
    }
}

// MARK: - Small pieces

/// A coloured state dot; VoiceOver reads the state.
struct ServerDot: View {
    let state: ServerHealth
    var size: CGFloat = 9

    var body: some View {
        Circle()
            .fill(ServerColors.state(state))
            .frame(width: size, height: size)
            .accessibilityElement()
            .accessibilityLabel(Text(verbatim: state.label))
    }
}

/// A small capsule label (host, duration, project).
struct ServerChip: View {
    let text: String
    var icon: String?
    var tint: Color = Theme.textSecondary

    var body: some View {
        HStack(spacing: 4) {
            if let icon { Image(systemName: icon).accessibilityHidden(true) }
            Text(verbatim: text).lineLimit(1)
        }
        .font(LucinaTokens.Fonts.caption2.weight(.semibold))
        .foregroundStyle(tint)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(Theme.field, in: Capsule())
    }
}

/// Keeps the data fresh on appear, on return to the foreground, then every 60 s.
private struct ServerLive: ViewModifier {
    let store: ServerStore
    @Environment(\.scenePhase) private var phase

    func body(content: Content) -> some View {
        content.task(id: phase) {
            guard phase == .active else { return }
            await store.load()
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(60))
                if Task.isCancelled { break }
                await store.load()
            }
        }
    }
}

extension View {
    func serverLive(_ store: ServerStore) -> some View { modifier(ServerLive(store: store)) }
}

// MARK: - The screen

struct ServerView: View {
    @Environment(ServerStore.self) private var store
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var store = store
        Group {
            switch store.tab {
            case .overview: ServerOverviewTab()
            case .services: ServerServicesTab()
            case .incidents, .deploys: ServerTimelineTab(tab: store.tab)
            }
        }
        .background(Theme.page.ignoresSafeArea())
        .tint(Theme.accent)
        .navigationTitle(Text("Server"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                if store.isLoading {
                    ProgressView().accessibilityLabel(Text("Refreshing"))
                } else {
                    Button { Task { await store.load(fresh: true) } } label: { Image(systemName: "arrow.clockwise") }
                        .accessibilityLabel(Text("Refresh"))
                }
            }
        }
        .navigationDestination(item: $store.openHost) { id in HostDetailView(hostId: id) }
        .sheet(item: $store.sheet) { sheet in
            ServerSheetView(sheet: sheet).environment(store).environment(app)
        }
        .serverLive(store)
        .accessibilityIdentifier("serverScreen")
    }
}

/// The status line, when it was updated, a quiet error, and the section picker (on top of every tab).
struct ServerHeader: View {
    @Environment(ServerStore.self) private var store

    var body: some View {
        @Bindable var store = store
        VStack(alignment: .leading, spacing: LucinaTokens.Space.md) {
            TimelineView(.periodic(from: .now, by: 30)) { context in
                HStack(spacing: 10) {
                    ServerDot(state: dotState, size: 12)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(verbatim: ServerText.status(store.overview, failed: store.error != nil))
                            .font(LucinaTokens.Fonts.headline)
                            .foregroundStyle(Theme.text)
                        if let ago = ServerText.ago(store.overview?.generatedDate ?? store.loadedAt, now: context.date) {
                            Text("Updated \(ago)")
                                .font(LucinaTokens.Fonts.caption2)
                                .foregroundStyle(Theme.textSecondary)
                        }
                    }
                    Spacer(minLength: 0)
                }
                .accessibilityElement(children: .combine)
            }
            if let error = store.error, store.overview != nil {
                ServerQuietNote(icon: "wifi.exclamationmark", text: String(localized: "Couldn't refresh: \(error)"))
            }
            if let failed = failedSources {
                ServerQuietNote(icon: "info.circle", text: String(localized: "Not available right now: \(failed)"))
            }
            Picker(selection: $store.tab) {
                ForEach(ServerTab.allCases) { tab in Text(verbatim: tab.title).tag(tab) }
            } label: {
                Text("Server section")
            }
            .pickerStyle(.segmented)
        }
    }

    /// "ops: Lucina unreachable", for the sources that failed.
    private var failedSources: String? {
        let failed = store.overview?.failedSources ?? []
        guard !failed.isEmpty else { return nil }
        return failed.map { source in [source.name, source.error].compactMap { $0 }.joined(separator: ": ") }.joined(separator: ", ")
    }

    private var dotState: ServerHealth {
        if let overview = store.overview { return overview.overall }
        return store.error != nil ? .crit : .unknown
    }
}

/// A one-line note in secondary text (a failed refresh, a source that is down).
struct ServerQuietNote: View {
    let icon: String
    let text: String

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: icon).accessibilityHidden(true)
            Text(verbatim: text).fixedSize(horizontal: false, vertical: true)
        }
        .font(LucinaTokens.Fonts.footnote)
        .foregroundStyle(Theme.textSecondary)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Overview

struct ServerOverviewTab: View {
    @Environment(ServerStore.self) private var store

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                ServerHeader()
                if let overview = store.overview {
                    content(overview)
                } else if store.error != nil {
                    EmptyCard(icon: "wifi.slash", title: "Server unreachable", detail: "Pull down to try again.")
                } else {
                    ProgressView().frame(maxWidth: .infinity).padding(.vertical, LucinaTokens.Space.xxl)
                }
            }
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.top, LucinaTokens.Space.sm)
            .padding(.bottom, LucinaTokens.Space.xxl)
        }
        .scrollContentBackground(.hidden)
        .refreshable { await store.load(fresh: true) }
    }

    @ViewBuilder private func content(_ overview: ServerOverview) -> some View {
        SectionHeader("Needs attention")
        if overview.needItems.isEmpty {
            Card(cornerRadius: 16) {
                Label("Nothing needs attention", systemImage: "checkmark.seal")
                    .font(LucinaTokens.Fonts.subheadline)
                    .foregroundStyle(Theme.textSecondary)
            }
        } else {
            Card(spacing: 0, cornerRadius: 16) {
                ForEach(Array(overview.needItems.enumerated()), id: \.element.id) { index, item in
                    if index > 0 { Divider() }
                    Button { store.sheet = .item(item.key) } label: {
                        ServerAttentionRow(item: item, hostName: item.host.flatMap { overview.host($0)?.name })
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        if !overview.hosts.isEmpty {
            SectionHeader("Hosts")
            ScrollView(.horizontal, showsIndicators: false) {
                LazyHStack(alignment: .top, spacing: LucinaTokens.Space.gridGap) {
                    ForEach(overview.hosts) { host in
                        Button { store.openHost = host.id } label: { ServerHostCard(host: host) }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier("serverHost-\(host.id)")
                    }
                }
            }
            .scrollClipDisabled()
        }
        ServerRestarted(overview: overview)
        ServerAllGood(overview: overview)
    }
}

/// One host: state, name and role, uptime, system, last report, three sparklines and the disks.
struct ServerHostCard: View {
    let host: ServerHost

    var body: some View {
        Card(spacing: LucinaTokens.Space.sm, cornerRadius: 16) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                ServerDot(state: host.state)
                VStack(alignment: .leading, spacing: 1) {
                    Text(verbatim: host.name).font(LucinaTokens.Fonts.headline).foregroundStyle(Theme.text).lineLimit(1)
                    if let role = host.role {
                        Text(verbatim: role).font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.textSecondary).lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
                if let uptime = host.uptime {
                    Text("up \(uptime)").font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.textSecondary).lineLimit(1)
                }
            }
            if let system = host.systemLine {
                Text(verbatim: system)
                    .font(LucinaTokens.Fonts.caption2)
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            ServerHostNotes(host: host)
            ForEach(ServerMetric.allCases) { metric in
                if metric.current(host) != nil || !(host.history.map { metric.points($0) } ?? []).isEmpty {
                    ServerSparkline(metric: metric, points: host.history.map { metric.points($0) } ?? [], current: metric.current(host),
                                    range: host.history?.range())
                }
            }
            ForEach(host.disks) { disk in ServerDiskBar(disk: disk) }
            TimelineView(.periodic(from: .now, by: 60)) { context in
                if let ago = ServerText.ago(host.lastReportDate, now: context.date) {
                    Text("Last report \(ago)").font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.textTertiary)
                }
            }
        }
        .frame(width: 260)
        .contentShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityHint(Text("Opens the host's details"))
    }
}

/// "Unreachable" for a host that is down, "Limited data" for one only pinged.
struct ServerHostNotes: View {
    let host: ServerHost

    var body: some View {
        if host.state == .crit || host.reachable == false {
            Label("Unreachable", systemImage: "wifi.slash")
                .font(LucinaTokens.Fonts.caption)
                .foregroundStyle(Theme.negative)
        } else if host.limited {
            Label("Limited data", systemImage: "info.circle")
                .font(LucinaTokens.Fonts.caption2)
                .foregroundStyle(Theme.textSecondary)
        }
    }
}

// MARK: - Metrics

enum ServerMetric: String, CaseIterable, Identifiable {
    case temperature, load, memory

    var id: String { rawValue }

    var title: String {
        switch self {
        case .temperature: String(localized: "CPU temperature")
        case .load: String(localized: "Load")
        case .memory: String(localized: "Memory")
        }
    }

    func points(_ history: ServerHistory) -> [ServerPoint] {
        switch self {
        case .temperature: history.points(history.tctl)
        case .load: history.points(history.load1)
        case .memory: history.points(history.mem)
        }
    }

    func current(_ host: ServerHost) -> Double? {
        switch self {
        case .temperature: host.tctl
        case .load: host.load1
        case .memory: host.memUsedPct
        }
    }

    func format(_ value: Double) -> String {
        switch self {
        case .temperature: "\(ServerText.number(value)) °C"
        case .load: ServerText.number(value, digits: 2)
        case .memory: "\(ServerText.number(value)) %"
        }
    }

    /// The temperature chart's warning line.
    static let hotLimit = 90.0

    /// The y range: the data's, padded; the temperature chart also shows the 90 °C line.
    func domain(_ points: [ServerPoint], withLimit: Bool = false) -> ClosedRange<Double> {
        let values = points.map(\.value)
        var low = values.min() ?? 0
        var high = values.max() ?? 1
        if withLimit, self == .temperature { high = max(high, Self.hotLimit + 2) }
        if self == .memory { low = min(low, 0); high = max(high, 100) }
        if self == .load { low = min(low, 0) }
        if self == .temperature {
            let padding = max(2, (high - low) * 0.05)
            low -= padding
            high += padding
        }
        if high - low < 1 { low -= 0.5; high += 0.5 }
        return low...high
    }

    /// "CPU temperature, 24 hours, now 95 °C, range 61–96".
    func accessibilityLabel(points: [ServerPoint], current: Double?) -> String {
        let now = (current ?? points.last?.value).map(format) ?? "–"
        guard let low = points.map(\.value).min(), let high = points.map(\.value).max() else {
            return String(localized: "\(title), now \(now)")
        }
        let digits = self == .load ? 2 : 0
        return String(localized: "\(title), 24 hours, now \(now), range \(ServerText.number(low, digits: digits))–\(ServerText.number(high, digits: digits))")
    }
}

/// A tiny line of the last 24 h with its title and current value (no axes).
struct ServerSparkline: View {
    let metric: ServerMetric
    let points: [ServerPoint]
    let current: Double?
    /// The 24 h window, so an hour of samples sits at the right end instead of filling the width.
    var range: ClosedRange<Date>?

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline) {
                Text(verbatim: metric.title).font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.textSecondary).lineLimit(1)
                Spacer(minLength: 4)
                Text(verbatim: (current ?? points.last?.value).map(metric.format) ?? "–")
                    .font(LucinaTokens.Fonts.caption.monospacedDigit())
                    .foregroundStyle(Theme.text)
            }
            if points.count >= 2 {
                Chart(points) { point in
                    LineMark(x: .value(String(localized: "Time"), point.date), y: .value(String(localized: "Value"), point.value),
                             series: .value(String(localized: "Segment"), point.segment))
                        .foregroundStyle(Theme.accent)
                        .lineStyle(StrokeStyle(lineWidth: 1.5, lineCap: .round, lineJoin: .round))
                        .interpolationMethod(.linear)
                }
                .chartXScale(domain: range ?? (points.first!.date...points.last!.date))
                .chartXAxis(.hidden)
                .chartYAxis(.hidden)
                .chartLegend(.hidden)
                .chartYScale(domain: metric.domain(points))
                .frame(height: 26)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(verbatim: metric.accessibilityLabel(points: points, current: current)))
    }
}

/// A disk: mount, used %, free space, and a thin bar.
struct ServerDiskBar: View {
    let disk: ServerDisk
    var showsSize = false

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(verbatim: disk.mount)
                    .font(LucinaTokens.Fonts.caption2.monospaced())
                    .foregroundStyle(Theme.text)
                    .lineLimit(1)
                    .truncationMode(.middle)
                Spacer(minLength: 4)
                Text(verbatim: line)
                    .font(LucinaTokens.Fonts.caption2.monospacedDigit())
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(1)
            }
            if let used = disk.usedPct {
                ProgressView(value: min(1, max(0, used / 100)))
                    .tint(ServerColors.disk(used))
                    .accessibilityHidden(true)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var line: String {
        let used = disk.usedPct.map { "\(ServerText.number($0)) %" }
        let free = disk.freeGb.map { String(localized: "\(ServerText.gigabytes($0)) free") }
        let size = showsSize ? disk.sizeGb.map { String(localized: "of \(ServerText.gigabytes($0))") } : nil
        return [used, free, size].compactMap { $0 }.joined(separator: " · ")
    }
}

// MARK: - Attention

/// Severity, title, summary, how long, where, and how much it breaks.
struct ServerAttentionRow: View {
    let item: ServerAttention
    let hostName: String?

    var body: some View {
        HStack(alignment: .top, spacing: LucinaTokens.Space.md) {
            ServerDot(state: item.severity).padding(.top, 6)
            VStack(alignment: .leading, spacing: 4) {
                Text(verbatim: item.title)
                    .font(LucinaTokens.Fonts.body.weight(.semibold))
                    .foregroundStyle(Theme.text)
                    .multilineTextAlignment(.leading)
                if let summary = item.summary, !summary.isEmpty {
                    Text(verbatim: summary)
                        .font(LucinaTokens.Fonts.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                        .multilineTextAlignment(.leading)
                        .lineLimit(3)
                }
                FlowRow(spacing: 6) {
                    if let length = ServerText.duration(since: item.sinceDate) {
                        ServerChip(text: String(localized: "for \(length)"), icon: "clock")
                    }
                    if let host = hostName ?? item.host {
                        ServerChip(text: host, icon: "server.rack")
                    }
                    if item.impactCount > 0 {
                        // Crit: it is breaking them now; warn: they would break if it went down.
                        ServerChip(text: item.severity == .crit ? String(localized: "breaks \(item.impactCount)") : String(localized: "affects \(item.impactCount)"),
                                   icon: "bolt.horizontal", tint: item.severity == .crit ? Theme.negative : Theme.warning)
                    }
                }
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right")
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Theme.textTertiary)
                .padding(.top, 4)
                .accessibilityHidden(true)
        }
        .padding(.vertical, LucinaTokens.Space.md)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("serverItem-\(item.key)")
    }
}

/// "Restarted recently · N", collapsed: running now, but failed or restarted in the last 24 h.
struct ServerRestarted: View {
    @Environment(ServerStore.self) private var store
    let overview: ServerOverview
    @State private var expanded = false

    var body: some View {
        let items = overview.flappingItems
        if !items.isEmpty {
            Card(cornerRadius: 16) {
                DisclosureGroup(isExpanded: $expanded) {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(items) { item in
                            Button { store.sheet = .item(item.key) } label: {
                                HStack(spacing: 8) {
                                    ServerDot(state: .warn, size: 6)
                                    VStack(alignment: .leading, spacing: 1) {
                                        Text(verbatim: item.title)
                                            .font(LucinaTokens.Fonts.footnote)
                                            .foregroundStyle(Theme.text)
                                            .multilineTextAlignment(.leading)
                                        if let summary = item.summary {
                                            Text(verbatim: summary).font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.textSecondary)
                                        }
                                    }
                                    Spacer(minLength: 4)
                                    if let host = item.host {
                                        Text(verbatim: overview.host(host)?.name ?? host).font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.textTertiary)
                                    }
                                }
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityElement(children: .combine)
                        }
                    }
                    .padding(.top, LucinaTokens.Space.sm)
                } label: {
                    Label {
                        Text("Restarted recently · \(max(overview.flappingCount, items.count))")
                    } icon: {
                        Image(systemName: "arrow.clockwise.circle")
                    }
                    .font(LucinaTokens.Fonts.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                }
            }
        }
    }
}

/// "All good · N checks", collapsed: the healthy components by group.
struct ServerAllGood: View {
    @Environment(ServerStore.self) private var store
    let overview: ServerOverview
    @State private var expanded = false

    var body: some View {
        let groups = overview.healthyByGroup
        let count = overview.checksOk ?? groups.reduce(0) { total, group in total + group.components.count }
        if count > 0 {
            Card(cornerRadius: 16) {
                DisclosureGroup(isExpanded: $expanded) {
                    VStack(alignment: .leading, spacing: LucinaTokens.Space.md) {
                        ForEach(groups) { group in
                            VStack(alignment: .leading, spacing: 6) {
                                Text(verbatim: group.group)
                                    .font(LucinaTokens.Fonts.caption)
                                    .foregroundStyle(Theme.textSecondary)
                                    .accessibilityAddTraits(.isHeader)
                                ForEach(group.components) { ref in
                                    Button { store.sheet = .component(ref.id) } label: {
                                        HStack(spacing: 8) {
                                            ServerDot(state: ref.component.state, size: 6)
                                            Text(verbatim: ref.component.name)
                                                .font(LucinaTokens.Fonts.footnote)
                                                .foregroundStyle(Theme.text)
                                                .multilineTextAlignment(.leading)
                                            Spacer(minLength: 4)
                                            if let host = ref.component.host {
                                                Text(verbatim: host).font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.textTertiary)
                                            }
                                        }
                                        .contentShape(Rectangle())
                                    }
                                    .buttonStyle(.plain)
                                    .accessibilityElement(children: .combine)
                                }
                            }
                        }
                    }
                    .padding(.top, LucinaTokens.Space.sm)
                } label: {
                    Label {
                        Text("All good · \(count) checks")
                    } icon: {
                        Image(systemName: "checkmark.circle")
                    }
                    .font(LucinaTokens.Fonts.subheadline)
                    .foregroundStyle(Theme.textSecondary)
                }
            }
        }
    }
}

// MARK: - Services

struct ServerServicesTab: View {
    @Environment(ServerStore.self) private var store
    @State private var query = ""

    private struct Block: Identifiable {
        let group: ServerGroup
        let project: ServerProject
        let components: [ServerComponentRef]
        var id: String { "\(group.name)/\(project.id)" }
    }

    private var blocks: [Block] {
        (store.overview?.services ?? []).flatMap { group in
            group.projects.compactMap { project -> Block? in
                let rows = project.components
                    .map { ServerComponentRef(component: $0, group: group.name, project: project.id) }
                    .filter { $0.matches(query) }
                return rows.isEmpty ? nil : Block(group: group, project: project, components: rows)
            }
        }
    }

    var body: some View {
        List {
            Section { ServerHeader() }
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets(top: 0, leading: 0, bottom: LucinaTokens.Space.sm, trailing: 0))
            let shown = blocks
            if store.overview != nil, shown.isEmpty {
                Section {
                    Text(query.isEmpty ? String(localized: "No services in the catalog.") : String(localized: "No service matches."))
                        .foregroundStyle(Theme.textSecondary)
                }
            }
            ForEach(shown) { block in
                Section {
                    ForEach(block.components) { ref in
                        Button { store.sheet = .component(ref.id) } label: { ServerComponentRow(ref: ref) }
                            .buttonStyle(.plain)
                    }
                } header: {
                    HStack(spacing: 6) {
                        ServerDot(state: block.project.state, size: 7)
                        Text(verbatim: "\(block.group.name) · \(block.project.id)")
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .searchable(text: $query, prompt: Text("Search services"))
        .refreshable { await store.load(fresh: true) }
    }
}

struct ServerComponentRow: View {
    let ref: ServerComponentRef

    var body: some View {
        HStack(spacing: LucinaTokens.Space.md) {
            ServerDot(state: ref.component.state)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: ref.component.name)
                    .font(LucinaTokens.Fonts.body)
                    .foregroundStyle(Theme.text)
                    .multilineTextAlignment(.leading)
                if ref.component.name != ref.component.id {
                    Text(verbatim: ref.component.id)
                        .font(LucinaTokens.Fonts.caption2.monospaced())
                        .foregroundStyle(Theme.textSecondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 4)
            if let host = ref.component.host {
                Text(verbatim: host).font(LucinaTokens.Fonts.caption).foregroundStyle(Theme.textSecondary).lineLimit(1)
            }
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Incidents and deploys

struct ServerTimelineTab: View {
    @Environment(ServerStore.self) private var store
    let tab: ServerTab

    private struct Day: Identifiable {
        let day: Date
        let events: [ServerEvent]
        var id: Date { day }
    }

    /// Newest day first; events keep the server's order (newest first).
    private var days: [Day] {
        let events = store.overview?.events(tab) ?? []
        let calendar = Calendar.current
        var order: [Date] = []
        var byDay: [Date: [ServerEvent]] = [:]
        for event in events {
            let day = event.date.map { calendar.startOfDay(for: $0) } ?? .distantPast
            if byDay[day] == nil { order.append(day) }
            byDay[day, default: []].append(event)
        }
        return order.sorted(by: >).map { Day(day: $0, events: byDay[$0] ?? []) }
    }

    var body: some View {
        List {
            Section { ServerHeader() }
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets(top: 0, leading: 0, bottom: LucinaTokens.Space.sm, trailing: 0))
            let shown = days
            if store.overview != nil, shown.isEmpty {
                Section {
                    Text(tab == .incidents ? String(localized: "No incidents or changes recently.") : String(localized: "No deploys or decisions recently."))
                        .foregroundStyle(Theme.textSecondary)
                }
            }
            ForEach(shown) { day in
                Section {
                    ForEach(Array(day.events.enumerated()), id: \.offset) { _, event in
                        ServerEventRow(event: event)
                    }
                } header: {
                    if day.day == .distantPast {
                        Text("Date unknown")
                    } else {
                        Text(day.day, format: .dateTime.weekday(.wide).day().month(.wide))
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .scrollContentBackground(.hidden)
        .refreshable { await store.load(fresh: true) }
    }
}

struct ServerEventRow: View {
    let event: ServerEvent

    var body: some View {
        HStack(alignment: .top, spacing: LucinaTokens.Space.md) {
            Image(systemName: event.symbol)
                .foregroundStyle(color)
                .frame(width: 24)
                .padding(.top, 2)
                .accessibilityLabel(Text(verbatim: ServerText.kind(event)))
            VStack(alignment: .leading, spacing: 4) {
                Text(verbatim: event.title)
                    .font(LucinaTokens.Fonts.body)
                    .foregroundStyle(Theme.text)
                if let detail = event.detail, !detail.isEmpty {
                    Text(verbatim: detail)
                        .font(LucinaTokens.Fonts.footnote)
                        .foregroundStyle(Theme.textSecondary)
                        .lineLimit(3)
                }
                FlowRow(spacing: 6) {
                    if let severity = event.severity {
                        ServerChip(text: ServerText.criticality(severity), icon: "exclamationmark.circle", tint: ServerColors.severity(severity))
                    }
                    if event.from != nil || event.to != nil {
                        transition
                    }
                    if let project = event.project { ServerChip(text: project, icon: "folder") }
                    if let host = event.host, host != event.project { ServerChip(text: host, icon: "server.rack") }
                }
            }
            Spacer(minLength: 4)
            if let date = event.date {
                Text(date, format: .dateTime.hour().minute())
                    .font(LucinaTokens.Fonts.caption.monospacedDigit())
                    .foregroundStyle(Theme.textSecondary)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }

    private var color: Color {
        switch event.kind {
        case "incident": ServerColors.severity(event.severity ?? "high")
        case "change": ServerColors.state(ServerHealth(raw: event.to))
        default: Theme.accent
        }
    }

    /// "ok → crit", each in its state's colour.
    private var transition: some View {
        let from = ServerHealth(raw: event.from)
        let to = ServerHealth(raw: event.to)
        return HStack(spacing: 4) {
            Text(verbatim: from.label).foregroundStyle(ServerColors.state(from))
            Image(systemName: "arrow.right").foregroundStyle(Theme.textTertiary).accessibilityLabel(Text("to"))
            Text(verbatim: to.label).foregroundStyle(ServerColors.state(to))
        }
        .font(LucinaTokens.Fonts.caption2.weight(.semibold))
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(Theme.field, in: Capsule())
    }
}
