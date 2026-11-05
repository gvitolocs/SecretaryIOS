import XCTest
@testable import Secretary

@MainActor
final class FakeExecutor: DeviceCommandExecuting {
    var executed: [DeviceCommand] = []
    var result: CommandResult = .success("obj-1")

    func execute(_ command: DeviceCommand) async -> CommandResult {
        executed.append(command)
        return result
    }

    func currentState() async -> DeviceState {
        DeviceState(alarms: [.object(["id": .string("a1"), "time": .string("07:00"), "label": .string("Gym")])])
    }
}

@MainActor
final class DeviceBridgeTests: XCTestCase {
    private var fileURL: URL!

    override func setUp() {
        fileURL = FileManager.default.temporaryDirectory.appendingPathComponent("device-\(UUID().uuidString).json")
        StubURLProtocol.requests = []
        StubURLProtocol.handler = { _ in .init(body: #"{"ok":true}"#) }
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: fileURL)
    }

    private func makeBridge(activatedAt: Date = Date().addingTimeInterval(-3600)) -> (DeviceBridge, FakeExecutor) {
        let client = StubURLProtocol.makeClient()
        let bridge = DeviceBridge(client: { DeviceClient(client: client) }, fileURL: fileURL, activatedAt: activatedAt)
        let executor = FakeExecutor()
        bridge.executor = executor
        return (bridge, executor)
    }

    private func commandItem(id: String, type: String, args: String = "{}", age: TimeInterval = 5) -> TimelineItem {
        let json = """
        {"id":"tl_\(id)","ts":\(Int((Date().timeIntervalSince1970 - age) * 1000)),"channel":"telegram","role":"lucina","text":"Setting it.",
         "meta":{"kind":"device_command","command":{"id":"\(id)","type":"\(type)","args":\(args)}}}
        """
        return try! JSONDecoder().decode(TimelineItem.self, from: Data(json.utf8))
    }

    private func waitUntil(_ condition: @escaping () -> Bool) async {
        for _ in 0..<100 where !condition() { try? await Task.sleep(for: .milliseconds(20)) }
    }

    private var ackBodies: [String: [String: Any]] {
        var out: [String: [String: Any]] = [:]
        for request in StubURLProtocol.requests {
            guard let path = request.url?.path, path.hasPrefix("/v1/device/commands/") else { continue }
            let id = path.replacingOccurrences(of: "/v1/device/commands/", with: "").replacingOccurrences(of: "/result", with: "")
            out[id] = (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data())) as? [String: Any]
        }
        return out
    }

    func testCommandParsing() throws {
        let command = try XCTUnwrap(DeviceCommand(item: commandItem(id: "c1", type: "alarm.set", args: #"{"time":"07:00","label":"Gym","repeat":["mon","wed"]}"#)))
        XCTAssertEqual(command.kind, .alarmSet)
        XCTAssertEqual(command.string("time"), "07:00")
        XCTAssertFalse(command.needsConfirmation)
        XCTAssertNil(DeviceCommand(item: commandItem(id: "c2", type: "launch.rockets")), "unknown types are ignored")
        XCTAssertTrue(DeviceCommand(item: commandItem(id: "c3", type: "alarm.delete", args: #"{"id":"x"}"#))!.needsConfirmation)
        XCTAssertTrue(DeviceCommand(item: commandItem(id: "c4", type: "calendar.create"))!.needsConfirmation)
        XCTAssertFalse(DeviceCommand(item: commandItem(id: "c5", type: "reminder.create"))!.needsConfirmation)
        XCTAssertEqual(DeviceExecutors.clock("07:30")?.hour, 7)
        XCTAssertNil(DeviceExecutors.clock("25:00"))
        XCTAssertEqual(DeviceExecutors.weekday("Wednesday"), .wednesday)
    }

    func testSafeCommandsRunAndAreAckedOnce() async {
        let (bridge, executor) = makeBridge()
        let item = commandItem(id: "c1", type: "alarm.set", args: #"{"time":"07:00","label":"Gym"}"#)
        bridge.consider([item])
        bridge.consider([item]) // the stream and a page can both deliver it
        await waitUntil { self.ackBodies["c1"] != nil }
        XCTAssertEqual(executor.executed.map(\.id), ["c1"])
        XCTAssertEqual(ackBodies["c1"]?["ok"] as? Bool, true)
        XCTAssertEqual(ackBodies["c1"]?["deviceObjectId"] as? String, "obj-1")
        // A new launch remembers it was handled.
        let client = StubURLProtocol.makeClient()
        let again = DeviceBridge(client: { DeviceClient(client: client) }, fileURL: fileURL)
        let other = FakeExecutor()
        again.executor = other
        again.consider([item])
        try? await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(other.executed.isEmpty)
    }

    func testDeletesWaitForTheOwner() async {
        let (bridge, executor) = makeBridge()
        bridge.consider([commandItem(id: "d1", type: "alarm.delete", args: #"{"id":"a1","label":"Gym"}"#),
                         commandItem(id: "d2", type: "calendar.delete", args: #"{"id":"e1"}"#)])
        XCTAssertEqual(bridge.pendingConfirmations.map(\.id), ["d1", "d2"])
        XCTAssertTrue(executor.executed.isEmpty, "nothing runs before the tap")
        await bridge.approve(bridge.pendingConfirmations[0])
        XCTAssertEqual(executor.executed.map(\.id), ["d1"])
        await bridge.decline(bridge.pendingConfirmations[0])
        XCTAssertEqual(executor.executed.map(\.id), ["d1"])
        XCTAssertTrue(bridge.pendingConfirmations.isEmpty)
        XCTAssertEqual(ackBodies["d2"]?["ok"] as? Bool, false)
        XCTAssertEqual(ackBodies["d2"]?["error"] as? String, "the owner declined it on the phone")
        // The state report followed the executed command.
        XCTAssertTrue(StubURLProtocol.requests.contains { $0.url?.path == "/v1/device/state" })
    }

    func testExpiredCommandsAreNotRun() async {
        let (bridge, executor) = makeBridge()
        bridge.consider([commandItem(id: "old", type: "timer.start", args: #"{"seconds":60}"#, age: 11 * 60)])
        await waitUntil { self.ackBodies["old"] != nil }
        XCTAssertTrue(executor.executed.isEmpty)
        XCTAssertEqual(ackBodies["old"]?["ok"] as? Bool, false)
    }

    func testCommandsFromBeforeThisInstallAreIgnored() async {
        let (bridge, executor) = makeBridge(activatedAt: Date())
        bridge.consider([commandItem(id: "before", type: "alarm.delete", args: #"{"id":"a1"}"#, age: 120)])
        try? await Task.sleep(for: .milliseconds(100))
        XCTAssertTrue(bridge.pendingConfirmations.isEmpty, "no prompt for history the timeline replays")
        XCTAssertTrue(executor.executed.isEmpty)
        bridge.consider([commandItem(id: "after", type: "alarm.set", args: #"{"time":"07:00"}"#, age: 0)])
        await waitUntil { !executor.executed.isEmpty }
        XCTAssertEqual(executor.executed.map(\.id), ["after"])
    }

    func testEventsQueueOfflineAndSurviveRestarts() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/device/events" ? .init(status: 404, body: #"{"error":{"code":"not_found","message":"x"}}"#) : .init(body: "{}")
        }
        let (bridge, _) = makeBridge()
        bridge.record(DeviceEvent(type: .appOpen, data: ["app": .string("Instagram")], id: "e1"))
        bridge.record(DeviceEvent(type: .focusChange, data: ["mode": .string("work")], id: "e2"))
        await bridge.flush()
        XCTAssertEqual(bridge.queuedEvents, 2, "kept while the server lacks the route")
        XCTAssertFalse(bridge.serverSupportsDevice)

        StubURLProtocol.requests = []
        StubURLProtocol.handler = { _ in .init(body: #"{"accepted":2,"duplicates":[],"results":[]}"#) }
        let client = StubURLProtocol.makeClient()
        let relaunched = DeviceBridge(client: { DeviceClient(client: client) }, fileURL: fileURL)
        XCTAssertEqual(relaunched.queuedEvents, 2)
        await relaunched.flush()
        XCTAssertEqual(relaunched.queuedEvents, 0)
        let body = try XCTUnwrap(StubURLProtocol.requests.first { $0.url?.path == "/v1/device/events" }?.httpBody)
        let events = try XCTUnwrap((JSONSerialization.jsonObject(with: body) as? [String: Any])?["events"] as? [[String: Any]])
        XCTAssertEqual(events.map { $0["id"] as? String }, ["e1", "e2"])
        XCTAssertEqual(events.first?["type"] as? String, "app_open")
        XCTAssertEqual(events.first?["source"] as? String, "ios")
        XCTAssertNotNil(events.first?["ts"] as? Int)
    }

    /// A term of events must reach the server: a body over its limit would lose the whole snapshot.
    func testASnapshotStaysWithinTheBodyLimit() throws {
        func event(_ i: Int, notes: Int) -> JSONValue {
            .object([
                "id": .string("CAL:\(String(repeating: "x", count: 100))@\(i)"),
                "title": .string("Software Security \(i)"),
                "location": .string("5220-014 Seminarlokale"),
                "notes": .string(String(repeating: "n", count: notes)),
            ])
        }
        var state = DeviceState()
        state.events = (0..<300).map { event($0, notes: 100) }
        let small = try state.encodedWithinLimit()
        XCTAssertTrue(String(decoding: small, as: UTF8.self).contains("nnnn"), "fits: notes stay")

        state.events = (0..<300).map { event($0, notes: 1000) }
        let noNotes = try state.encodedWithinLimit()
        XCTAssertLessThanOrEqual(noNotes.count, DeviceState.bodyLimit)
        XCTAssertFalse(String(decoding: noNotes, as: UTF8.self).contains("nnnn"), "notes go first")
        XCTAssertTrue(String(decoding: noNotes, as: UTF8.self).contains("Software Security 299"), "every event stays")

        state.events = (0..<300).map { event($0, notes: 0) }
        let few = try state.encodedWithinLimit(limit: 20_000)
        XCTAssertLessThanOrEqual(few.count, 20_000)
        let text = String(decoding: few, as: UTF8.self)
        XCTAssertTrue(text.contains("Software Security 0\""), "the nearest stay")
        XCTAssertFalse(text.contains("Software Security 299"), "the farthest go")
    }
}
