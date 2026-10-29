import Foundation
import XCTest
@testable import Secretary

/// A scripted gym engine: `state()` answers the current revision; `send` pops scripted answers.
final class FakeGymTransport: GymTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var _revision: Int
    private var script: [(Int) -> GymResponse]
    private(set) var sent: [EngineRequest<GymCommand>] = []
    private(set) var stateCalls = 0

    init(revision: Int, script: [(Int) -> GymResponse]) {
        _revision = revision
        self.script = script
    }

    var revision: Int {
        get { lock.withLock { _revision } }
        set { lock.withLock { _revision = newValue } }
    }

    func state(timezone: String) async throws -> GymResponse {
        lock.withLock { stateCalls += 1 }
        return try Self.response(ok: true, revision: revision)
    }

    func send(_ request: EngineRequest<GymCommand>) async throws -> GymResponse {
        let next: ((Int) -> GymResponse)? = lock.withLock {
            sent.append(request)
            return script.isEmpty ? nil : script.removeFirst()
        }
        guard let next else { throw APIError.network(message: "script exhausted") }
        return next(request.expectedRevision)
    }

    static func response(ok: Bool, revision: Int? = nil, code: String? = nil, error: String? = nil) throws -> GymResponse {
        var parts = [#""ok":\#(ok)"#, #""replayed":false"#]
        if let code { parts.append(#""code":"\#(code)""#) }
        if let error { parts.append(#""error":"\#(error)""#) }
        if let revision { parts.append(#""view":\#(Fixtures.gymView(revision: revision))"#) }
        return try JSONDecoder().decode(GymResponse.self, from: Data("{\(parts.joined(separator: ","))}".utf8))
    }

    static func conflict() -> GymResponse {
        try! response(ok: false, code: "conflict", error: "State changed. Refresh and retry the edit.")
    }
}

@MainActor
final class GymConflictTests: XCTestCase {
    private var opIds = 0

    private func makeStore(_ transport: FakeGymTransport) -> GymStore {
        GymStore(transport: transport, makeOpId: { [unowned self] in opIds += 1; return "op-\(opIds)" }, timezone: { "Europe/Rome" })
    }

    func testConflictRefetchesAndRetriesOnceWithANewOpId() async throws {
        let transport = FakeGymTransport(revision: 3, script: [
            { _ in FakeGymTransport.conflict() },
            { expected in try! FakeGymTransport.response(ok: true, revision: expected + 1) },
        ])
        let store = makeStore(transport)
        await store.refresh()
        XCTAssertEqual(store.revision, 3)

        transport.revision = 5 // someone else changed the state
        let response = await store.run(.startRest(seconds: 90))

        XCTAssertNotNil(response)
        XCTAssertEqual(transport.sent.count, 2)
        XCTAssertEqual(transport.sent.map(\.opId), ["op-1", "op-2"], "the retry gets a new op_id (docs: offline queue recipe)")
        XCTAssertEqual(transport.sent.map(\.expectedRevision), [3, 5], "the retry uses the refetched revision")
        XCTAssertEqual(transport.sent.map(\.command), [.startRest(seconds: 90), .startRest(seconds: 90)])
        XCTAssertEqual(transport.sent.first?.timezone, "Europe/Rome")
        XCTAssertEqual(transport.stateCalls, 2)
        XCTAssertEqual(store.revision, 6)
        XCTAssertNil(store.actionError)
    }

    func testSecondConflictIsReportedNotRetriedAgain() async {
        let transport = FakeGymTransport(revision: 1, script: [
            { _ in FakeGymTransport.conflict() },
            { _ in FakeGymTransport.conflict() },
            { _ in XCTFail("no third attempt"); return FakeGymTransport.conflict() },
        ])
        let store = makeStore(transport)
        await store.refresh()
        let response = await store.run(.finishWorkout)
        XCTAssertNil(response)
        XCTAssertEqual(transport.sent.count, 2)
        XCTAssertEqual(store.actionError, EngineRetry.conflictMessage)
    }

    func testValidationErrorsAreShownAndNotRetried() async {
        let transport = FakeGymTransport(revision: 2, script: [
            { _ in try! FakeGymTransport.response(ok: false, code: "invalid_command", error: "Complete at least one set before finishing") },
        ])
        let store = makeStore(transport)
        await store.refresh()
        let response = await store.run(.finishWorkout)
        XCTAssertNil(response)
        XCTAssertEqual(transport.sent.count, 1)
        XCTAssertEqual(store.actionError, "Complete at least one set before finishing")
    }

    func testCommandsAreSerialisedAndEachSeesTheNewRevision() async {
        let transport = FakeGymTransport(revision: 0, script: [
            { expected in try! FakeGymTransport.response(ok: true, revision: expected + 1) },
            { expected in try! FakeGymTransport.response(ok: true, revision: expected + 1) },
            { expected in try! FakeGymTransport.response(ok: true, revision: expected + 1) },
        ])
        let store = makeStore(transport)
        await store.refresh()
        async let a = store.run(.addSet(exerciseId: "we-1"))
        async let b = store.run(.addSet(exerciseId: "we-1"))
        async let c = store.run(.addSet(exerciseId: "we-1"))
        _ = await (a, b, c)
        XCTAssertEqual(transport.sent.map(\.expectedRevision), [0, 1, 2])
        XCTAssertEqual(Set(transport.sent.map(\.opId)).count, 3, "one op_id per user action")
        XCTAssertEqual(store.revision, 3)
    }

    func testTransportFailureLeavesStateAndReports() async {
        let transport = FakeGymTransport(revision: 4, script: [])
        let store = makeStore(transport)
        var reported: APIError?
        store.onTransportError = { reported = $0 }
        await store.refresh()
        let response = await store.run(.stopRest)
        XCTAssertNil(response)
        XCTAssertEqual(store.revision, 4)
        XCTAssertEqual(reported?.code, "network")
        XCTAssertEqual(store.actionError, "Secretary's server isn't reachable. That change wasn't saved.")
    }

    func testEngineRetryHelperDirectly() async throws {
        var calls: [(String, Int)] = []
        let outcome = try await EngineRetry.run(
            opId: "x", nextOpId: { "y" }, revision: 1,
            send: { (op: String, rev: Int) -> GymResponse in
                calls.append((op, rev))
                return rev == 1 ? FakeGymTransport.conflict() : try FakeGymTransport.response(ok: true, revision: 8)
            },
            refetch: { 7 })
        guard case let .success(response) = outcome else { return XCTFail("expected success") }
        XCTAssertEqual(response.viewRevision, 8)
        XCTAssertEqual(calls.map(\.0), ["x", "y"])
        XCTAssertEqual(calls.map(\.1), [1, 7])
    }

    func testLocalClockTick() async throws {
        let transport = FakeGymTransport(revision: 1, script: [])
        let store = makeStore(transport)
        await store.refresh()
        XCTAssertEqual(GymStore.durationText(725), "12m 05s")
        XCTAssertEqual(GymStore.durationText(3720), "1h 02m")
        XCTAssertEqual(store.restRemaining(at: Date()), 0)
    }
}
