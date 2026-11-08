import XCTest
@testable import Secretary

@MainActor
final class ToolStepsTests: XCTestCase {
    private func item(_ id: String, _ role: TimelineItem.Role, _ text: String) -> TimelineItem {
        TimelineItem(id: id, ts: Date(timeIntervalSince1970: Double(id) ?? 0), channel: .ios, role: role, text: text)
    }

    private func step(_ id: String, _ tool: String, ok: Bool = true) -> TimelineItem {
        var step = item(id, .lucina, "🔧 \(tool) {\"action\":\"list\"} → \(ok ? "ok" : "error")")
        step.meta = TimelineMeta(kind: "tool")
        step.meta?.tool = tool
        step.meta?.ok = ok
        return step
    }

    func testRunsOfStepsFoldIntoOneEntryBetweenMessages() {
        let entries = ToolSteps.entries([
            item("1", .user, "Sort my university folder"),
            step("2", "drive"), step("3", "drive"), step("4", "drive", ok: false), step("5", "media"),
            item("6", .lucina, "Done: three folders."),
            step("7", "drive"),
        ])
        XCTAssertEqual(entries.map(\.id), ["1", "tools-2", "6", "tools-7"])
        guard case let .tools(steps) = entries[1] else { return XCTFail("expected a tool run") }
        XCTAssertEqual(steps.map(\.id), ["2", "3", "4", "5"])
    }

    func testSummaryNamesToolsAndCountsFailures() {
        let summary = ToolSteps.summary([step("1", "drive"), step("2", "media", ok: false), step("3", "drive")])
        XCTAssertEqual(summary.tools, ["Drive", "Gallery"])
        XCTAssertEqual(summary.failed, 1)
        XCTAssertEqual(ToolSteps.name("web_search"), "Web search")
        // An older item without meta.tool still names its tool from the text.
        var old = item("9", .lucina, "🔧 calendar {} → ok")
        old.meta = TimelineMeta(kind: "tool")
        XCTAssertEqual(ToolSteps.summary([old]).tools, ["Calendar"])
        XCTAssertEqual(ToolSteps.text(step("1", "drive")), "drive {\"action\":\"list\"} → ok")
    }

    func testToolMetaDecodes() throws {
        let json = #"{"id":"1","ts":1000,"channel":"web","role":"lucina","text":"🔧 drive {} → ok","meta":{"kind":"tool","ok":true,"tool":"drive"}}"#
        let decoded = try JSONDecoder().decode(TimelineItem.self, from: Data(json.utf8))
        XCTAssertTrue(ToolSteps.isStep(decoded))
        XCTAssertEqual(decoded.meta?.tool, "drive")
        XCTAssertEqual(decoded.meta?.ok, true)
    }
}
