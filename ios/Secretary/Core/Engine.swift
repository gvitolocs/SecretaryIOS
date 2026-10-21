import Foundation

// The command protocol shared by the gym and body engines (docs/server-api.md,
// "The command protocol"). Every write is
// `{op_id, expected_revision, timezone, command}`; the answer is
// `{ok, replayed, code?, error?, view?, ...}`.

struct EngineRequest<Command: Encodable & Sendable>: Encodable, Sendable {
    let opId: String
    let expectedRevision: Int
    let timezone: String
    let command: Command

    private enum CodingKeys: String, CodingKey {
        case opId = "op_id"
        case expectedRevision = "expected_revision"
        case timezone
        case command
    }
}

/// The parts of an engine response that the retry logic needs.
protocol EngineResponse: Sendable {
    var ok: Bool { get }
    var code: String? { get }
    var error: String? { get }
    /// The revision of the view in the response, when there is one.
    var viewRevision: Int? { get }
}

enum EngineOutcome<Response: EngineResponse>: Sendable {
    /// `ok: true` (replays included).
    case success(Response)
    /// The engine rejected the command (`invalid_command`, `op_id_reused`, `storage_error`).
    case rejected(Response)
    /// Still `conflict` after one refetch-and-retry.
    case conflict(Response)
}

enum EngineRetry {
    static let conflictMessage = "This changed somewhere else. I refreshed it; please try again."

    /// Sends one user action with conflict handling:
    /// on `conflict`, refetch the state and resend **once** with the same `op_id`
    /// (a conflict never consumes an op_id, so the resend is still idempotent) and the
    /// fresh revision. A second conflict is reported, never retried in a loop.
    ///
    /// - Parameters:
    ///   - opId: generated once per user action by the caller.
    ///   - revision: the revision the user saw.
    ///   - send: sends the request for a given `expected_revision`.
    ///   - refetch: loads the current state and returns its revision.
    static func run<Response: EngineResponse>(
        opId: String,
        revision: Int,
        send: (_ opId: String, _ expectedRevision: Int) async throws -> Response,
        refetch: () async throws -> Int
    ) async throws -> EngineOutcome<Response> {
        var expected = revision
        for attempt in 0..<2 {
            let response = try await send(opId, expected)
            if response.ok { return .success(response) }
            guard response.code == "conflict" else { return .rejected(response) }
            if attempt == 0 {
                expected = try await refetch()
                continue
            }
            return .conflict(response)
        }
        preconditionFailure("unreachable")
    }
}

/// Serialises async work on the main actor so each engine command sees the revision
/// produced by the one before it.
@MainActor
final class SerialQueue {
    private var tail: Task<Void, Never>?

    func run<T: Sendable>(_ operation: @escaping @MainActor () async -> T) async -> T {
        let previous = tail
        let task = Task { @MainActor () -> T in
            await previous?.value
            return await operation()
        }
        tail = Task { @MainActor in _ = await task.value }
        return await task.value
    }

    func reset() {
        tail = nil
    }
}

enum EngineClock {
    static var timezone: String { TimeZone.current.identifier }
}
