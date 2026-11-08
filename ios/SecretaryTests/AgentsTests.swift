import XCTest
@testable import Secretary

/// Lucina Agents: the wire shapes are lucina-agents' own (crates/lucina-agents types.rs, timeline.rs, events.rs,
/// permission.rs), with the JSON written the way serde writes it (camelCase, snake_case tags).
@MainActor
final class AgentsTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    // MARK: Fixtures

    nonisolated static let itemsJSON = """
    {"agents":[
     {"id":"agt_0123456789abcdef","shortId":"agt_012","title":"Fix the login bug","provider":"claude","model":"opus-5-5",
      "status":"running","cwd":"/home/nez/Projects/Lucina","createdAt":"2026-10-28T09:00:00.000Z","updatedAt":"2026-10-28T10:00:00.000Z",
      "lastUserMessageAt":"2026-10-28T09:59:00.000Z","requiresAttention":false,"labels":{},"bucket":"running"},
     {"id":"agt_b","shortId":"agt_b","title":null,"provider":"codex","model":null,"status":"idle","cwd":"/home/nez/Projects/Startup/Pokoin",
      "createdAt":"2026-10-28T08:00:00.000Z","updatedAt":"2026-10-28T08:30:00.000Z","lastUserMessageAt":null,
      "requiresAttention":true,"attentionReason":"permission","labels":{},"bucket":"needs_input","external":true,"host":"mac"},
     {"broken":true},
     {"id":"agt_c","provider":"future","status":"teleporting","bucket":"nonsense","somethingNew":{"a":1}}
    ]}
    """

    nonisolated static let permissionJSON = """
    {"id":"perm_1","provider":"claude","name":"Bash","kind":"tool","title":"Run a command","description":"Claude wants to run: rm -rf build",
     "input":{"command":"rm -rf build"},"detail":{"type":"shell","command":"rm -rf build"},
     "actions":[{"id":"allow","label":"Allow","behavior":"allow","variant":"primary"},{"id":"deny","label":"Deny","behavior":"deny","variant":"danger"}],
     "approvalId":"apr_9"}
    """

    nonisolated static func snapshotJSON(id: String = "agt_1", status: String = "running", pending: String = "[]", workspace: Bool = true,
                             archivedAt: String? = nil, extra: String = "") -> String {
        """
        {"id":"\(id)","provider":"claude","cwd":"/home/nez/Projects/Lucina/wt","model":"opus-5-5",
         \(workspace ? "\"workspaceId\":\"wks_0011223344556677\"," : "")
         "createdAt":"2026-10-28T09:00:00.000Z","updatedAt":"2026-10-28T10:00:00.000Z","lastUserMessageAt":"2026-10-28T09:59:00.000Z",
         "status":"\(status)","activeTurn":{"turnId":"t1","startedAt":"2026-10-28T09:59:00.000Z"},
         "capabilities":{"supportsStreaming":true,"supportsSessionPersistence":true,"supportsDynamicModes":false,"supportsMcpServers":false,
           "supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":false,"supportsRewindFiles":false,"supportsRewindBoth":false},
         "currentModeId":null,"availableModes":[],"pendingPermissions":\(pending),"persistence":null,"title":"Fix the login bug","labels":{},
         "requiresAttention":false\(archivedAt.map { ",\"archivedAt\":\"\($0)\"" } ?? "")\(extra)}
        """
    }

    nonisolated static let timelineJSON = """
    {"epoch":"ep1","nextSeq":9,"rows":[
     {"seq":1,"timestamp":"2026-10-28T09:59:00.000Z","item":{"type":"user_message","text":"fix it","messageId":"m1"}},
     {"seq":2,"timestamp":"2026-10-28T09:59:01.000Z","item":{"type":"reasoning","text":"Let me "},"turnId":"t1"},
     {"seq":3,"timestamp":"2026-10-28T09:59:01.100Z","item":{"type":"reasoning","text":"look."},"turnId":"t1"},
     {"seq":4,"timestamp":"2026-10-28T09:59:02.000Z","item":{"type":"tool_call","callId":"c1","name":"Bash","status":"running","error":null,
        "detail":{"type":"shell","command":"cargo test","cwd":"/w"}}},
     {"seq":5,"timestamp":"2026-10-28T09:59:09.000Z","item":{"type":"tool_call","callId":"c1","name":"Bash","status":"completed","error":null,
        "detail":{"type":"shell","command":"cargo test","output":"ok\\n","exitCode":0}}},
     {"seq":6,"timestamp":"2026-10-28T09:59:10.000Z","item":{"type":"assistant_message","text":"All **green**","messageId":"a1"}},
     {"seq":7,"timestamp":"2026-10-28T09:59:10.500Z","item":{"type":"assistant_message","text":", done.","messageId":"a1"}},
     {"seq":8,"timestamp":"2026-10-28T09:59:11.000Z","item":{"type":"todo","items":[{"text":"a","completed":true},{"text":"b","completed":false,"status":"in_progress"}]}},
     {"seq":9,"timestamp":"2026-10-28T09:59:12.000Z","item":{"type":"tool_call","callId":"c2","name":"Edit","status":"failed","error":"denied",
        "detail":{"type":"edit","filePath":"/w/a.rs","oldString":"a","newString":"b"}}},
     {"seq":10,"timestamp":"2026-10-28T09:59:13.000Z","item":{"type":"plugin","id":"p","pluginId":"x","kind":"k","version":1,"data":null}}
    ]}
    """

    private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T { try Fixtures.decode(type, json) }

    private func store(_ replies: @escaping (URLRequest) -> StubURLProtocol.Reply = { _ in .init() }) -> AgentsStore {
        StubURLProtocol.handler = replies
        let client = StubURLProtocol.makeClient()
        return AgentsStore(api: { AgentsAPI(client: client) })
    }

    // MARK: Decoding

    func testListItemsDecodeAndABadItemDoesNotHideTheList() throws {
        let list = try decode(AgentListResponse.self, Self.itemsJSON).agents
        XCTAssertEqual(list.map(\.id), ["agt_0123456789abcdef", "agt_b", "agt_c"])
        XCTAssertEqual(list[0].bucket, .running)
        XCTAssertEqual(list[0].stateLabel, "Running")
        XCTAssertEqual(list[0].providerLine, "claude · opus-5-5")
        XCTAssertEqual(list[1].bucket, .needsInput)
        XCTAssertEqual(list[1].attentionReason, "permission")
        XCTAssertTrue(list[1].external)
        XCTAssertEqual(list[1].displayTitle, "Pokoin", "no title: the folder")
        XCTAssertEqual(list[2].status, .unknown)
        XCTAssertEqual(list[2].bucket, .done)
        XCTAssertEqual(list[2].shortId, "agt_c")
    }

    func testSnapshotDecodesPermissionsGateAndWorkspace() throws {
        let gate = ",\"gate\":{\"verdict\":\"refused\",\"details\":[\"todo found\"],\"round\":1,\"finishedAt\":\"2026-10-28T10:00:00Z\"}"
        let s = try decode(AgentSnapshot.self, Self.snapshotJSON(pending: "[\(Self.permissionJSON)]", extra: gate))
        XCTAssertEqual(s.status, .running)
        XCTAssertTrue(s.activeTurn)
        XCTAssertEqual(s.workspaceId, "wks_0011223344556677")
        XCTAssertEqual(s.gate?.verdict, "refused")
        XCTAssertEqual(s.gate?.details, ["todo found"])
        let p = try XCTUnwrap(s.pendingPermissions.first)
        XCTAssertEqual(p.headline, "Run a command")
        XCTAssertEqual(p.inputText, "rm -rf build")
        XCTAssertEqual(p.approvalId, "apr_9")
        XCTAssertEqual(p.actions.map(\.behavior), ["allow", "deny"])
        XCTAssertEqual(s.item.bucket, .needsInput, "a pending permission makes the bucket")
        XCTAssertEqual(s.item.stateLabel, "Needs permission")
    }

    func testPermissionWithoutAnApprovalIdAndWithOddInput() throws {
        let p = try decode(AgentPermission.self, #"{"id":"p","provider":"codex","name":"Fetch","kind":"other","input":{"a":[1,2],"b":true}}"#)
        XCTAssertNil(p.approvalId)
        XCTAssertEqual(p.headline, "Fetch")
        XCTAssertTrue(p.inputText.contains("\"a\""), "no command or path: the JSON")
    }

    func testTimelineDecodesEveryItemKind() throws {
        let page = try decode(AgentTimelinePage.self, Self.timelineJSON)
        XCTAssertEqual(page.epoch, "ep1")
        XCTAssertEqual(page.rows.count, 10)
        XCTAssertEqual(page.rows[0].item, .userMessage(text: "fix it"))
        guard case let .toolCall(done) = page.rows[4].item else { return XCTFail("tool call") }
        XCTAssertEqual(done.status, "completed")
        XCTAssertNil(done.error)
        XCTAssertEqual(done.detail.type, "shell")
        XCTAssertEqual(done.detail.summary, "cargo test")
        XCTAssertEqual(done.detail.body, "$ cargo test\nok\n")
        guard case let .toolCall(failed) = page.rows[8].item else { return XCTFail("tool call") }
        XCTAssertEqual(failed.error, "denied")
        XCTAssertEqual(failed.detail.body, "/w/a.rs\n\n- a\n\n+ b")
        guard case let .todo(todos) = page.rows[7].item else { return XCTFail("todo") }
        XCTAssertEqual(todos, [AgentTodo(text: "a", completed: true, inProgress: false), AgentTodo(text: "b", completed: false, inProgress: true)])
        XCTAssertEqual(page.rows[9].item, .other(type: "plugin"))
    }

    func testEntriesFoldToolCallsAndJoinChunks() throws {
        let rows = try decode(AgentTimelinePage.self, Self.timelineJSON).rows
        let entries = AgentTimeline.entries(rows)
        // user, reasoning (2 chunks), one Bash call, one assistant message (2 chunks), todo, Edit, plugin
        XCTAssertEqual(entries.count, 7)
        XCTAssertEqual(entries[1].item, .reasoning(text: "Let me look."))
        guard case let .toolCall(bash) = entries[2].item else { return XCTFail("tool call") }
        XCTAssertEqual(bash.status, "completed", "the last update of a call wins")
        XCTAssertEqual(entries[2].id, 4, "and it keeps the place and id of the first row")
        XCTAssertEqual(entries[3].item, .assistantMessage(text: "All **green**, done.", messageId: "a1"))
        // A different message id starts a new message.
        let two = AgentTimeline.entries([AgentTimelineRow(seq: 1, item: .assistantMessage(text: "a", messageId: "x")),
                                         AgentTimelineRow(seq: 2, item: .assistantMessage(text: "b", messageId: "y"))])
        XCTAssertEqual(two.count, 2)
    }

    func testDiffDecodes() throws {
        let json = """
        {"cwd":"/w","files":[{"path":"a.rs","oldPath":"a.rs","isNew":false,"isDeleted":false,"additions":1,"deletions":1,"status":"ok",
          "hunks":[{"oldStart":1,"oldCount":1,"newStart":1,"newCount":1,"lines":[{"type":"remove","content":"-a"},{"type":"add","content":"+b"}]}]},
         {"path":"big.bin","isNew":true,"isDeleted":false,"additions":0,"deletions":0,"hunks":[],"status":"binary"}],"diffTooLarge":false,"error":null}
        """
        let d = try decode(AgentDiff.self, json)
        XCTAssertEqual(d.files.map(\.path), ["a.rs", "big.bin"])
        XCTAssertEqual(d.files[0].hunks[0].lines.map(\.kind), ["remove", "add"])
        XCTAssertEqual(d.files[1].status, "binary")
        XCTAssertNil(d.error)
    }

    // MARK: Stream frames

    private func frame(_ data: String, id: String? = nil, event: String = "message") -> SSEEvent { SSEEvent(event: event, data: data, id: id, retry: nil) }

    func testStreamFramesDecode() throws {
        let state = #"{"type":"agent_state","agent":"# + Self.snapshotJSON(id: "agt_1") + "}"
        guard case let .state(s) = try XCTUnwrap(AgentBusEvent.decode(frame(state))) else { return XCTFail("state") }
        XCTAssertEqual(s.id, "agt_1")

        let timeline = #"{"type":"agent_stream","agentId":"agt_1","seq":11,"epoch":"ep1","timestamp":"2026-10-28T10:00:00.000Z","event":{"type":"timeline","provider":"claude","turnId":"t1","item":{"type":"assistant_message","text":"hi"}}}"#
        guard case let .timeline(id, row, epoch) = try XCTUnwrap(AgentBusEvent.decode(frame(timeline))) else { return XCTFail("timeline") }
        XCTAssertEqual(id, "agt_1")
        XCTAssertEqual(row.seq, 11)
        XCTAssertEqual(row.turnId, "t1")
        XCTAssertEqual(row.item, .assistantMessage(text: "hi", messageId: nil))
        XCTAssertEqual(epoch, "ep1")

        let requested = #"{"type":"agent_stream","agentId":"agt_1","epoch":"ep1","timestamp":"t","event":{"type":"permission_requested","provider":"claude","request":"# + Self.permissionJSON + "}}"
        guard case let .permissionRequested(_, p) = try XCTUnwrap(AgentBusEvent.decode(frame(requested))) else { return XCTFail("requested") }
        XCTAssertEqual(p.id, "perm_1")

        let resolved = #"{"type":"agent_stream","agentId":"agt_1","epoch":"ep1","timestamp":"t","event":{"type":"permission_resolved","provider":"claude","requestId":"perm_1","resolution":{"behavior":"allow"}}}"#
        XCTAssertEqual(AgentBusEvent.decode(frame(resolved)), .permissionResolved(agentId: "agt_1", requestId: "perm_1"))

        let view = #"{"type":"permission","permission":{"id":"perm_1","kind":"agent","agentId":"agt_1","name":"Bash","title":"Run","status":"pending","createdAt":"a","expiresAt":"b","approvalId":"apr_9"}}"#
        guard case let .permission(v) = try XCTUnwrap(AgentBusEvent.decode(frame(view))) else { return XCTFail("permission") }
        XCTAssertEqual(v.approvalId, "apr_9")
        XCTAssertEqual(v.agentId, "agt_1")

        XCTAssertEqual(AgentBusEvent.decode(frame(#"{"type":"agent_deleted","agentId":"agt_1"}"#)), .deleted(agentId: "agt_1"))
        XCTAssertEqual(AgentBusEvent.decode(frame(#"{"type":"workspace","workspace":{}}"#)), .ignored)
        XCTAssertEqual(AgentBusEvent.decode(frame(#"{"type":"agent_command","agentId":"a"}"#)), .ignored)
        XCTAssertEqual(AgentBusEvent.decode(frame(#"{"type":"agent_stream","agentId":"a","event":{"type":"turn_started","provider":"x"}}"#)), .ignored)
        XCTAssertNil(AgentBusEvent.decode(frame("not json")))
    }

    func testResetFrameIsRecognisedByNameAndByData() {
        XCTAssertEqual(AgentBusEvent.decode(frame(#"{"type":"stream.reset"}"#, id: "5", event: "stream.reset")), .reset)
        XCTAssertEqual(AgentBusEvent.decode(frame(#"{"type":"stream.reset"}"#)), .reset)
    }

    func testSSEParserKeepsIdsAndNamedEvents() {
        var parser = SSEParser()
        var out: [SSEEvent] = []
        let text = "id: 1761600000000001\ndata: {\"type\":\"agent_deleted\",\"agentId\":\"a\"}\n\n: keep-alive\n\nevent: stream.reset\nid: 1761600000000009\ndata: {\"type\":\"stream.reset\"}\n\n"
        for line in text.components(separatedBy: "\n") { if let e = parser.feed(line) { out.append(e) } }
        XCTAssertEqual(out.map(\.id), ["1761600000000001", "1761600000000009"])
        XCTAssertEqual(out.map { AgentBusEvent.decode($0) }, [.deleted(agentId: "a"), .reset])
    }

    // MARK: List store

    func testStoreAppliesStateDeleteAndFilters() async throws {
        let s = store { _ in .init(body: Self.itemsJSON) }
        await s.refresh()
        XCTAssertEqual(s.items.count, 3)
        XCTAssertEqual(s.sections.map(\.bucket), [.needsInput, .running, .done], "most urgent first")

        // A snapshot updates the row and moves its bucket.
        let snap = try decode(AgentSnapshot.self, Self.snapshotJSON(id: "agt_0123456789abcdef", status: "idle"))
        s.apply(.state(snap))
        XCTAssertEqual(s.items.first { $0.id == "agt_0123456789abcdef" }?.bucket, .done)
        // A new agent appears.
        s.apply(.state(try decode(AgentSnapshot.self, Self.snapshotJSON(id: "agt_new"))))
        XCTAssertEqual(s.items.count, 4)
        // Archived agents leave the live list.
        s.apply(.state(try decode(AgentSnapshot.self, Self.snapshotJSON(id: "agt_new", status: "idle", archivedAt: "2026-10-28T11:00:00Z"))))
        XCTAssertEqual(s.items.count, 3)
        s.apply(.deleted(agentId: "agt_b"))
        XCTAssertEqual(s.items.map(\.id), ["agt_0123456789abcdef", "agt_c"])
    }

    func testArchivedToggleAsksForArchived() async {
        let s = store { _ in .init(body: #"{"agents":[]}"#) }
        await s.setArchived(true)
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.path, "/v1/agents/items")
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.query, "archived=true")
        XCTAssertTrue(s.showArchived)
    }

    func testResetRefetchesTheListAndTellsTheScreens() async throws {
        let s = store { _ in .init(body: Self.itemsJSON) }
        await s.refresh()
        var got: [AgentBusEvent] = []
        s.observe("agt_1") { got.append($0) }
        s.handle(frame(#"{"type":"stream.reset"}"#, id: "77", event: "stream.reset"))
        XCTAssertEqual(s.lastEventId, "77", "the reset frame's id is where to resume from")
        XCTAssertEqual(s.resetCount, 1)
        XCTAssertEqual(got, [.reset])
        let before = StubURLProtocol.requests.count
        try await Task.sleep(for: .milliseconds(200))
        XCTAssertGreaterThan(StubURLProtocol.requests.count, before, "the list was fetched again")
    }

    func testEventsReachOnlyTheOpenAgentAndApprovalIdsAreKept() throws {
        let s = store()
        var one: [AgentBusEvent] = []
        var two: [AgentBusEvent] = []
        s.observe("agt_1") { one.append($0) }
        s.observe("agt_2") { two.append($0) }
        s.handle(frame(#"{"type":"agent_stream","agentId":"agt_2","seq":1,"epoch":"e","timestamp":"t","event":{"type":"timeline","provider":"x","item":{"type":"reasoning","text":"r"}}}"#, id: "3"))
        s.handle(frame(#"{"type":"permission","permission":{"id":"perm_1","agentId":"agt_1","status":"pending","approvalId":"apr_9"}}"#, id: "4"))
        XCTAssertEqual(one.count, 1)
        XCTAssertEqual(two.count, 1)
        XCTAssertEqual(s.approvalIds["perm_1"], "apr_9")
        XCTAssertEqual(s.lastEventId, "4")
        s.stopObserving("agt_1")
        s.handle(frame(#"{"type":"agent_deleted","agentId":"agt_1"}"#, id: "5"))
        XCTAssertEqual(one.count, 1)
    }

    // MARK: Detail

    /// A client that serves one agent: the snapshot, then the timeline pages `rows` (a closure so tests can change it).
    private func detail(snapshot: @escaping () -> String = { AgentsTests.snapshotJSON() }, timeline: @escaping (Int) -> String = { _ in AgentsTests.timelineJSON },
                        other: @escaping (URLRequest) -> StubURLProtocol.Reply = { _ in .init() }) -> (AgentDetailModel, AgentsStore) {
        let s = store { request in
            let path = request.url?.path ?? ""
            if path.hasSuffix("/timeline") {
                let after = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "afterSeq" }?.value.flatMap(Int.init) ?? 0
                return .init(body: timeline(after))
            }
            if request.httpMethod == "GET", path == "/v1/agents/agt_1" { return .init(body: snapshot()) }
            return other(request)
        }
        return (AgentDetailModel(agentId: "agt_1", store: s), s)
    }

    func testDetailLoadsSnapshotAndWholeTimeline() async {
        let (m, _) = detail()
        await m.load()
        XCTAssertTrue(m.loaded)
        XCTAssertEqual(m.snapshot?.title, "Fix the login bug")
        XCTAssertEqual(m.rows.count, 10)
        XCTAssertEqual(m.lastSeq, 10)
        XCTAssertEqual(m.epoch, "ep1")
        XCTAssertEqual(m.entries.count, 7)
        let timelineRequest = StubURLProtocol.requests.first { $0.url?.path == "/v1/agents/agt_1/timeline" }
        XCTAssertEqual(timelineRequest?.url?.query?.contains("afterSeq=0"), true)
    }

    func testStreamedRowsAreAppendedInOrderAndDuplicatesDropped() async {
        let (m, _) = detail()
        await m.load()
        func row(_ seq: Int, _ text: String) -> AgentBusEvent {
            .timeline(agentId: "agt_1", row: AgentTimelineRow(seq: seq, item: .assistantMessage(text: text, messageId: "a2")), epoch: "ep1")
        }
        m.apply(row(11, "He"))
        m.apply(row(11, "He"))
        m.apply(row(12, "llo"))
        XCTAssertEqual(m.lastSeq, 12)
        XCTAssertEqual(m.rows.count, 12)
        XCTAssertEqual(m.entries.last?.item, .assistantMessage(text: "Hello", messageId: "a2"))
    }

    func testAGapInSeqFetchesTheMissingRows() async throws {
        var served = 0
        let (m, _) = detail(timeline: { after in
            served += 1
            if after == 0 { return Self.timelineJSON }
            return #"{"epoch":"ep1","nextSeq":13,"rows":[{"seq":11,"timestamp":"t","item":{"type":"reasoning","text":"a"}},{"seq":12,"timestamp":"t","item":{"type":"reasoning","text":"b"}},{"seq":13,"timestamp":"t","item":{"type":"error","message":"boom"}}]}"#
        })
        await m.load()
        m.apply(.timeline(agentId: "agt_1", row: AgentTimelineRow(seq: 13, item: .error("boom")), epoch: "ep1"))
        try await Task.sleep(for: .milliseconds(300))
        XCTAssertEqual(m.lastSeq, 13)
        XCTAssertEqual(m.rows.map(\.seq).suffix(3), [11, 12, 13])
        let after = StubURLProtocol.requests.compactMap { $0.url?.query }.filter { $0.contains("afterSeq=10") }
        XCTAssertFalse(after.isEmpty, "caught up from the last row we had")
    }

    func testANewEpochReloadsTheTimeline() async throws {
        var epoch = "ep1"
        let (m, _) = detail(timeline: { _ in
            epoch == "ep1" ? Self.timelineJSON : #"{"epoch":"ep2","nextSeq":1,"rows":[{"seq":1,"timestamp":"t","item":{"type":"user_message","text":"again"}}]}"#
        })
        await m.load()
        epoch = "ep2"
        await m.catchUp()
        XCTAssertEqual(m.epoch, "ep2")
        XCTAssertEqual(m.rows.map(\.seq), [1])
        XCTAssertEqual(m.entries.first?.item, .userMessage(text: "again"))
    }

    func testStreamedRowsFromAnotherEpochAreNotMixedIn() async throws {
        let (m, _) = detail()
        await m.load()
        m.apply(.timeline(agentId: "agt_1", row: AgentTimelineRow(seq: 11, item: .error("x")), epoch: "ep9"))
        XCTAssertEqual(m.rows.count, 10, "not appended: the timeline is reloaded instead")
    }

    func testPermissionsComeAndGoWithTheStream() async throws {
        let (m, s) = detail()
        await m.load()
        let p = try decode(AgentPermission.self, Self.permissionJSON)
        m.apply(.permissionRequested(agentId: "agt_1", p))
        m.apply(.permissionRequested(agentId: "agt_1", p))
        XCTAssertEqual(m.pending.map(\.id), ["perm_1"], "no duplicates")
        XCTAssertEqual(m.approvalId(for: p), "apr_9")
        m.apply(.permissionResolved(agentId: "agt_1", requestId: "perm_1"))
        XCTAssertTrue(m.pending.isEmpty)
        // The book's view closing a request does the same.
        m.apply(.permissionRequested(agentId: "agt_1", p))
        m.apply(.permission(try decode(AgentPermissionView.self, #"{"id":"perm_1","agentId":"agt_1","status":"allowed"}"#)))
        XCTAssertTrue(m.pending.isEmpty)
        _ = s
    }

    // MARK: Requests

    func testSendSteersWhileRunning() async throws {
        let (m, _) = detail(other: { _ in .init(body: #"{"agentId":"agt_1","disposition":"steered"}"#) })
        await m.load()
        XCTAssertTrue(m.isRunning)
        m.draft = "  use the other approach  "
        let ok = await m.send()
        XCTAssertTrue(ok)
        XCTAssertEqual(m.draft, "")
        let request = try XCTUnwrap(StubURLProtocol.requests.last { $0.httpMethod == "POST" })
        XCTAssertEqual(request.url?.path, "/v1/agents/agt_1/send")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: String])
        XCTAssertEqual(body, ["prompt": "use the other approach", "activeTurnBehavior": "steer"])
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer sec_test")
    }

    func testSendOnAnIdleAgentUsesTheDefaultBehaviour() throws {
        let body = AgentsAPI.sendBody(prompt: "go", steer: false)
        XCTAssertEqual(body.activeTurnBehavior, "interrupt")
        XCTAssertEqual(AgentsAPI.sendBody(prompt: "go", steer: true).activeTurnBehavior, "steer")
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(body)) as? [String: String])
        XCTAssertEqual(json, ["prompt": "go", "activeTurnBehavior": "interrupt"])
    }

    func testAnswerBodiesPickTheRequestsOwnActions() throws {
        let p = try decode(AgentPermission.self, Self.permissionJSON)
        XCTAssertEqual(AgentsAPI.answerBody(p, allow: true), .init(behavior: "allow", message: nil, selectedActionId: "allow"))
        let deny = AgentsAPI.answerBody(p, allow: false)
        XCTAssertEqual(deny.behavior, "deny")
        XCTAssertEqual(deny.selectedActionId, "deny")
        XCTAssertEqual(deny.message, "Denied from the Secretary app")
        let bare = try decode(AgentPermission.self, #"{"id":"p","name":"Bash"}"#)
        XCTAssertNil(AgentsAPI.answerBody(bare, allow: true).selectedActionId)
        // The allow body has no `message` key at all.
        let data = try JSONEncoder().encode(AgentsAPI.answerBody(bare, allow: true))
        XCTAssertEqual(try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: String]), ["behavior": "allow"])
    }

    func testAllowPostsToThePermissionRouteAndClearsIt() async throws {
        var answered = false
        let (m, _) = detail(snapshot: { Self.snapshotJSON(pending: answered ? "[]" : "[\(Self.permissionJSON)]") },
                            other: { _ in answered = true; return .init(body: #"{"agentId":"agt_1","requestId":"perm_1","behavior":"allow"}"#) })
        await m.load()
        let p = try XCTUnwrap(m.pending.first)
        await m.answer(p, allow: true)
        let request = try XCTUnwrap(StubURLProtocol.requests.last { $0.httpMethod == "POST" })
        XCTAssertEqual(request.url?.path, "/v1/agents/agt_1/permissions/perm_1")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: String])
        XCTAssertEqual(body, ["behavior": "allow", "selectedActionId": "allow"])
        XCTAssertTrue(m.pending.isEmpty)
        XCTAssertNil(m.actionError)
    }

    func testAnAlreadyAnsweredRequestIsDroppedWithTheServersMessage() async throws {
        var answered = false
        let (m, _) = detail(snapshot: { Self.snapshotJSON(pending: answered ? "[]" : "[\(Self.permissionJSON)]") },
                            other: { _ in answered = true; return .init(status: 409, body: #"{"error":{"code":"PERMISSION_ALREADY_ANSWERED","message":"Permission request perm_1 was already answered"}}"#) })
        await m.load()
        await m.answer(try XCTUnwrap(m.pending.first), allow: false)
        XCTAssertTrue(m.pending.isEmpty)
        XCTAssertEqual(m.actionError, "Permission request perm_1 was already answered")
    }

    func testAnOtherErrorKeepsTheRequestPending() async throws {
        let (m, _) = detail(snapshot: { Self.snapshotJSON(pending: "[\(Self.permissionJSON)]") },
                            other: { _ in .init(status: 403, body: #"{"error":{"code":"PERMISSION_FORBIDDEN","message":"Only the owner can answer"}}"#) })
        await m.load()
        await m.answer(try XCTUnwrap(m.pending.first), allow: true)
        XCTAssertEqual(m.pending.count, 1)
        XCTAssertEqual(m.actionError, "Only the owner can answer")
    }

    func testStopArchiveAndDiffRoutes() async throws {
        let (m, _) = detail(other: { request in
            if request.url?.path.hasSuffix("/diff") == true { return .init(body: #"{"cwd":"/w","files":[],"diffTooLarge":false,"error":null}"#) }
            return .init(body: #"{"agentId":"agt_1","stopped":true}"#)
        })
        await m.load()
        await m.stop()
        let stop = StubURLProtocol.requests.last { $0.url?.path == "/v1/agents/agt_1/stop" }
        XCTAssertEqual(stop?.httpMethod, "POST")
        await m.setArchived(true)
        XCTAssertTrue(StubURLProtocol.requests.contains { $0.url?.path == "/v1/agents/agt_1/archive" })
        await m.loadDiff()
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.path, "/v1/agents/workspaces/wks_0011223344556677/diff")
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.query, "mode=all")
        XCTAssertEqual(m.diff?.files.count, 0)
    }

    func testIdsWithSlashesStayOneSegment() {
        XCTAssertEqual(AgentsAPI.segment("a/b c"), "a%2Fb%20c")
    }
}
