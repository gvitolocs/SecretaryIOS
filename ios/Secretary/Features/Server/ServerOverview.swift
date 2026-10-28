import Foundation

// Server (owner, 2026-10-28): the homelab at a glance. One endpoint, `GET /v1/system/overview` on secretary-server
// (contract: server-v2 contract.md), feeds this plugin, the web page and the widget. The JSON is snake_case; every
// field is decoded leniently, so a partial answer (a source down, an older server) still shows what it has, and a
// broken element never hides the others. All free text was redacted by the server: show it as is, add nothing raw.

// MARK: - Lenient decoding

/// One element of a list that is skipped (nil) when it does not decode.
private struct ServerFailable<T: Decodable>: Decodable {
    let value: T?
    init(from decoder: Decoder) throws { value = try? T(from: decoder) }
}

extension KeyedDecodingContainer {
    /// A list whose broken elements are dropped; empty when missing, null or mistyped.
    func serverList<T: Decodable>(_ type: T.Type, _ key: Key) -> [T] {
        ((try? decodeIfPresent([ServerFailable<T>].self, forKey: key)) ?? nil)?.compactMap(\.value) ?? []
    }

    /// An integer that may arrive as `142` or `142.0`.
    func serverInt(_ key: Key) -> Int? {
        if let i = lenientOptional(Int.self, key) { return i }
        return lenientOptional(Double.self, key).flatMap { Int(exactly: $0) }
    }
}

// MARK: - State

/// ok | warn | crit | idle | unknown (catalog `effective` mapped by the server).
enum ServerHealth: String, Codable, Sendable, Comparable {
    case ok, warn, crit, idle, unknown

    init(raw: String?) { self = raw.flatMap(ServerHealth.init(rawValue:)) ?? .unknown }

    /// 0 = worst.
    var rank: Int {
        switch self {
        case .crit: 0
        case .warn: 1
        case .unknown: 2
        case .ok: 3
        case .idle: 4
        }
    }

    var isProblem: Bool { self == .crit || self == .warn }
    var isHealthy: Bool { self == .ok || self == .idle }

    static func < (a: ServerHealth, b: ServerHealth) -> Bool { a.rank < b.rank }

    /// The text equivalent of the coloured dot.
    var label: String {
        switch self {
        case .ok: String(localized: "OK")
        case .warn: String(localized: "Warning")
        case .crit: String(localized: "Critical")
        case .idle: String(localized: "Waiting")
        case .unknown: String(localized: "Unknown")
        }
    }
}

enum ServerDates {
    static func parse(_ iso: String?) -> Date? {
        guard let iso, !iso.isEmpty else { return nil }
        if let d = try? Date(iso, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true)) { return d }
        return try? Date(iso, strategy: Date.ISO8601FormatStyle())
    }
}

// MARK: - The overview

struct ServerOverview: Codable, Equatable, Sendable {
    var generatedAt: String?
    var overall: ServerHealth
    var attentionCount: Int
    /// Running now, but failed or restarted in the last 24 h: shown apart, never counted as needing attention.
    var flappingCount: Int
    var checksOk: Int?
    var checksTotal: Int?
    var sources: [String: ServerSource]
    var hosts: [ServerHost]
    var attention: [ServerAttention]
    var services: [ServerGroup]
    var timeline: [ServerEvent]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        generatedAt = c.lenientOptional(String.self, .generatedAt)
        overall = ServerHealth(raw: c.lenientOptional(String.self, .overall))
        hosts = c.serverList(ServerHost.self, .hosts)
        attention = c.serverList(ServerAttention.self, .attention)
        attentionCount = c.serverInt(.attentionCount) ?? attention.filter { !$0.flapping }.count
        flappingCount = c.serverInt(.flappingCount) ?? attention.filter(\.flapping).count
        checksOk = c.serverInt(.checksOk)
        checksTotal = c.serverInt(.checksTotal)
        sources = c.lenientOptional([String: ServerFailable<ServerSource>].self, .sources)?.compactMapValues(\.value) ?? [:]
        services = c.serverList(ServerGroup.self, .services)
        timeline = c.serverList(ServerEvent.self, .timeline)
    }

    private enum CodingKeys: String, CodingKey {
        case generatedAt = "generated_at", overall, attentionCount = "attention_count", flappingCount = "flapping_count", checksOk = "checks_ok"
        case checksTotal = "checks_total", sources, hosts, attention, services, timeline
    }

    var generatedDate: Date? { ServerDates.parse(generatedAt) }

    /// The items that need the owner (not the ones that only restarted earlier).
    var needItems: [ServerAttention] { attention.filter { !$0.flapping } }

    /// Running, but restarted in the last 24 h.
    var flappingItems: [ServerAttention] { attention.filter(\.flapping) }

    /// What needs the owner: the attention items, else (no item but a host is down or hot) those hosts.
    var needCount: Int {
        let items = max(attentionCount, needItems.count)
        if items > 0 { return items }
        return overall == .ok ? 0 : hosts.filter { $0.state.isProblem }.count
    }

    /// Sources that failed (`ops: Lucina unreachable`), for a quiet note.
    var failedSources: [(name: String, error: String?)] {
        sources.filter { !$0.value.ok }.sorted { $0.key < $1.key }.map { source -> (name: String, error: String?) in (source.key, source.value.error) }
    }

    // MARK: Lookups

    func item(key: String) -> ServerAttention? { attention.first { $0.key == key } }

    /// The attention item of a catalog component: `catalog:<id>`, else any item naming that component.
    func item(forComponent id: String) -> ServerAttention? {
        item(key: "catalog:\(id)") ?? attention.first { $0.componentId == id }
    }

    func host(_ id: String) -> ServerHost? { hosts.first { $0.id == id } }

    /// A catalog component with the group and project it belongs to.
    func component(_ id: String) -> ServerComponentRef? {
        for group in services {
            for project in group.projects {
                if let component = project.components.first(where: { $0.id == id }) {
                    return ServerComponentRef(component: component, group: group.name, project: project.id)
                }
            }
        }
        return nil
    }

    var allComponents: [ServerComponentRef] {
        services.flatMap { group in
            group.projects.flatMap { project in
                project.components.map { ServerComponentRef(component: $0, group: group.name, project: project.id) }
            }
        }
    }

    func components(onHost id: String) -> [ServerComponentRef] {
        allComponents.filter { $0.component.host == id }.sorted { ($0.component.state, $0.component.name) < ($1.component.state, $1.component.name) }
    }

    func attention(onHost id: String) -> [ServerAttention] { attention.filter { $0.host == id } }

    /// Healthy components (ok or idle) by group, for "All good · N checks".
    var healthyByGroup: [ServerHealthyGroup] {
        services.compactMap { group -> ServerHealthyGroup? in
            let healthy = group.projects.flatMap { project in
                project.components.filter(\.state.isHealthy).map { ServerComponentRef(component: $0, group: group.name, project: project.id) }
            }
            return healthy.isEmpty ? nil : ServerHealthyGroup(group: group.name, components: healthy.sorted { $0.component.name < $1.component.name })
        }
    }

    /// Incidents and state changes (Incidents tab), or deploys and decisions (Deploys tab), newest first.
    func events(_ tab: ServerTab) -> [ServerEvent] {
        switch tab {
        case .incidents: timeline.filter { $0.kind == "incident" || $0.kind == "change" }
        case .deploys: timeline.filter { $0.kind == "deploy" || $0.kind == "decision" }
        default: timeline
        }
    }
}

struct ServerSource: Codable, Equatable, Sendable {
    var ok: Bool
    var at: String?
    var error: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ok = c.lenientOptional(Bool.self, .ok) ?? false
        at = c.lenientOptional(String.self, .at)
        error = c.lenientOptional(String.self, .error)
    }

    private enum CodingKeys: String, CodingKey { case ok, at, error }
}

// MARK: - Hosts

struct ServerHost: Codable, Equatable, Identifiable, Sendable {
    let id: String
    var name: String
    var role: String?
    var state: ServerHealth
    var reachable: Bool?
    var limited: Bool
    var lastReport: String?
    var uptime: String?
    var os: String?
    var kernel: String?
    var cpus: Int?
    var load: [Double]?
    var memUsedPct: Double?
    var tctl: Double?
    var disks: [ServerDisk]
    var history: ServerHistory?
    var findings: [String]
    var componentId: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = c.lenientOptional(String.self, .name) ?? id
        role = c.lenientOptional(String.self, .role)
        state = ServerHealth(raw: c.lenientOptional(String.self, .state))
        reachable = c.lenientOptional(Bool.self, .reachable)
        limited = c.lenientOptional(Bool.self, .limited) ?? false
        lastReport = c.lenientOptional(String.self, .lastReport)
        uptime = c.lenientOptional(String.self, .uptime)
        os = c.lenientOptional(String.self, .os)
        kernel = c.lenientOptional(String.self, .kernel)
        cpus = c.serverInt(.cpus)
        load = c.lenientOptional([Double].self, .load)
        memUsedPct = c.lenientOptional(Double.self, .memUsedPct)
        tctl = c.lenientOptional(Double.self, .tctl)
        disks = c.serverList(ServerDisk.self, .disks)
        history = c.lenientOptional(ServerHistory.self, .history)
        findings = c.lenientOptional([String].self, .findings) ?? []
        componentId = c.lenientOptional(String.self, .componentId)
    }

    private enum CodingKeys: String, CodingKey {
        case id, name, role, state, reachable, limited, lastReport = "last_report", uptime, os, kernel, cpus, load
        case memUsedPct = "mem_used_pct", tctl, disks, history, findings, componentId = "component_id"
    }

    var lastReportDate: Date? { ServerDates.parse(lastReport) }

    /// "Ubuntu 26.04.1 LTS · Linux 7.0.0-42-generic"
    var systemLine: String? {
        let parts = [os, kernel].compactMap { $0?.isEmpty == false ? $0 : nil }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    var load1: Double? { load?.first }
}

struct ServerDisk: Codable, Equatable, Identifiable, Sendable {
    var mount: String
    var usedPct: Double?
    var freeGb: Double?
    var sizeGb: Double?

    var id: String { mount }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        mount = c.lenientOptional(String.self, .mount) ?? "?"
        usedPct = c.lenientOptional(Double.self, .usedPct)
        freeGb = c.lenientOptional(Double.self, .freeGb)
        sizeGb = c.lenientOptional(Double.self, .sizeGb)
    }

    private enum CodingKeys: String, CodingKey { case mount, usedPct = "used_pct", freeGb = "free_gb", sizeGb = "size_gb" }
}

/// One sample of a host's 24-hour history.
struct ServerPoint: Identifiable, Equatable, Sendable {
    let id: Int
    let date: Date
    let value: Double
    /// Points with a gap (empty buckets) between them are in different segments, so the line breaks.
    var segment = 0
}

/// The last 24 h in 15-minute buckets, oldest first; nil = no sample.
struct ServerHistory: Codable, Equatable, Sendable {
    var stepS: Int
    var end: String?
    var tctl: [Double?]
    var load1: [Double?]
    var mem: [Double?]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        stepS = max(1, c.serverInt(.stepS) ?? 900)
        end = c.lenientOptional(String.self, .end)
        tctl = c.lenientOptional([Double?].self, .tctl) ?? []
        load1 = c.lenientOptional([Double?].self, .load1) ?? []
        mem = c.lenientOptional([Double?].self, .mem) ?? []
    }

    private enum CodingKeys: String, CodingKey { case stepS = "step_s", end, tctl, load1, mem }

    /// The 24 h the buckets cover, ending at `end`.
    func range(now: Date = Date()) -> ClosedRange<Date> {
        let last = ServerDates.parse(end) ?? now
        let span = Double(max(tctl.count, load1.count, mem.count, 1)) * Double(stepS)
        return last.addingTimeInterval(-span)...last
    }

    /// The samples with their times (the last bucket ends at `end`); empty buckets are left out
    /// and start a new segment.
    func points(_ values: [Double?], now: Date = Date()) -> [ServerPoint] {
        let last = ServerDates.parse(end) ?? now
        let count = values.count
        var segment = 0
        var previous: Int?
        return values.enumerated().compactMap { index, value in
            guard let value, value.isFinite else { return nil }
            if let previous, index - previous > 1 { segment += 1 }
            previous = index
            return ServerPoint(id: index, date: last.addingTimeInterval(-Double(count - 1 - index) * Double(stepS)), value: value, segment: segment)
        }
    }
}

// MARK: - Attention

struct ServerAttention: Codable, Equatable, Identifiable, Sendable {
    let key: String
    var sources: [String]
    var severity: ServerHealth
    var title: String
    var componentId: String?
    var host: String?
    var group: String?
    var project: String?
    var kind: String?
    var criticality: String?
    var summary: String?
    var details: [String]
    var since: String?
    var failures24h: Int?
    /// Running now; only failed or restarted earlier.
    var flapping: Bool
    var impact: ServerImpact?
    var caused: [String]
    var logs: String?
    var restart: String?
    var runbook: String?

    var id: String { key }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        key = try c.decode(String.self, forKey: .key)
        sources = c.lenientOptional([String].self, .sources) ?? []
        let severity = ServerHealth(raw: c.lenientOptional(String.self, .severity))
        self.severity = severity.isProblem ? severity : .warn
        componentId = c.lenientOptional(String.self, .componentId)
        title = c.lenientOptional(String.self, .title) ?? componentId ?? key
        host = c.lenientOptional(String.self, .host)
        group = c.lenientOptional(String.self, .group)
        project = c.lenientOptional(String.self, .project)
        kind = c.lenientOptional(String.self, .kind)
        criticality = c.lenientOptional(String.self, .criticality)
        summary = c.lenientOptional(String.self, .summary)
        details = c.lenientOptional([String].self, .details) ?? []
        since = c.lenientOptional(String.self, .since)
        failures24h = c.serverInt(.failures24h)
        flapping = c.lenientOptional(Bool.self, .flapping) ?? false
        impact = c.lenientOptional(ServerImpact.self, .impact)
        caused = c.lenientOptional([String].self, .caused) ?? []
        logs = c.lenientOptional(String.self, .logs)
        restart = c.lenientOptional(String.self, .restart)
        runbook = c.lenientOptional(String.self, .runbook)
    }

    private enum CodingKeys: String, CodingKey {
        case key, sources, severity, title, componentId = "component_id", host, group, project, kind, criticality, summary
        case details, since, failures24h = "failures_24h", flapping, impact, caused, logs, restart, runbook
    }

    var sinceDate: Date? { ServerDates.parse(since) }

    /// How many things break or degrade while this is down.
    var impactCount: Int { (impact?.broken.count ?? 0) + (impact?.degraded.count ?? 0) }
}

struct ServerImpact: Codable, Equatable, Sendable {
    var broken: [ServerRef]
    var degraded: [ServerRef]
    var publicHostnamesBroken: [String]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        broken = c.serverList(ServerRef.self, .broken)
        degraded = c.serverList(ServerRef.self, .degraded)
        publicHostnamesBroken = c.lenientOptional([String].self, .publicHostnamesBroken) ?? []
    }

    private enum CodingKeys: String, CodingKey { case broken, degraded, publicHostnamesBroken = "public_hostnames_broken" }

    var isEmpty: Bool { broken.isEmpty && degraded.isEmpty && publicHostnamesBroken.isEmpty }
}

struct ServerRef: Codable, Equatable, Identifiable, Sendable {
    let id: String
    var name: String

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = c.lenientOptional(String.self, .name) ?? id
    }

    private enum CodingKeys: String, CodingKey { case id, name }
}

// MARK: - Services

struct ServerGroup: Codable, Equatable, Identifiable, Sendable {
    var name: String
    var state: ServerHealth
    var projects: [ServerProject]

    var id: String { name }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = c.lenientOptional(String.self, .name) ?? String(localized: "Other")
        state = ServerHealth(raw: c.lenientOptional(String.self, .state))
        projects = c.serverList(ServerProject.self, .projects)
    }

    private enum CodingKeys: String, CodingKey { case name, state, projects }
}

struct ServerProject: Codable, Equatable, Identifiable, Sendable {
    let id: String
    var state: ServerHealth
    var components: [ServerComponent]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenientOptional(String.self, .id) ?? "—"
        state = ServerHealth(raw: c.lenientOptional(String.self, .state))
        components = c.serverList(ServerComponent.self, .components)
    }

    private enum CodingKeys: String, CodingKey { case id, state, components }
}

struct ServerComponent: Codable, Equatable, Identifiable, Sendable {
    let id: String
    var name: String
    var kind: String?
    var host: String?
    var state: ServerHealth
    var criticality: String?
    var description: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = c.lenientOptional(String.self, .name) ?? id
        kind = c.lenientOptional(String.self, .kind)
        host = c.lenientOptional(String.self, .host)
        state = ServerHealth(raw: c.lenientOptional(String.self, .state))
        criticality = c.lenientOptional(String.self, .criticality)
        description = c.lenientOptional(String.self, .description)
    }

    private enum CodingKeys: String, CodingKey { case id, name, kind, host, state, criticality, description }
}

/// A group's healthy components ("All good").
struct ServerHealthyGroup: Identifiable, Equatable, Sendable {
    let group: String
    let components: [ServerComponentRef]
    var id: String { group }
}

/// A component with where it sits in the tree.
struct ServerComponentRef: Equatable, Identifiable, Sendable {
    let component: ServerComponent
    let group: String
    let project: String

    var id: String { component.id }

    /// Services search: id, name, host or project.
    func matches(_ query: String) -> Bool {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return true }
        return [component.id, component.name, component.host ?? "", project].contains { $0.localizedCaseInsensitiveContains(q) }
    }
}

// MARK: - Timeline

struct ServerEvent: Codable, Equatable, Sendable {
    var at: String
    var kind: String
    var title: String
    var project: String?
    var host: String?
    var severity: String?
    var from: String?
    var to: String?
    var detail: String?
    var opsId: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        at = c.lenientOptional(String.self, .at) ?? ""
        kind = c.lenientOptional(String.self, .kind) ?? "change"
        title = c.lenientOptional(String.self, .title) ?? ""
        project = c.lenientOptional(String.self, .project)
        host = c.lenientOptional(String.self, .host)
        severity = c.lenientOptional(String.self, .severity)
        from = c.lenientOptional(String.self, .from)
        to = c.lenientOptional(String.self, .to)
        detail = c.lenientOptional(String.self, .detail)
        opsId = c.lenientOptional(String.self, .opsId)
    }

    private enum CodingKeys: String, CodingKey { case at, kind, title, project, host, severity, from, to, detail, opsId = "ops_id" }

    var date: Date? { ServerDates.parse(at) }

    var symbol: String {
        switch kind {
        case "incident": "exclamationmark.triangle.fill"
        case "deploy": "shippingbox.fill"
        case "decision": "checkmark.seal.fill"
        default: "arrow.left.arrow.right"
        }
    }
}

// MARK: - Screen state

enum ServerTab: String, CaseIterable, Identifiable, Sendable {
    case overview, services, incidents, deploys

    var id: String { rawValue }

    var title: String {
        switch self {
        case .overview: String(localized: "Overview")
        case .services: String(localized: "Services")
        case .incidents: String(localized: "Incidents")
        case .deploys: String(localized: "Deploys")
        }
    }
}

/// The sheet over the Server screen: an attention item (by key) or a component (by id), looked up in the
/// current data so a refresh updates it.
enum ServerSheet: Identifiable, Hashable, Sendable {
    case item(String)
    case component(String)

    var id: String {
        switch self {
        case let .item(key): "item:\(key)"
        case let .component(id): "component:\(id)"
        }
    }
}

/// What a push or a link asks the Server screen to open.
enum ServerTarget: Equatable, Sendable {
    /// `?item=<key>`
    case item(String)
    /// `/agents/map/<component id>`: its attention item if any, else the component.
    case component(String)
    /// `?host=<id>`
    case host(String)

    /// Where the target lands in `overview`; nil when it is not there (yet).
    func resolve(in overview: ServerOverview) -> ServerResolution? {
        switch self {
        case let .item(key):
            if overview.item(key: key) != nil { return .sheet(.item(key)) }
            if key.hasPrefix("catalog:") { return ServerTarget.component(String(key.dropFirst("catalog:".count))).resolve(in: overview) }
            return nil
        case let .component(id):
            if let item = overview.item(forComponent: id) { return .sheet(.item(item.key)) }
            if overview.component(id) != nil { return .sheet(.component(id)) }
            return nil
        case let .host(id):
            return overview.host(id) != nil ? .host(id) : nil
        }
    }
}

enum ServerResolution: Equatable, Sendable {
    case sheet(ServerSheet)
    case host(String)
}

/// The app routes that open the Server plugin (pushes, widget taps, Lucina's buttons):
///   server-health[/services|/incidents|/deploys][?item=<key>|?host=<id>]
///   agents/map/<component id>        (the catalog's outage push)
struct ServerLink: Equatable, Sendable {
    var tab: ServerTab?
    var target: ServerTarget?

    static func parse(_ route: String) -> ServerLink? {
        let trimmed = route.hasPrefix("/") ? String(route.dropFirst()) : route
        let pieces = trimmed.split(separator: "?", maxSplits: 1).map(String.init)
        let path = pieces.first ?? ""
        let parts = path.split(separator: "/").map { String($0).removingPercentEncoding ?? String($0) }
        var query: [String: String] = [:]
        if pieces.count > 1 {
            for pair in pieces[1].split(separator: "&") {
                let kv = pair.split(separator: "=", maxSplits: 1).map(String.init)
                guard let name = kv.first, kv.count == 2 else { continue }
                let value = kv[1].replacingOccurrences(of: "+", with: " ")
                query[name] = value.removingPercentEncoding ?? value
            }
        }
        switch parts.first {
        case "server-health":
            var link = ServerLink(tab: .overview)
            if parts.count >= 2 { link.tab = ServerTab(rawValue: parts[1]) }
            if let item = query["item"], !item.isEmpty {
                link.target = .item(item)
            } else if let host = query["host"], !host.isEmpty {
                link.target = .host(host)
            }
            return link
        case "agents":
            guard parts.count >= 3, parts[1] == "map", !parts[2].isEmpty else { return nil }
            return ServerLink(tab: .overview, target: .component(parts[2]))
        default:
            return nil
        }
    }
}

// MARK: - Texts

enum ServerText {
    /// The Plugins card line.
    static func pluginDetail(_ overview: ServerOverview?) -> String {
        guard let overview else { return String(localized: "Hosts, services, incidents") }
        let n = overview.needCount
        if n == 0, overview.overall == .unknown { return String(localized: "No data yet") }
        if n == 0, overview.overall == .crit { return String(localized: "Server unreachable") }
        return n == 0 ? String(localized: "All systems normal") : String(localized: "\(n) need attention")
    }

    /// The header line: all normal, N need attention, or unreachable (no data and the last load failed).
    static func status(_ overview: ServerOverview?, failed: Bool) -> String {
        guard let overview else { return failed ? String(localized: "Server unreachable") : String(localized: "Loading…") }
        return pluginDetail(overview)
    }

    /// "now", "1 min ago", "2 hr ago".
    static func ago(_ date: Date?, now: Date = Date()) -> String? {
        guard let date else { return nil }
        if abs(now.timeIntervalSince(date)) < 45 { return String(localized: "now") }
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .short
        return f.localizedString(for: date, relativeTo: now)
    }

    /// "2h", "3d" (how long something has been wrong).
    static func duration(since date: Date?, now: Date = Date()) -> String? {
        guard let date else { return nil }
        let f = DateComponentsFormatter()
        f.unitsStyle = .abbreviated
        f.maximumUnitCount = 1
        f.allowedUnits = [.day, .hour, .minute]
        return f.string(from: max(60, now.timeIntervalSince(date)))
    }

    static func number(_ value: Double, digits: Int = 0) -> String {
        value.formatted(.number.precision(.fractionLength(digits)))
    }

    static func gigabytes(_ gb: Double) -> String {
        gb >= 1000 ? "\(number(gb / 1000, digits: 1)) TB" : "\(number(gb)) GB"
    }

    /// The message "Ask Lucina" pre-fills: what is wrong, where, since when, what it breaks, the logs command.
    static func askLucina(_ item: ServerAttention, hostName: String?, now: Date = Date()) -> String {
        var lines: [String] = []
        let host = hostName ?? item.host
        if let host {
            lines.append(String(localized: "The server reports a problem with \(item.title) on \(host) (\(item.severity.label))."))
        } else {
            lines.append(String(localized: "The server reports a problem with \(item.title) (\(item.severity.label))."))
        }
        if let summary = item.summary, !summary.isEmpty { lines.append(summary) }
        if let since = item.sinceDate {
            let when = since.formatted(date: .abbreviated, time: .shortened)
            let length = duration(since: since, now: now) ?? ""
            lines.append(String(localized: "Since \(when) (\(length))."))
        }
        if let impact = item.impact {
            let names = (impact.broken + impact.degraded).map(\.name) + impact.publicHostnamesBroken
            if !names.isEmpty { lines.append(String(localized: "Affects: \(names.joined(separator: ", ")).")) }
        }
        if let logs = item.logs, !logs.isEmpty { lines.append(String(localized: "Logs: \(logs)")) }
        lines.append(String(localized: "What's wrong and how do I fix it?"))
        return lines.joined(separator: "\n")
    }
}
