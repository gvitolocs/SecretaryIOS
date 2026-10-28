import Charts
import SwiftUI
import UIKit

// The Server plugin's detail screens: a host (pushed), an attention item and a component (sheets).

// MARK: - Host

struct HostDetailView: View {
    @Environment(ServerStore.self) private var store
    @Environment(AppModel.self) private var app
    let hostId: String
    @State private var sheet: ServerSheet?

    var body: some View {
        let overview = store.overview
        let host = overview?.host(hostId)
        ScrollView {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                if let overview, let host {
                    content(host, overview)
                } else {
                    EmptyCard(icon: "server.rack", title: "Host not found", detail: "This host isn't in the latest report.")
                }
            }
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.top, LucinaTokens.Space.sm)
            .padding(.bottom, LucinaTokens.Space.xxl)
        }
        .lucinaScreen()
        .navigationTitle(Text(verbatim: host?.name ?? hostId))
        .navigationBarTitleDisplayMode(.large)
        .refreshable { await store.load(fresh: true) }
        .sheet(item: $sheet) { sheet in
            ServerSheetView(sheet: sheet).environment(store).environment(app)
        }
        .serverLive(store)
    }

    @ViewBuilder private func content(_ host: ServerHost, _ overview: ServerOverview) -> some View {
        Card(spacing: LucinaTokens.Space.sm, cornerRadius: 16) {
            HStack(spacing: 8) {
                ServerDot(state: host.state, size: 11)
                Text(verbatim: host.state.label).font(LucinaTokens.Fonts.headline)
                if let role = host.role {
                    Text(verbatim: "· \(role)").font(LucinaTokens.Fonts.subheadline).foregroundStyle(Theme.textSecondary)
                }
            }
            .accessibilityElement(children: .combine)
            ServerHostNotes(host: host)
            ServerFact(label: "Uptime", value: host.uptime)
            ServerFact(label: "System", value: host.os)
            ServerFact(label: "Kernel", value: host.kernel)
            ServerFact(label: "CPUs", value: host.cpus.map { "\($0)" })
            ServerFact(label: "Load", value: host.load.map { values in values.map { ServerText.number($0, digits: 2) }.joined(separator: " · ") })
            TimelineView(.periodic(from: .now, by: 60)) { context in
                ServerFact(label: "Last report", value: ServerText.ago(host.lastReportDate, now: context.date))
            }
        }
        if !host.findings.isEmpty {
            Card(spacing: 6, cornerRadius: 16) {
                Text("Findings").font(LucinaTokens.Fonts.headline)
                ForEach(Array(host.findings.enumerated()), id: \.offset) { _, finding in
                    Label(finding, systemImage: "exclamationmark.circle")
                        .font(LucinaTokens.Fonts.subheadline)
                        .foregroundStyle(Theme.text)
                }
            }
        }
        if let history = host.history {
            ForEach(ServerMetric.allCases) { metric in
                let points = metric.points(history)
                if points.count >= 2 {
                    ServerHistoryChart(metric: metric, points: points, current: metric.current(host), end: ServerDates.parse(history.end))
                }
            }
        }
        if !host.disks.isEmpty {
            Card(spacing: LucinaTokens.Space.md, cornerRadius: 16) {
                Text("Disks").font(LucinaTokens.Fonts.headline)
                ForEach(host.disks) { disk in ServerDiskBar(disk: disk, showsSize: true) }
            }
        }
        let items = overview.attention(onHost: host.id)
        if !items.isEmpty {
            SectionHeader("Needs attention")
            Card(spacing: 0, cornerRadius: 16) {
                ForEach(Array(items.enumerated()), id: \.element.id) { index, item in
                    if index > 0 { Divider() }
                    Button { sheet = .item(item.key) } label: { ServerAttentionRow(item: item, hostName: host.name) }
                        .buttonStyle(.plain)
                }
            }
        }
        let components = overview.components(onHost: host.id)
        if !components.isEmpty {
            SectionHeader("Components")
            Card(spacing: 0, cornerRadius: 16) {
                ForEach(Array(components.enumerated()), id: \.element.id) { index, ref in
                    if index > 0 { Divider() }
                    Button { sheet = .component(ref.id) } label: { ServerComponentRow(ref: ref).padding(.vertical, 10) }
                        .buttonStyle(.plain)
                }
            }
        }
    }
}

/// A label and a value on one line (nothing when there is no value).
struct ServerFact: View {
    let label: LocalizedStringKey
    let value: String?

    var body: some View {
        if let value, !value.isEmpty {
            HStack(alignment: .firstTextBaseline, spacing: LucinaTokens.Space.md) {
                Text(label).font(LucinaTokens.Fonts.subheadline).foregroundStyle(Theme.textSecondary)
                Spacer(minLength: 4)
                Text(verbatim: value)
                    .font(LucinaTokens.Fonts.subheadline)
                    .foregroundStyle(Theme.text)
                    .multilineTextAlignment(.trailing)
                    .textSelection(.enabled)
            }
            .accessibilityElement(children: .combine)
        }
    }
}

/// 24 hours of one metric, full width, with the hours on the x axis (and the 90 °C line for the temperature).
struct ServerHistoryChart: View {
    let metric: ServerMetric
    let points: [ServerPoint]
    let current: Double?
    let end: Date?

    var body: some View {
        Card(spacing: LucinaTokens.Space.md, cornerRadius: 16) {
            HStack(alignment: .firstTextBaseline) {
                Text(verbatim: metric.title).font(LucinaTokens.Fonts.headline)
                Spacer(minLength: 4)
                Text(verbatim: (current ?? points.last?.value).map(metric.format) ?? "–")
                    .font(LucinaTokens.Fonts.subheadline.monospacedDigit())
                    .foregroundStyle(Theme.textSecondary)
            }
            Chart {
                ForEach(points) { point in
                    AreaMark(x: .value(String(localized: "Time"), point.date), y: .value(String(localized: "Value"), point.value),
                             series: .value(String(localized: "Segment"), point.segment))
                        .foregroundStyle(LucinaChartStyle.area)
                        .interpolationMethod(.linear)
                    LineMark(x: .value(String(localized: "Time"), point.date), y: .value(String(localized: "Value"), point.value),
                             series: .value(String(localized: "Segment"), point.segment))
                        .foregroundStyle(Theme.accent)
                        .lineStyle(LucinaChartStyle.line)
                        .interpolationMethod(.linear)
                }
                if metric == .temperature {
                    RuleMark(y: .value(String(localized: "Limit"), ServerMetric.hotLimit))
                        .foregroundStyle(LucinaChartStyle.goalColor)
                        .lineStyle(LucinaChartStyle.goal)
                        .annotation(position: .top, alignment: .leading) {
                            Text(verbatim: metric.format(ServerMetric.hotLimit))
                                .font(LucinaTokens.Fonts.caption2)
                                .foregroundStyle(Theme.textSecondary)
                        }
                }
            }
            .chartXScale(domain: range)
            .chartYScale(domain: metric.domain(points, withLimit: true))
            .chartYAxis { AxisMarks(position: .leading) }
            .chartXAxis {
                AxisMarks(values: .stride(by: .hour, count: 6)) { _ in
                    AxisGridLine()
                    AxisValueLabel(format: .dateTime.hour())
                }
            }
            .lucinaChart(height: 170)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text(verbatim: metric.accessibilityLabel(points: points, current: current)))
        }
    }

    /// The last 24 hours, to the end of the newest bucket.
    private var range: ClosedRange<Date> {
        let last = end ?? points.last?.date ?? Date()
        return last.addingTimeInterval(-24 * 3600)...last
    }
}

// MARK: - Sheets

/// The sheet for an attention item or a component, looked up in the current data (so a refresh updates it).
struct ServerSheetView: View {
    @Environment(ServerStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let sheet: ServerSheet
    @State private var toast: String?

    var body: some View {
        NavigationStack {
            Group {
                switch sheet {
                case let .item(key):
                    if let item = store.overview?.item(key: key) {
                        ServerItemView(item: item, toast: $toast)
                    } else {
                        gone
                    }
                case let .component(id):
                    if let ref = store.overview?.component(id) {
                        ServerComponentView(ref: ref, toast: $toast)
                    } else {
                        gone
                    }
                }
            }
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .overlay(alignment: .bottom) {
            if let toast {
                Label(toast, systemImage: "checkmark.circle.fill")
                    .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.onAccent)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
                    .background(Theme.accentFill, in: Capsule())
                    .padding(.bottom, LucinaTokens.Space.xl)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .animation(.easeInOut(duration: LucinaTokens.Motion.base), value: toast)
        .task(id: toast) {
            guard toast != nil else { return }
            try? await Task.sleep(for: .seconds(1.8))
            if !Task.isCancelled { toast = nil }
        }
        .lucinaSheet()
    }

    private var gone: some View {
        ScrollView {
            EmptyCard(icon: "checkmark.seal", title: "No longer reported",
                      detail: "It isn't in the latest report anymore; it may have recovered.")
                .padding(LucinaTokens.Space.gutter)
        }
    }
}

/// An attention item: what is wrong, since when, what it breaks, and what to do.
struct ServerItemView: View {
    @Environment(ServerStore.self) private var store
    let item: ServerAttention
    @Binding var toast: String?

    var body: some View {
        let hostName = item.host.flatMap { store.overview?.host($0)?.name } ?? item.host
        ScrollView {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 8) {
                        ServerDot(state: item.severity, size: 11)
                        Text(verbatim: item.severity.label)
                            .font(LucinaTokens.Fonts.caption)
                            .foregroundStyle(ServerColors.state(item.severity))
                    }
                    Text(verbatim: item.title).font(LucinaTokens.Fonts.title2)
                    if let summary = item.summary, !summary.isEmpty {
                        Text(verbatim: summary).font(LucinaTokens.Fonts.body).foregroundStyle(Theme.textSecondary)
                    }
                }
                .accessibilityElement(children: .combine)
                facts(hostName: hostName)
                if !item.details.isEmpty {
                    Card(spacing: 6, cornerRadius: 16) {
                        Text("Details").font(LucinaTokens.Fonts.headline)
                        ForEach(Array(item.details.enumerated()), id: \.offset) { _, line in
                            Text(verbatim: "• \(line)")
                                .font(LucinaTokens.Fonts.footnote)
                                .foregroundStyle(Theme.text)
                                .textSelection(.enabled)
                        }
                    }
                }
                if let impact = item.impact, !impact.isEmpty { ServerImpactCard(impact: impact, down: item.severity == .crit) }
                if !item.caused.isEmpty {
                    Card(spacing: 6, cornerRadius: 16) {
                        Text("Also down because of this").font(LucinaTokens.Fonts.headline)
                        Text(verbatim: item.caused.map { store.overview?.component($0)?.component.name ?? $0 }.joined(separator: ", "))
                            .font(LucinaTokens.Fonts.subheadline)
                            .foregroundStyle(Theme.textSecondary)
                    }
                }
                ServerItemActions(item: item, hostName: hostName, toast: $toast)
            }
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.vertical, LucinaTokens.Space.lg)
        }
        .navigationTitle(Text(verbatim: item.title))
        .accessibilityIdentifier("serverItemSheet")
    }

    private func facts(hostName: String?) -> some View {
        Card(spacing: LucinaTokens.Space.sm, cornerRadius: 16) {
            TimelineView(.periodic(from: .now, by: 60)) { context in
                VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
                    if let since = item.sinceDate {
                        let length = String(localized: "for \(ServerText.duration(since: since, now: context.date) ?? "")")
                        ServerFact(label: "Since", value: "\(since.formatted(date: .abbreviated, time: .shortened)) · \(length)")
                    }
                }
            }
            ServerFact(label: "Failures in 24 h", value: item.failures24h.map { "\($0)" })
            ServerFact(label: "Host", value: hostName)
            ServerFact(label: "Project", value: [item.group, item.project].compactMap { $0 }.joined(separator: " · "))
            ServerFact(label: "Kind", value: item.kind)
            ServerFact(label: "Criticality", value: item.criticality.map(ServerText.criticality))
            ServerFact(label: "Reported by", value: item.sources.joined(separator: ", "))
        }
    }
}

/// What breaks or degrades while the item is down, and the public hostnames that stop answering.
struct ServerImpactCard: View {
    let impact: ServerImpact
    /// Down now (crit): what it breaks; otherwise what it would break if it went down.
    var down = true

    var body: some View {
        Card(spacing: LucinaTokens.Space.sm, cornerRadius: 16) {
            Text(down ? "Impact" : "If it goes down").font(LucinaTokens.Fonts.headline)
            if !impact.broken.isEmpty {
                line(down ? "Broken" : "Would break", impact.broken.map(\.name), color: Theme.negative)
            }
            if !impact.degraded.isEmpty {
                line(down ? "Degraded" : "Would degrade", impact.degraded.map(\.name), color: Theme.warning)
            }
            if !impact.publicHostnamesBroken.isEmpty {
                line("Public hostnames", impact.publicHostnamesBroken, color: Theme.negative)
            }
        }
    }

    private func line(_ title: LocalizedStringKey, _ names: [String], color: Color) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(LucinaTokens.Fonts.caption).foregroundStyle(color)
            Text(verbatim: names.joined(separator: ", "))
                .font(LucinaTokens.Fonts.subheadline)
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
        }
        .accessibilityElement(children: .combine)
    }
}

/// Copy the logs or restart command, read the runbook, or ask Lucina.
struct ServerItemActions: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    let item: ServerAttention
    let hostName: String?
    @Binding var toast: String?
    @State private var runbookOpen = false

    var body: some View {
        VStack(alignment: .leading, spacing: LucinaTokens.Space.md) {
            if let logs = item.logs, !logs.isEmpty {
                ServerCommandBox(title: "Logs", command: logs)
                Button { copy(logs, String(localized: "Logs command copied")) } label: {
                    Label("Copy logs command", systemImage: "doc.on.doc").frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("serverCopyLogs")
            }
            if let restart = item.restart, !restart.isEmpty {
                ServerCommandBox(title: "Restart", command: restart)
                Button { copy(restart, String(localized: "Restart command copied")) } label: {
                    Label("Copy restart command", systemImage: "arrow.counterclockwise").frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
            }
            if let runbook = item.runbook, !runbook.isEmpty {
                Card(cornerRadius: 16) {
                    DisclosureGroup(isExpanded: $runbookOpen) {
                        Text(Self.markdown(runbook))
                            .font(LucinaTokens.Fonts.subheadline)
                            .foregroundStyle(Theme.text)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.top, LucinaTokens.Space.sm)
                    } label: {
                        Label("Runbook", systemImage: "book").font(LucinaTokens.Fonts.headline)
                    }
                }
            }
            PrimaryButton(title: "Ask Lucina", icon: "bubble.left.and.text.bubble.right") { ask() }
                .accessibilityIdentifier("serverAskLucina")
        }
    }

    /// The runbook's Markdown (inline styles and links; its line breaks kept).
    static func markdown(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
    }

    private func copy(_ command: String, _ message: String) {
        UIPasteboard.general.string = command
        toast = message
        AccessibilityNotification.Announcement(message).post()
    }

    /// The chat, with a message about this item ready to send.
    private func ask() {
        let text = ServerText.askLucina(item, hostName: hostName)
        dismiss()
        app.openWidgetRoute(ChatComposeRoute.route(text: text))
    }
}

/// A command in a monospaced, selectable box.
struct ServerCommandBox: View {
    let title: LocalizedStringKey
    let command: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(LucinaTokens.Fonts.caption).foregroundStyle(Theme.textSecondary)
            Text(verbatim: command)
                .font(.footnote.monospaced())
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(LucinaTokens.Space.md)
                .background(Theme.field, in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field, style: .continuous))
        }
    }
}

/// A catalog component: what it is, where it runs, its state; and the actions when it needs attention.
struct ServerComponentView: View {
    @Environment(ServerStore.self) private var store
    let ref: ServerComponentRef
    @Binding var toast: String?

    var body: some View {
        let component = ref.component
        let item = store.overview?.item(forComponent: component.id)
        let hostName = component.host.flatMap { store.overview?.host($0)?.name } ?? component.host
        ScrollView {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 8) {
                        ServerDot(state: component.state, size: 11)
                        Text(verbatim: component.state.label)
                            .font(LucinaTokens.Fonts.caption)
                            .foregroundStyle(ServerColors.state(component.state))
                    }
                    Text(verbatim: component.name).font(LucinaTokens.Fonts.title2)
                    Text(verbatim: component.id)
                        .font(LucinaTokens.Fonts.caption.monospaced())
                        .foregroundStyle(Theme.textSecondary)
                        .textSelection(.enabled)
                }
                .accessibilityElement(children: .combine)
                Card(spacing: LucinaTokens.Space.sm, cornerRadius: 16) {
                    ServerFact(label: "Kind", value: component.kind)
                    ServerFact(label: "Host", value: hostName)
                    ServerFact(label: "Project", value: "\(ref.group) · \(ref.project)")
                    ServerFact(label: "Criticality", value: component.criticality.map(ServerText.criticality))
                    ServerFact(label: "State", value: component.state.label)
                }
                if let description = component.description, !description.isEmpty {
                    Card(spacing: 6, cornerRadius: 16) {
                        Text("Description").font(LucinaTokens.Fonts.headline)
                        Text(verbatim: description).font(LucinaTokens.Fonts.subheadline).foregroundStyle(Theme.textSecondary)
                    }
                }
                if let item {
                    SectionHeader("Needs attention")
                    if let summary = item.summary, !summary.isEmpty {
                        Text(verbatim: summary).font(LucinaTokens.Fonts.body).foregroundStyle(Theme.text)
                    }
                    if let impact = item.impact, !impact.isEmpty { ServerImpactCard(impact: impact, down: item.severity == .crit) }
                    ServerItemActions(item: item, hostName: hostName, toast: $toast)
                }
            }
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.vertical, LucinaTokens.Space.lg)
        }
        .navigationTitle(Text(verbatim: component.name))
    }
}
