import Foundation

// Parcels: Lucina tracks the owner's deliveries. Contract: `/v1/parcels…` on getsecretary.tech (owner device auth),
// forwarded to Lucina's `/api/secretary/parcels…` with her JSON unchanged. Field names are camelCase and times are
// RFC 3339 UTC strings (read with `ParcelText.date`). A 503 `parcels_unavailable` means Lucina is unreachable or
// the feature is off.

/// How far a delivery has come. A stage this app doesn't know decodes to `.unknown`.
enum ParcelStage: String, Decodable, Equatable, Sendable, CaseIterable {
    case pending, registered, accepted
    case inTransit = "in_transit"
    case outForDelivery = "out_for_delivery"
    case delivered, customs
    case failedAttempt = "failed_attempt"
    case readyForPickup = "ready_for_pickup"
    case returned, exception, unknown

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = ParcelStage(rawValue: raw) ?? .unknown
    }
}

/// Whether Lucina still checks the parcel. An unknown value decodes to `.other`.
enum ParcelState: String, Decodable, Equatable, Sendable {
    case active, delivered, stopped, other

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = ParcelState(rawValue: raw) ?? .other
    }
}

/// How often Lucina checks the parcel now. An unknown value decodes to `.other`.
enum ParcelSchedule: String, Decodable, Equatable, Sendable {
    case hourly, daily, stopped, other

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = ParcelSchedule(rawValue: raw) ?? .other
    }
}

struct ParcelStatus: Decodable, Equatable, Sendable {
    var stage: ParcelStage
    var text: String?
    var lastUpdate: String?
    /// `2026-09-28`: a day, not a moment.
    var expectedDelivery: String?
    var location: String?
    var destinationCountry: String?

    private enum CodingKeys: String, CodingKey {
        case stage, text, lastUpdate, expectedDelivery, location, destinationCountry
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        stage = try c.decodeIfPresent(ParcelStage.self, forKey: .stage) ?? .unknown
        text = try c.decodeIfPresent(String.self, forKey: .text)
        lastUpdate = try c.decodeIfPresent(String.self, forKey: .lastUpdate)
        expectedDelivery = try c.decodeIfPresent(String.self, forKey: .expectedDelivery)
        location = try c.decodeIfPresent(String.self, forKey: .location)
        destinationCountry = try c.decodeIfPresent(String.self, forKey: .destinationCountry)
    }

    init(stage: ParcelStage, text: String? = nil, lastUpdate: String? = nil, expectedDelivery: String? = nil,
         location: String? = nil, destinationCountry: String? = nil) {
        self.stage = stage
        self.text = text
        self.lastUpdate = lastUpdate
        self.expectedDelivery = expectedDelivery
        self.location = location
        self.destinationCountry = destinationCountry
    }
}

struct ParcelEvent: Decodable, Equatable, Sendable {
    var time: String?
    var description: String
    var location: String?
    var stage: ParcelStage

    private enum CodingKeys: String, CodingKey { case time, description, location, stage }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        time = try c.decodeIfPresent(String.self, forKey: .time)
        description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
        location = try c.decodeIfPresent(String.self, forKey: .location)
        stage = try c.decodeIfPresent(ParcelStage.self, forKey: .stage) ?? .unknown
    }

    init(time: String? = nil, description: String, location: String? = nil, stage: ParcelStage = .unknown) {
        self.time = time
        self.description = description
        self.location = location
        self.stage = stage
    }
}

struct Parcel: Decodable, Identifiable, Equatable, Sendable {
    var id: String
    var number: String
    /// The scraper's carrier id (`postnord`).
    var carrier: String?
    var label: String?
    var state: ParcelState
    var schedule: ParcelSchedule
    /// Near its destination: the reason for an hourly schedule.
    var near: Bool
    var addedAt: String?
    var lastChecked: String?
    var nextCheck: String?
    var lastError: String?
    var status: ParcelStatus?
    /// Newest first, at most 50; empty before the first successful check.
    var events: [ParcelEvent]

    private enum CodingKeys: String, CodingKey {
        case id, number, carrier, label, state, schedule, near, addedAt, lastChecked, nextCheck, lastError, status, events
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        number = try c.decode(String.self, forKey: .number)
        carrier = try c.decodeIfPresent(String.self, forKey: .carrier)
        label = try c.decodeIfPresent(String.self, forKey: .label)
        state = try c.decodeIfPresent(ParcelState.self, forKey: .state) ?? .active
        schedule = try c.decodeIfPresent(ParcelSchedule.self, forKey: .schedule) ?? .daily
        near = try c.decodeIfPresent(Bool.self, forKey: .near) ?? false
        addedAt = try c.decodeIfPresent(String.self, forKey: .addedAt)
        lastChecked = try c.decodeIfPresent(String.self, forKey: .lastChecked)
        nextCheck = try c.decodeIfPresent(String.self, forKey: .nextCheck)
        lastError = try c.decodeIfPresent(String.self, forKey: .lastError)
        status = try c.decodeIfPresent(ParcelStatus.self, forKey: .status)
        events = try c.decodeIfPresent([ParcelEvent].self, forKey: .events) ?? []
    }

    init(id: String, number: String, carrier: String? = nil, label: String? = nil, state: ParcelState = .active,
         schedule: ParcelSchedule = .daily, near: Bool = false, addedAt: String? = nil, lastChecked: String? = nil,
         nextCheck: String? = nil, lastError: String? = nil, status: ParcelStatus? = nil, events: [ParcelEvent] = []) {
        self.id = id
        self.number = number
        self.carrier = carrier
        self.label = label
        self.state = state
        self.schedule = schedule
        self.near = near
        self.addedAt = addedAt
        self.lastChecked = lastChecked
        self.nextCheck = nextCheck
        self.lastError = lastError
        self.status = status
        self.events = events
    }
}

/// `GET /v1/parcels`.
struct ParcelListResponse: Decodable, Equatable, Sendable {
    var parcels: [Parcel]
    /// The local hour Lucina's daily check and digest run at.
    var digestHour: Int?

    private enum CodingKeys: String, CodingKey { case parcels, digestHour }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        parcels = try c.decodeIfPresent([Parcel].self, forKey: .parcels) ?? []
        digestHour = try c.decodeIfPresent(Int.self, forKey: .digestHour)
    }

    init(parcels: [Parcel], digestHour: Int? = nil) {
        self.parcels = parcels
        self.digestHour = digestHour
    }
}

/// `{"parcel": Parcel}`: the answer of add, get, refresh and rename.
struct ParcelResponse: Decodable, Equatable, Sendable {
    var parcel: Parcel
}

/// `POST /v1/parcels`. The carrier is detected when none is given.
struct ParcelAddRequest: Encodable, Equatable, Sendable {
    var number: String
    var carrier: String?
    var label: String?
}

/// `PATCH /v1/parcels/{id}`. A nil label is sent as `null` and clears it.
struct ParcelLabelRequest: Encodable, Equatable, Sendable {
    var label: String?

    private enum CodingKeys: String, CodingKey { case label }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(label, forKey: .label)
    }
}

struct ParcelsAPI: Sendable {
    let client: APIClient

    static func segment(_ id: String) -> String {
        id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? id
    }

    func list() async throws -> ParcelListResponse {
        try await client.get("/v1/parcels")
    }

    /// Detects the carrier, saves and tracks once, so the wait is longer than a plain read. A number that is
    /// already tracked comes back as that parcel, so a retry after a dropped connection is harmless.
    func add(_ request: ParcelAddRequest) async throws -> Parcel {
        let response: ParcelResponse = try await client.send("POST", "/v1/parcels", body: request, timeout: 45, idempotent: true)
        return response.parcel
    }

    func get(_ id: String) async throws -> Parcel {
        let response: ParcelResponse = try await client.get("/v1/parcels/\(Self.segment(id))")
        return response.parcel
    }

    /// Checks the carrier now, unless the last check is under 10 minutes old (then the stored parcel).
    func refresh(_ id: String) async throws -> Parcel {
        let response = try await client.send("POST", "/v1/parcels/\(Self.segment(id))/refresh", body: nil, timeout: 45, idempotent: true)
        return try client.decode(ParcelResponse.self, from: response).parcel
    }

    /// A label of at most 80 characters, or nil to clear it.
    func rename(_ id: String, label: String?) async throws -> Parcel {
        let response: ParcelResponse = try await client.send("PATCH", "/v1/parcels/\(Self.segment(id))",
                                                             body: ParcelLabelRequest(label: label))
        return response.parcel
    }

    func remove(_ id: String) async throws {
        _ = try await client.send("DELETE", "/v1/parcels/\(Self.segment(id))", body: nil)
    }
}
