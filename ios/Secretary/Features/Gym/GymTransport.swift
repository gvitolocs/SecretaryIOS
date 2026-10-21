import Foundation

/// How the gym store reaches a gym engine. The views and the store only see this
/// protocol, so the engine can live on the server (HTTP, today) or on the device
/// (`FFIGymTransport` over `secretary-ffi`'s `SecretaryCore.h`, for offline use) without
/// touching the UI.
///
/// Contract for implementations:
/// - Engine outcomes are **returned**, never thrown: `{ok:false, code:"conflict"}`,
///   `invalid_command`, `op_id_reused` and `storage_error` come back as a `GymResponse`,
///   exactly as the Rust engine produces them.
/// - Only transport failures throw (`APIError`: 401, 429, offline, timeouts).
protocol GymTransport: Sendable {
    /// The `snapshot` view.
    func state(timezone: String) async throws -> GymResponse
    /// Any command, writes and queries alike (the request JSON is the engine's).
    func send(_ request: EngineRequest<GymCommand>) async throws -> GymResponse
}

/// `GET /v1/gym/state` and `POST /v1/gym/command` on secretary-server.
struct HTTPGymTransport: GymTransport {
    let client: APIClient

    /// Statuses whose body is an engine response (docs: 409 conflict, 422 invalid/reused,
    /// 500 storage_error).
    static let engineStatuses: Set<Int> = [409, 422, 500]

    func state(timezone: String) async throws -> GymResponse {
        let response = try await client.send("GET", "/v1/gym/state", query: [URLQueryItem(name: "timezone", value: timezone)], body: nil)
        return try Self.decodeStateBody(response.data)
    }

    func send(_ request: EngineRequest<GymCommand>) async throws -> GymResponse {
        let body: Data
        do {
            body = try APIClient.encoder.encode(request)
        } catch {
            throw APIError.decoding(message: "Could not encode the gym command: \(error)")
        }
        let response = try await client.send("POST", "/v1/gym/command", body: body,
                                              timeout: request.command.type == "restore_json" ? 60 : APIClient.defaultTimeout,
                                              passThrough: Self.engineStatuses)
        if response.status == 500, !Self.looksLikeEngineBody(response.data) {
            throw APIClient.error(for: response)
        }
        do {
            return try APIClient.decoder.decode(GymResponse.self, from: response.data)
        } catch {
            if (200..<300).contains(response.status) {
                throw APIError.decoding(message: String(describing: error))
            }
            throw APIClient.error(for: response)
        }
    }

    /// The state route answers the engine response `{ok, view}`; accept a bare view too.
    static func decodeStateBody(_ data: Data) throws -> GymResponse {
        if let response = try? APIClient.decoder.decode(GymResponse.self, from: data), response.view != nil {
            return response
        }
        do {
            let view = try APIClient.decoder.decode(GymView.self, from: data)
            return GymResponse(view: view)
        } catch {
            throw APIError.decoding(message: String(describing: error))
        }
    }

    static func looksLikeEngineBody(_ data: Data) -> Bool {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return false }
        return object["ok"] is Bool
    }
}

extension GymResponse {
    /// A successful response wrapping a bare view.
    init(view: GymView) {
        ok = true
        replayed = false
        code = nil
        error = nil
        self.view = view
        text = nil
        exercises = nil
        clock = nil
        weightGrams = nil
    }
}
