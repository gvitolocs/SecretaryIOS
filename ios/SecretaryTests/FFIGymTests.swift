import XCTest
@testable import Secretary

/// Runs the real Rust engine (SecretaryCore.xcframework) on temporary files.
final class FFIGymTests: XCTestCase {
    private var directory: URL!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent("ffi-gym-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDown() {
        try? FileManager.default.removeItem(at: directory)
    }

    private func transport(_ name: String = "gym.json") throws -> FFIGymTransport {
        FFIGymTransport(engine: try FFIGymEngine(fileURL: directory.appendingPathComponent(name)))
    }

    private func send(_ transport: FFIGymTransport, _ command: GymCommand, revision: Int, opId: String = UUID().uuidString) async throws -> GymResponse {
        try await transport.send(EngineRequest(opId: opId, expectedRevision: revision, timezone: "Europe/Rome", command: command))
    }

    func testSnapshotCommandsConflictAndPersistence() async throws {
        let engine = try transport()
        let snapshot = try await engine.state(timezone: "Europe/Rome")
        let view = try XCTUnwrap(snapshot.view)
        XCTAssertTrue(view.state.exercises.contains { $0.id == "bench-press" }, "the engine seeds its catalog")
        XCTAssertTrue(view.state.routines.contains { $0.name == "Full Body" })
        let revision = view.state.revision

        let started = try await send(engine, .startWorkout(title: "Workout", routineId: nil), revision: revision, opId: "op-start")
        XCTAssertTrue(started.ok)
        XCTAssertEqual(started.viewRevision, revision + 1)

        // Same op_id, same command: a replay, nothing changes.
        let replay = try await send(engine, .startWorkout(title: "Workout", routineId: nil), revision: revision, opId: "op-start")
        XCTAssertTrue(replay.ok)
        XCTAssertTrue(replay.replayed)
        XCTAssertEqual(replay.viewRevision, revision + 1)

        // A stale revision is a conflict, returned (not thrown) like the HTTP transport.
        let stale = try await send(engine, .addExercise(exerciseId: "bench-press"), revision: revision)
        XCTAssertFalse(stale.ok)
        XCTAssertEqual(stale.code, "conflict")

        let invalid = try await send(engine, .finishWorkout, revision: revision + 1)
        XCTAssertFalse(invalid.ok)
        XCTAssertEqual(invalid.code, "invalid_command")
        XCTAssertNotNil(invalid.error)

        // A second handle while the first is open is refused (the engine locks its file).
        XCTAssertThrowsError(try transport())
    }

    func testTheEngineFileSurvivesClosing() async throws {
        func startWorkout() async throws {
            let engine = try transport()
            let revision = try await engine.state(timezone: "UTC").view?.state.revision ?? 0
            let started = try await send(engine, .startWorkout(title: "Legs", routineId: nil), revision: revision)
            XCTAssertTrue(started.ok)
        } // the handle closes here
        try await startWorkout()
        let reopened = try transport()
        let again = try await reopened.state(timezone: "UTC")
        XCTAssertEqual(again.view?.state.active?.title, "Legs")
    }

    func testStoreRunsOnTheDeviceEngine() async throws {
        let store = await GymStore(transport: try transport())
        await store.refresh()
        let started = await store.run(.startWorkout(title: "Push", routineId: nil))
        XCTAssertNotNil(started)
        let exerciseAdded = await store.run(.addExercise(exerciseId: "bench-press"))
        XCTAssertNotNil(exerciseAdded)
        let active = await store.view?.state.active
        XCTAssertEqual(active?.exercises.first?.exerciseId, "bench-press")
        let clock = await store.query(.clock)
        XCTAssertNotNil(clock?.clock)
    }

    func testBackupSyncBetweenTwoEngines() async throws {
        let phone = try transport("phone.json")
        let server = try transport("server.json")
        var revision = try await phone.state(timezone: "UTC").view?.state.revision ?? 0
        _ = try await send(phone, .createExercise(name: "Sled Push", muscle: "Legs", equipment: "Sled", kind: "weight_reps"), revision: revision)
        revision += 1
        try await GymSync(device: phone, server: server).upload()
        let copied = try await server.state(timezone: "UTC").view
        XCTAssertTrue(copied?.state.exercises.contains { $0.name == "Sled Push" } == true, "upload replaced the server's data")

        _ = try await send(server, .createExercise(name: "Farmer Carry", muscle: "Grip", equipment: "Dumbbell", kind: "distance_time"),
                           revision: copied?.state.revision ?? 0)
        try await GymSync(device: phone, server: server).download()
        let back = try await phone.state(timezone: "UTC").view
        XCTAssertTrue(back?.state.exercises.contains { $0.name == "Farmer Carry" } == true, "download replaced the phone's data")
        _ = revision
    }

    func testEngineVersionListsModules() {
        XCTAssertFalse(FFIGymEngine.version.isEmpty)
    }
}
