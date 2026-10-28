import XCTest
@testable import Secretary

/// The Server plugin: `GET /v1/system/overview` (server-v2 contract), its routes, lookups and texts.
@MainActor
final class ServerTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    // MARK: Fixtures

    nonisolated static let fullJSON = """
    {
      "generated_at": "2026-10-28T21:50:00Z",
      "overall": "crit",
      "attention_count": 2,
      "checks_ok": 122,
      "checks_total": 150,
      "sources": {
        "catalog":  { "ok": true,  "at": "2026-10-28T21:45:00Z", "error": null },
        "sentinel": { "ok": true,  "at": "2026-10-28T21:48:00Z", "error": null },
        "ops":      { "ok": false, "at": null, "error": "Lucina unreachable" }
      },
      "hosts": [{
        "id": "nezopt", "name": "nezopt", "role": "home server", "state": "warn", "reachable": true, "limited": false,
        "last_report": "2026-10-28T21:45:00Z", "uptime": "23d 4h", "os": "Ubuntu 26.04.1 LTS", "kernel": "Linux 7.0.0-42-generic",
        "cpus": 16, "load": [9.58, 5.03, 2.48], "mem_used_pct": 33.3, "tctl": 95.1,
        "disks": [{ "mount": "/", "used_pct": 77.6, "free_gb": 422.0, "size_gb": 1885.0 }],
        "history": { "step_s": 900, "end": "2026-10-28T22:00:00Z",
                     "tctl": [70.1, null, 80.0, 95.1], "load1": [1.2, 2.0, null, 9.58], "mem": [31.0, 32.0, 33.0, 33.3] },
        "findings": ["Tctl 95 °C"], "component_id": "nezopt"
      }, {
        "id": "nespc", "name": "nespc", "role": null, "state": "crit", "reachable": false, "limited": true,
        "last_report": null, "uptime": null, "os": null, "kernel": null, "cpus": null, "load": null, "mem_used_pct": null,
        "tctl": null, "disks": [], "history": null, "findings": [], "component_id": null
      }],
      "attention": [{
        "key": "catalog:gmaps-server", "sources": ["catalog", "sentinel"], "severity": "crit", "title": "gmaps-server",
        "component_id": "gmaps-server", "host": "nezopt", "group": "Lucina", "project": "secretary", "kind": "service",
        "criticality": "high", "summary": "Not running; port 18210 not listening",
        "details": ["user unit gmaps-server.service: inactive/dead", "port 18210: not listening"],
        "since": "2026-10-28T19:16:04Z", "failures_24h": 142,
        "impact": { "broken": [{ "id": "lucina-maps", "name": "Lucina Maps" }], "degraded": [], "public_hostnames_broken": ["maps.example.com"] },
        "caused": ["lucina-maps"], "logs": "journalctl --user -u gmaps-server -n 100 --no-pager",
        "restart": "systemctl --user restart gmaps-server", "runbook": "**Check** the port."
      }, {
        "key": "sentinel:nezopt:unit:marketplace-rails-sync", "sources": ["sentinel"], "severity": "warn",
        "title": "marketplace-rails-sync", "component_id": null, "host": "nezopt", "group": null, "project": null, "kind": null,
        "criticality": null, "summary": "Failed 3 times", "details": [], "since": "2026-10-28T20:00:00Z", "failures_24h": null,
        "impact": null, "caused": [], "logs": null, "restart": null, "runbook": null
      }],
      "services": [{
        "name": "Lucina", "state": "crit",
        "projects": [{
          "id": "secretary", "state": "crit",
          "components": [
            { "id": "gmaps-server", "name": "Google Maps proxy", "kind": "service", "host": "nezopt", "state": "crit", "criticality": "high", "description": "Maps for Lucina" },
            { "id": "secretary-gateways", "name": "Secretary plugins", "kind": "service", "host": "nezopt", "state": "ok", "criticality": "low", "description": "…" },
            { "id": "backup-timer", "name": "Backups", "kind": "timer", "host": "p1", "state": "idle", "criticality": "medium", "description": null }
          ]
        }]
      }],
      "timeline": [
        { "at": "2026-10-28T21:39:00Z", "kind": "deploy", "title": "nezopt upgraded to Ubuntu 26.04.1 LTS", "project": "nezopt", "host": "nezopt",
          "severity": null, "from": null, "to": null, "detail": null, "ops_id": "e8e10d40c994d9f0" },
        { "at": "2026-10-28T19:16:04Z", "kind": "change", "title": "gmaps-server", "project": null, "host": "nezopt",
          "severity": null, "from": "ok", "to": "crit", "detail": null, "ops_id": null },
        { "at": "2026-10-27T08:00:00Z", "kind": "incident", "title": "Disk full on p1", "project": "p1", "host": "p1",
          "severity": "high", "from": null, "to": null, "detail": "one line", "ops_id": "abc" }
      ]
    }
    """

    /// A source down, fields missing or of a newer shape, and broken elements next to good ones.
    nonisolated static let partialJSON = """
    {
      "overall": "warn",
      "hosts": [{ "id": "p1" }, { "name": "no id" }, { "id": "mac", "state": "galaxy", "load": "high", "history": { "tctl": [null, 50] } }],
      "attention": [{ "key": "sentinel:p1:disk", "severity": "weird", "future": { "x": 1 } }, { "title": "no key" }],
      "services": "not a list",
      "sources": { "catalog": { "ok": false, "error": "timeout" }, "broken": 7 }
    }
    """

    private func full() throws -> ServerOverview { try Fixtures.decode(ServerOverview.self, Self.fullJSON) }

    // MARK: Decoding

    func testDecodesFullOverview() throws {
        let o = try full()
        XCTAssertEqual(o.overall, .crit)
        XCTAssertEqual(o.attentionCount, 2)
        XCTAssertEqual(o.checksOk, 122)
        XCTAssertEqual(o.checksTotal, 150)
        XCTAssertEqual(o.generatedDate, ServerDates.parse("2026-10-28T21:50:00Z"))
        XCTAssertEqual(o.failedSources.map(\.name), ["ops"])
        XCTAssertEqual(o.failedSources.first?.error, "Lucina unreachable")

        XCTAssertEqual(o.hosts.map(\.id), ["nezopt", "nespc"])
        let nezopt = try XCTUnwrap(o.host("nezopt"))
        XCTAssertEqual(nezopt.role, "home server")
        XCTAssertEqual(nezopt.state, .warn)
        XCTAssertEqual(nezopt.cpus, 16)
        XCTAssertEqual(nezopt.load1, 9.58)
        XCTAssertEqual(nezopt.memUsedPct, 33.3)
        XCTAssertEqual(nezopt.tctl, 95.1)
        XCTAssertEqual(nezopt.systemLine, "Ubuntu 26.04.1 LTS · Linux 7.0.0-42-generic")
        XCTAssertEqual(nezopt.disks.first?.usedPct, 77.6)
        XCTAssertEqual(nezopt.findings, ["Tctl 95 °C"])
        let history = try XCTUnwrap(nezopt.history)
        XCTAssertEqual(history.tctl.count, 4)
        let temps = history.points(history.tctl)
        XCTAssertEqual(temps.map(\.value), [70.1, 80.0, 95.1], "empty buckets are left out")
        XCTAssertEqual(temps.last?.date, ServerDates.parse("2026-10-28T22:00:00Z"))
        XCTAssertEqual(temps.first?.date, ServerDates.parse("2026-10-28T21:15:00Z"))
        let nespc = try XCTUnwrap(o.host("nespc"))
        XCTAssertTrue(nespc.limited)
        XCTAssertEqual(nespc.reachable, false)
        XCTAssertNil(nespc.history)

        let item = try XCTUnwrap(o.item(key: "catalog:gmaps-server"))
        XCTAssertEqual(item.severity, .crit)
        XCTAssertEqual(item.sources, ["catalog", "sentinel"])
        XCTAssertEqual(item.failures24h, 142)
        XCTAssertEqual(item.impact?.broken.map(\.name), ["Lucina Maps"])
        XCTAssertEqual(item.impact?.publicHostnamesBroken, ["maps.example.com"])
        XCTAssertEqual(item.impactCount, 1)
        XCTAssertEqual(item.caused, ["lucina-maps"])
        XCTAssertEqual(item.logs, "journalctl --user -u gmaps-server -n 100 --no-pager")
        XCTAssertEqual(item.restart, "systemctl --user restart gmaps-server")
        XCTAssertNotNil(item.sinceDate)

        XCTAssertEqual(o.allComponents.count, 3)
        XCTAssertEqual(o.component("backup-timer")?.component.state, .idle)
        XCTAssertEqual(o.component("backup-timer")?.group, "Lucina")
        XCTAssertEqual(o.healthyByGroup.first?.components.map(\.id), ["backup-timer", "secretary-gateways"])
        XCTAssertEqual(o.components(onHost: "nezopt").map(\.id), ["gmaps-server", "secretary-gateways"], "worst first")
        XCTAssertEqual(o.attention(onHost: "nezopt").count, 2)

        XCTAssertEqual(o.timeline.count, 3)
        XCTAssertEqual(o.events(.deploys).map(\.title), ["nezopt upgraded to Ubuntu 26.04.1 LTS"])
        XCTAssertEqual(o.events(.incidents).map(\.kind), ["change", "incident"])
        XCTAssertEqual(o.timeline[1].from, "ok")
        XCTAssertEqual(o.timeline[1].to, "crit")
    }

    func testDecodesPartialOverview() throws {
        let o = try Fixtures.decode(ServerOverview.self, Self.partialJSON)
        XCTAssertEqual(o.overall, .warn)
        XCTAssertNil(o.generatedAt)
        XCTAssertEqual(o.hosts.map(\.id), ["p1", "mac"], "a host without an id is dropped, the others stay")
        XCTAssertEqual(o.host("p1")?.name, "p1")
        XCTAssertEqual(o.host("p1")?.state, .unknown)
        XCTAssertEqual(o.host("mac")?.state, .unknown)
        XCTAssertNil(o.host("mac")?.load)
        XCTAssertEqual(o.host("mac")?.history?.stepS, 900)
        XCTAssertEqual(o.host("mac")?.history?.tctl.count, 2)
        XCTAssertEqual(o.attention.map(\.key), ["sentinel:p1:disk"])
        XCTAssertEqual(o.attention.first?.severity, .warn, "an unknown severity still shows as a problem")
        XCTAssertEqual(o.attention.first?.title, "sentinel:p1:disk")
        XCTAssertEqual(o.attentionCount, 1, "missing count: the items")
        XCTAssertTrue(o.services.isEmpty)
        XCTAssertTrue(o.timeline.isEmpty)
        XCTAssertEqual(o.failedSources.map(\.name), ["catalog"])

        // Nothing at all is still an overview (unknown, empty).
        let empty = try Fixtures.decode(ServerOverview.self, "{}")
        XCTAssertEqual(empty.overall, .unknown)
        XCTAssertEqual(empty.needCount, 1 - 1 + 0)
    }

    // MARK: Texts

    func testPluginDetail() throws {
        XCTAssertEqual(ServerText.pluginDetail(nil), String(localized: "Hosts, services, incidents"))
        XCTAssertEqual(ServerText.pluginDetail(try full()), String(localized: "\(2) need attention"))
        let ok = try Fixtures.decode(ServerOverview.self, #"{"overall":"ok","attention_count":0,"attention":[]}"#)
        XCTAssertEqual(ServerText.pluginDetail(ok), String(localized: "All systems normal"))
        // No item, but a host is down: that host needs attention.
        let hostDown = try Fixtures.decode(ServerOverview.self, #"{"overall":"crit","attention_count":0,"hosts":[{"id":"a","state":"crit"},{"id":"b","state":"ok"}]}"#)
        XCTAssertEqual(hostDown.needCount, 1)
        XCTAssertEqual(ServerText.status(nil, failed: true), String(localized: "Server unreachable"))
    }

    func testAskLucinaMessage() throws {
        let item = try XCTUnwrap(try full().item(key: "catalog:gmaps-server"))
        let text = ServerText.askLucina(item, hostName: "nezopt", now: try XCTUnwrap(ServerDates.parse("2026-10-28T21:16:04Z")))
        XCTAssertTrue(text.contains("gmaps-server"))
        XCTAssertTrue(text.contains("nezopt"))
        XCTAssertTrue(text.contains("Not running; port 18210 not listening"))
        XCTAssertTrue(text.contains("Lucina Maps"))
        XCTAssertTrue(text.contains("journalctl --user -u gmaps-server -n 100 --no-pager"))
        XCTAssertFalse(text.contains("systemctl"), "the restart command is not sent")
    }

    func testMetricAccessibilityLabel() throws {
        let host = try XCTUnwrap(try full().host("nezopt"))
        let history = try XCTUnwrap(host.history)
        let label = ServerMetric.temperature.accessibilityLabel(points: ServerMetric.temperature.points(history), current: host.tctl)
        XCTAssertTrue(label.contains("95 °C"), label)
        XCTAssertTrue(label.contains("70–95"), label)
        XCTAssertEqual(ServerMetric.temperature.domain(ServerMetric.temperature.points(history), withLimit: true).upperBound, 97.1, accuracy: 0.001)
    }

    // MARK: Routes

    func testRouteParsing() {
        XCTAssertEqual(ServerLink.parse("/server-health"), ServerLink(tab: nil, target: nil))
        XCTAssertEqual(ServerLink.parse("server-health?item=catalog:x"), ServerLink(tab: nil, target: .item("catalog:x")))
        XCTAssertEqual(ServerLink.parse("/server-health?item=sentinel%3Anezopt%3Aunit%3Ax"), ServerLink(tab: nil, target: .item("sentinel:nezopt:unit:x")))
        XCTAssertEqual(ServerLink.parse("/server-health?host=nezopt"), ServerLink(tab: nil, target: .host("nezopt")))
        XCTAssertEqual(ServerLink.parse("/server-health/incidents"), ServerLink(tab: .incidents, target: nil))
        XCTAssertEqual(ServerLink.parse("agents/map/x"), ServerLink(tab: .overview, target: .component("x")))
        XCTAssertEqual(ServerLink.parse("/agents/map/gmaps-server"), ServerLink(tab: .overview, target: .component("gmaps-server")))
        XCTAssertNil(ServerLink.parse("/agents/map"))
        XCTAssertNil(ServerLink.parse("/agents"))
        XCTAssertNil(ServerLink.parse("/shopping"))

        XCTAssertEqual(NotificationRoute.tab(for: "/server-health?item=catalog:x"), .plugins)
        XCTAssertEqual(NotificationRoute.tab(for: "/agents/map/x"), .plugins)
        XCTAssertEqual(NotificationRoute.server("/agents/map/x")?.target, .component("x"))
        XCTAssertNil(NotificationRoute.server("//server-health"))
    }

    func testChatComposeRoute() {
        let text = "gmaps-server on nezopt\nLogs: journalctl --user -u x -n 100 & more?"
        let route = ChatComposeRoute.route(text: text)
        XCTAssertTrue(route.hasPrefix("chat/compose?text="))
        XCTAssertEqual(ChatComposeRoute.text(route), text)
        XCTAssertEqual(ChatComposeRoute.text(String(WidgetRoute.route(from: WidgetRoute.url(route)) ?? "")), text)
        XCTAssertNil(ChatComposeRoute.text("chat/compose"))
    }

    // MARK: Lookup

    func testItemLookup() throws {
        let o = try full()
        XCTAssertEqual(ServerTarget.item("catalog:gmaps-server").resolve(in: o), .sheet(.item("catalog:gmaps-server")))
        XCTAssertEqual(ServerTarget.item("sentinel:nezopt:unit:marketplace-rails-sync").resolve(in: o),
                       .sheet(.item("sentinel:nezopt:unit:marketplace-rails-sync")))
        // A catalog key no longer in attention: the component itself.
        XCTAssertEqual(ServerTarget.item("catalog:secretary-gateways").resolve(in: o), .sheet(.component("secretary-gateways")))
        XCTAssertNil(ServerTarget.item("sentinel:gone").resolve(in: o))
        // The catalog's outage push: the item when it is in attention, else the component.
        XCTAssertEqual(ServerTarget.component("gmaps-server").resolve(in: o), .sheet(.item("catalog:gmaps-server")))
        XCTAssertEqual(ServerTarget.component("backup-timer").resolve(in: o), .sheet(.component("backup-timer")))
        XCTAssertNil(ServerTarget.component("nothing").resolve(in: o))
        XCTAssertEqual(ServerTarget.host("nezopt").resolve(in: o), .host("nezopt"))
        XCTAssertNil(ServerTarget.host("nowhere").resolve(in: o))
    }

    func testPendingItemOpensOnceLoaded() async throws {
        StubURLProtocol.handler = { request in
            XCTAssertEqual(request.url?.path, "/v1/system/overview")
            return .init(body: Self.fullJSON)
        }
        let client = StubURLProtocol.makeClient()
        let store = ServerStore(api: { ServerAPI(client: client) })
        store.open(ServerLink(tab: .services, target: .component("gmaps-server")))
        XCTAssertEqual(store.tab, .services)
        XCTAssertNil(store.sheet, "nothing loaded yet")
        await store.load(fresh: true)
        XCTAssertEqual(store.sheet, .item("catalog:gmaps-server"))
        XCTAssertEqual(store.tab, .overview)
        XCTAssertNotNil(store.loadedAt)
        XCTAssertTrue(StubURLProtocol.requests.contains { $0.url?.query == "fresh=1" })

        // Loaded: a host opens at once; an unknown target is dropped.
        store.sheet = nil
        store.open(ServerLink(tab: nil, target: .host("nezopt")))
        XCTAssertEqual(store.openHost, "nezopt")
        store.open(ServerLink(tab: nil, target: .item("sentinel:gone")))
        await store.load()
        XCTAssertNil(store.sheet)
        XCTAssertNil(store.pending)
    }

    func testFailedRefreshKeepsData() async throws {
        var fail = false
        StubURLProtocol.handler = { _ in fail ? .init(status: 502, body: "") : .init(body: Self.fullJSON) }
        let client = StubURLProtocol.makeClient()
        let store = ServerStore(api: { ServerAPI(client: client) })
        await store.load()
        XCTAssertNotNil(store.overview)
        fail = true
        await store.load()
        XCTAssertNotNil(store.overview, "the last data stays on screen")
        XCTAssertNotNil(store.error)
        XCTAssertFalse(store.isLoading)
    }

    // MARK: Widget

    func testWidgetSection() throws {
        let json = try Fixtures.decode(JSONValue.self, Self.fullJSON)
        let section = WidgetFetcher.serverSection(json)
        XCTAssertEqual(section.overall, "crit")
        XCTAssertEqual(section.attentionCount, 2)
        XCTAssertEqual(section.titles, ["gmaps-server", "marketplace-rails-sync"])
        XCTAssertEqual(section.generatedAt, ServerDates.parse("2026-10-28T21:50:00Z"))
        let fromApp = ServerSection(try full(), fetchedAt: section.fetchedAt)
        XCTAssertEqual(fromApp.overall, section.overall)
        XCTAssertEqual(fromApp.attentionCount, section.attentionCount)
        XCTAssertEqual(fromApp.titles, section.titles)
    }
}
