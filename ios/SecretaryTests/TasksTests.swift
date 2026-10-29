import XCTest
@testable import Secretary

@MainActor
final class TasksTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    static let view = """
    {"ok": true, "replayed": false, "view": {"revision": 7, "today": "2026-10-22", "timezone": "Europe/Copenhagen",
     "counts": {"inbox": 4, "today": 2, "overdue": 1, "upcoming": 1},
     "goal": {"daily_goal": 5, "completed_today": 1, "reached": false},
     "projects": [{"id": "inbox1", "name": "Inbox", "color": "charcoal", "inbox": true, "favorite": false, "count": 4, "sections": []}],
     "labels": [], "filters": [],
     "sync": {"status": "ok", "connected": true, "last_sync_ms": 1792656000000, "pending": 0},
     "conflicts": [{"id": "c_1", "content": "20min speaking", "kind": "changed_remotely", "message": "Todoist's version was kept.", "at_ms": 1}],
     "undo": [],
     "list": {"kind": "today", "title": "Today", "count": 2, "groups": [
       {"key": "overdue", "kind": "overdue", "title": "Overdue", "tasks": [
         {"id": "a2", "content": "10 min anki/memrise", "description": "", "project_id": "inbox1", "project_name": "Inbox",
          "project_color": "charcoal", "priority": 1, "labels": ["lingue"], "checked": false, "subtasks": 0, "subtasks_done": 0,
          "pending": false, "due": {"date": "2026-10-20", "string": "ogni giorno", "is_recurring": true, "rel_days": -2, "overdue": true}},
         {"broken": true}]},
       {"key": "2026-10-22", "kind": "date", "title": "", "date": "2026-10-22", "rel_days": 0, "tasks": [
         {"id": "a1", "content": "15 min shadowing", "project_id": "inbox1", "project_name": "Inbox", "priority": 4,
          "due": {"date": "2026-10-22", "time": "09:00", "string": "every day at 9", "is_recurring": true, "rel_days": 0, "overdue": false}}]}
     ]}}}
    """

    func testDecodesTheEngineViewLeniently() throws {
        let r = try Fixtures.decode(TasksResponse.self, Self.view)
        let v = try XCTUnwrap(r.view)
        XCTAssertEqual(v.counts.overdue, 1)
        XCTAssertEqual(v.goal.dailyGoal, 5)
        XCTAssertEqual(v.list.groups.map(\.kind), ["overdue", "date"])
        XCTAssertEqual(v.list.groups[0].tasks.map(\.id), ["a2"], "a bad task does not hide the group")
        let anki = v.list.groups[0].tasks[0]
        XCTAssertEqual(anki.priority, 1)
        XCTAssertTrue(anki.due?.isRecurring ?? false)
        XCTAssertEqual(TaskDates.label(try XCTUnwrap(v.list.groups[1].tasks[0].due), locale: Locale(identifier: "en_US")), "Today 09:00")
        XCTAssertEqual(TaskDates.header(v.list.groups[1], locale: Locale(identifier: "en_GB")), "22 Oct · Today · Thursday")
        XCTAssertEqual(v.conflicts.first?.kind, "changed_remotely")
    }

    func testCommandsCarryAnOpIdTheViewAndNoRevision() async throws {
        StubURLProtocol.handler = { _ in .init(body: Self.view) }
        let api = TasksAPI(client: StubURLProtocol.makeClient())
        _ = try await api.command(.quickAdd(text: "chiamare Marco domani alle 9 #Lavoro p1", defaultDue: "today"), opId: "op-1", view: .today)
        let request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/v1/tasks/command")
        XCTAssertEqual(request.value(forHTTPHeaderField: "X-Secretary-Client"), "ios")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: Any])
        XCTAssertEqual(body["op_id"] as? String, "op-1")
        XCTAssertNil(body["expected_revision"])
        XCTAssertEqual(body["view"] as? [String: String], ["kind": "today"])
        let command = try XCTUnwrap(body["command"] as? [String: Any])
        XCTAssertEqual(command["type"] as? String, "quick_add")
        XCTAssertEqual(command["default_due"] as? String, "today")

        _ = try await api.state(.upcoming)
        let url = try XCTUnwrap(StubURLProtocol.requests.last?.url)
        XCTAssertEqual(url.path, "/v1/tasks/state")
        XCTAssertTrue(url.query?.contains("view=upcoming") ?? false)
    }

    func testOfflineCommandsWaitAndGoOutInOrderWithTheirOpIds() async throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "tasks-tests-\(UUID().uuidString)"))
        var down = true
        StubURLProtocol.handler = { req in
            if down && req.httpMethod == "POST" { return .init(error: URLError(.notConnectedToInternet)) }
            return .init(body: Self.view)
        }
        let client = StubURLProtocol.makeClient()
        let store = TasksStore(api: { TasksAPI(client: client) }, defaults: defaults)
        await store.refresh()
        let anki = try XCTUnwrap(store.state?.list.groups[0].tasks.first)
        await store.toggle(anki)
        XCTAssertEqual(store.outbox.count, 1)
        XCTAssertTrue(store.hidden.contains("a2"), "the completed task hides while it waits")
        XCTAssertTrue(store.visible(store.groups()[0]).isEmpty)
        // kept across launches
        let again = TasksStore(api: { TasksAPI(client: client) }, defaults: defaults)
        XCTAssertEqual(again.outbox, store.outbox)

        down = false
        let opId = store.outbox[0].opId
        await store.flush()
        XCTAssertTrue(store.outbox.isEmpty)
        let sent = StubURLProtocol.requests.filter { $0.httpMethod == "POST" }.last
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: sent?.httpBody ?? Data()) as? [String: Any])
        XCTAssertEqual(body["op_id"] as? String, opId, "the same op_id: a replay is harmless")
        XCTAssertEqual((body["command"] as? [String: Any])?["type"] as? String, "complete")
    }

    func testRescheduleChoices() {
        // Thursday 22 October 2026
        let c = TasksStore.choices(today: "2026-10-22")
        XCTAssertEqual(c.map(\.date), ["2026-10-22", "2026-10-23", "2026-10-26"])
    }
}
