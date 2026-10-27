import Foundation
import SecretaryCore

/// The Rust gym engine (`secretary-ffi`, `SecretaryCore.h`) running in-process on a local
/// JSON file: the same engine and JSON protocol as `/v1/gym/*`, with no network.
///
/// Calls into the engine are synchronous, so they run on a private serial queue (one
/// request at a time per handle, as the header requires) and never block the main thread.
final class FFIGymEngine: @unchecked Sendable {
    enum EngineError: LocalizedError {
        case open(String)
        case emptyResponse

        var errorDescription: String? {
            switch self {
            case let .open(message): return String(localized: "Couldn't open your gym data: \(message)")
            case .emptyResponse: return String(localized: "The gym engine returned no answer.")
            }
        }
    }

    /// Set once by `openFile` (on `queue`, or in the throwing init); read on `queue`.
    private var handle: OpaquePointer?
    private var openFailure: String?
    private let queue = DispatchQueue(label: "tech.getsecretary.gym-engine", qos: .userInitiated)
    let fileURL: URL

    init(fileURL: URL) throws {
        self.fileURL = fileURL
        openFile()
        if let openFailure { throw EngineError.open(openFailure) }
    }

    /// App launch: gym.json is read and parsed on the engine's queue, not on the main thread.
    /// Requests queue up behind the open; `openError()` says whether it worked.
    init(openingInBackground fileURL: URL) {
        self.fileURL = fileURL
        queue.async { [self] in openFile() }
    }

    private func openFile() {
        do {
            try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        } catch {
            openFailure = error.localizedDescription
            return
        }
        guard let handle = fileURL.path.withCString({ secretary_gym_open($0) }) else {
            var message = "unknown error"
            if let raw = secretary_open_error() {
                message = String(cString: raw)
                secretary_string_free(raw)
            }
            openFailure = message
            return
        }
        self.handle = handle
    }

    /// Why the engine could not open its file, once the open has run (nil: it is open).
    func openError() async -> String? {
        await withCheckedContinuation { continuation in
            queue.async { [self] in continuation.resume(returning: handle == nil ? (openFailure ?? "unknown error") : nil) }
        }
    }

    deinit {
        // Requests hold a strong reference to the engine, so none is in flight here.
        if let handle { secretary_gym_close(handle) }
    }

    /// Sends one JSON request and returns the engine's JSON response.
    func request(_ json: Data) async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            queue.async { [self] in
                guard let handle else {
                    continuation.resume(throwing: EngineError.open(openFailure ?? "unknown error"))
                    return
                }
                let text = String(decoding: json, as: UTF8.self)
                guard let raw = text.withCString({ secretary_gym_request(handle, $0) }) else {
                    continuation.resume(throwing: EngineError.emptyResponse)
                    return
                }
                let response = Data(String(cString: raw).utf8)
                secretary_string_free(raw)
                continuation.resume(returning: response)
            }
        }
    }

    static var version: String {
        guard let raw = secretary_version() else { return "?" }
        defer { secretary_string_free(raw) }
        return String(cString: raw)
    }

    static var defaultFileURL: URL {
        let base = (try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("Secretary", isDirectory: true).appendingPathComponent("gym.json")
    }
}

/// `GymTransport` over the on-device engine. Engine outcomes (conflict, invalid_command,
/// storage_error) come back as responses, exactly like the HTTP transport.
struct FFIGymTransport: GymTransport {
    let engine: FFIGymEngine

    func state(timezone: String) async throws -> GymResponse {
        try await send(EngineRequest(opId: UUID().uuidString.lowercased(), expectedRevision: 0, timezone: timezone, command: .snapshot))
    }

    func send(_ request: EngineRequest<GymCommand>) async throws -> GymResponse {
        let body = try APIClient.encoder.encode(request)
        let data = try await engine.request(body)
        do {
            return try APIClient.decoder.decode(GymResponse.self, from: data)
        } catch {
            throw APIError.decoding(message: String(describing: error))
        }
    }
}

/// Moves the whole gym history between this iPhone and the server, using the engines'
/// own backup format (`export_json` → `restore_json`). The device engine and the server
/// engine each create their own ids, so commands cannot be replayed from one to the
/// other; a full backup is the safe unit of sync.
struct GymSync: Sendable {
    let device: GymTransport
    let server: GymTransport

    enum SyncError: LocalizedError {
        case failed(String)
        var errorDescription: String? {
            if case let .failed(message) = self { return message }
            return nil
        }
    }

    /// Replaces the server's gym data with this iPhone's.
    func upload() async throws { try await copy(from: device, to: server, name: "the server") }

    /// Replaces this iPhone's gym data with the server's.
    func download() async throws { try await copy(from: server, to: device, name: "this iPhone") }

    private func copy(from source: GymTransport, to target: GymTransport, name: String) async throws {
        let tz = EngineClock.timezone
        let export = try await source.send(EngineRequest(opId: UUID().uuidString.lowercased(), expectedRevision: 0,
                                                         timezone: tz, command: .exportJSON))
        guard export.ok, let backup = export.text else { throw SyncError.failed(export.error ?? String(localized: "Couldn't export the gym data.")) }
        let current = try await target.state(timezone: tz)
        guard let revision = current.view?.state.revision else { throw SyncError.failed(current.error ?? String(localized: "Couldn't read \(name).")) }
        let restored = try await target.send(EngineRequest(opId: UUID().uuidString.lowercased(), expectedRevision: revision,
                                                           timezone: tz, command: .restoreJSON(backup: backup)))
        guard restored.ok else { throw SyncError.failed(restored.error ?? String(localized: "Couldn't restore the gym data on \(name).")) }
    }
}
