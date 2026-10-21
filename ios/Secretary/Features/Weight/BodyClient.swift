import Foundation

// Everything the app knows about the body-weight engine's wire format lives in this
// file, because the contract is still settling (docs/server-api.md "Body module" vs the
// web mock's older "assumed" shape). If the server renames something, only this file
// and its tests should change.
//
//   GET  /v1/body/state?timezone=…  -> {ok, view} (a bare view is accepted too)
//   POST /v1/body/command           -> {op_id, expected_revision, timezone, command}
//                                      engine body on 200/409/422/500
//
// Compatibility choices (both shapes accept these):
// - writes send `weight_grams` (integer), never the `weight` text, and always send
//   `measured_ms` and `note` (the mock requires them; the doc makes them optional);
// - labels (`labels`, `stat_labels`) are used when present, else the same one-decimal
//   formatting is done here.

enum BodyUnit: String, CaseIterable, Sendable {
    case kg, lb

    static let gramsPerPound = 453.59237

    var title: String { self == .kg ? "Kilograms" : "Pounds" }

    /// The value in this unit (for charts and edit fields).
    func value(grams: Int) -> Double {
        self == .lb ? Double(grams) / Self.gramsPerPound : Double(grams) / 1000
    }

    /// One decimal, no trailing ".0" ("80.5", "80").
    func number(grams: Int) -> String {
        let fixed = String(format: "%.1f", value(grams: grams))
        return fixed.hasSuffix(".0") ? String(fixed.dropLast(2)) : fixed
    }

    func label(grams: Int) -> String { "\(number(grams: grams)) \(rawValue)" }

    /// "+1.2 kg" / "-0.4 kg" / "0 kg", like the engine's signed stat labels.
    func signedLabel(grams: Int) -> String {
        if grams == 0 { return "0 \(rawValue)" }
        return "\(grams > 0 ? "+" : "-")\(number(grams: abs(grams))) \(rawValue)"
    }

    /// Mirrors the engine's `parse_weight`: up to three decimals, `,` or `.` as the
    /// separator, lb → g with 1 lb = 453.59237 g rounded half up to the gram. Range checks
    /// stay with the engine.
    func parse(_ text: String) -> Int? {
        let normalized = text.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: ",", with: ".")
        let parts = normalized.split(separator: ".", omittingEmptySubsequences: false)
        guard (1...2).contains(parts.count), let whole = parts.first, !whole.isEmpty, whole.allSatisfy(\.isASCIIDigit) else { return nil }
        let fraction = parts.count == 2 ? String(parts[1]) : ""
        guard fraction.count <= 3, fraction.allSatisfy(\.isASCIIDigit), whole.count <= 12 else { return nil }
        guard let wholeValue = Int(whole), let fractionValue = Int((fraction + "000").prefix(3)) else { return nil }
        let milli = wholeValue * 1000 + fractionValue
        switch self {
        case .kg: return milli
        case .lb: return (milli * 45_359_237 + 50_000_000) / 100_000_000
        }
    }
}

private extension Character {
    var isASCIIDigit: Bool { isASCII && isNumber }
}

struct BodySettings: Decodable, Equatable, Sendable {
    var unit: BodyUnit
    var goalGrams: Int?

    init(unit: BodyUnit, goalGrams: Int?) {
        self.unit = unit
        self.goalGrams = goalGrams
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        unit = BodyUnit(rawValue: c.lenient(String.self, .unit, "kg")) ?? .kg
        goalGrams = c.lenientOptional(Int.self, .goalGrams)
    }

    private enum CodingKeys: String, CodingKey {
        case unit
        case goalGrams = "goal_grams"
    }
}

struct BodyEntry: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let weightGrams: Int
    let measuredMs: Int
    let note: String

    var date: Date { Date(timeIntervalSince1970: Double(measuredMs) / 1000) }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = c.lenient(String.self, .id, "")
        weightGrams = c.lenient(Int.self, .weightGrams, 0)
        measuredMs = c.lenient(Int.self, .measuredMs, 0)
        note = c.lenient(String.self, .note, "")
    }

    private enum CodingKeys: String, CodingKey {
        case id, note
        case weightGrams = "weight_grams"
        case measuredMs = "measured_ms"
    }
}

struct BodyStats: Decodable, Equatable, Sendable {
    let count: Int?
    let currentGrams: Int?
    let avg7Grams: Int?
    let weeklyChangeGrams: Int?
    let minGrams: Int?
    let maxGrams: Int?
    let trend30Grams: Int?
    let trend90Grams: Int?
    let goalGrams: Int?
    let goalDistanceGrams: Int?
    let goalReached: Bool?

    static let empty = BodyStats()

    private init() {
        count = nil; currentGrams = nil; avg7Grams = nil; weeklyChangeGrams = nil; minGrams = nil; maxGrams = nil
        trend30Grams = nil; trend90Grams = nil; goalGrams = nil; goalDistanceGrams = nil; goalReached = nil
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        count = c.lenientOptional(Int.self, .count)
        currentGrams = c.lenientOptional(Int.self, .currentGrams)
        avg7Grams = c.lenientOptional(Int.self, .avg7Grams)
        weeklyChangeGrams = c.lenientOptional(Int.self, .weeklyChangeGrams)
        minGrams = c.lenientOptional(Int.self, .minGrams)
        maxGrams = c.lenientOptional(Int.self, .maxGrams)
        trend30Grams = c.lenientOptional(Int.self, .trend30Grams)
        trend90Grams = c.lenientOptional(Int.self, .trend90Grams)
        goalGrams = c.lenientOptional(Int.self, .goalGrams)
        goalDistanceGrams = c.lenientOptional(Int.self, .goalDistanceGrams)
        goalReached = c.lenientOptional(Bool.self, .goalReached)
    }

    private enum CodingKeys: String, CodingKey {
        case count
        case currentGrams = "current_grams"
        case avg7Grams = "avg7_grams"
        case weeklyChangeGrams = "weekly_change_grams"
        case minGrams = "min_grams"
        case maxGrams = "max_grams"
        case trend30Grams = "trend30_grams"
        case trend90Grams = "trend90_grams"
        case goalGrams = "goal_grams"
        case goalDistanceGrams = "goal_distance_grams"
        case goalReached = "goal_reached"
    }
}

struct BodySeriesPoint: Decodable, Identifiable, Equatable, Sendable {
    var id: String { entryId ?? "\(measuredMs)" }
    let entryId: String?
    let measuredMs: Int
    let weightGrams: Int
    let avg7Grams: Int?

    var date: Date { Date(timeIntervalSince1970: Double(measuredMs) / 1000) }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        entryId = c.lenientOptional(String.self, .entryId)
        measuredMs = c.lenient(Int.self, .measuredMs, 0)
        weightGrams = c.lenient(Int.self, .weightGrams, 0)
        avg7Grams = c.lenientOptional(Int.self, .avg7Grams)
    }

    private enum CodingKeys: String, CodingKey {
        case entryId = "entry_id"
        case measuredMs = "measured_ms"
        case weightGrams = "weight_grams"
        case avg7Grams = "avg7_grams"
    }
}

struct BodyView: Decodable, Equatable, Sendable {
    let revision: Int
    let settings: BodySettings
    /// Newest first.
    let entries: [BodyEntry]
    let stats: BodyStats
    /// Oldest first, with the 7-day average at each point. Absent on older engines.
    let series: [BodySeriesPoint]?
    /// `{entry_id: "80.5 kg"}` (doc shape; absent on the mock).
    let labels: [String: String]
    /// `current`, `avg7`, `weekly_change`, `min`, `max`, `trend30`, `trend90`, `goal`, `goal_distance`.
    let statLabels: [String: String]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // Tolerate revision/settings/entries nested under `state`, like the web client does.
        let inner = try? c.nestedContainer(keyedBy: CodingKeys.self, forKey: .state)
        guard let revision = c.lenientOptional(Int.self, .revision) ?? inner?.lenientOptional(Int.self, .revision) else {
            throw DecodingError.keyNotFound(CodingKeys.revision, .init(codingPath: c.codingPath, debugDescription: "body view without revision"))
        }
        self.revision = revision
        settings = c.lenientOptional(BodySettings.self, .settings) ?? inner?.lenientOptional(BodySettings.self, .settings)
            ?? BodySettings(unit: .kg, goalGrams: nil)
        entries = (c.lenientOptional([BodyEntry].self, .entries) ?? inner?.lenientOptional([BodyEntry].self, .entries) ?? [])
            .filter { !$0.id.isEmpty }
        stats = c.lenientOptional(BodyStats.self, .stats) ?? inner?.lenientOptional(BodyStats.self, .stats) ?? .empty
        series = c.lenientOptional([BodySeriesPoint].self, .series) ?? inner?.lenientOptional([BodySeriesPoint].self, .series)
        labels = c.lenient([String: String].self, .labels, [:])
        statLabels = c.lenient([String: String].self, .statLabels, [:])
    }

    private enum CodingKeys: String, CodingKey {
        case revision, settings, entries, stats, series, labels, state
        case statLabels = "stat_labels"
    }

    var unit: BodyUnit { settings.unit }

    func label(for entry: BodyEntry) -> String { labels[entry.id] ?? unit.label(grams: entry.weightGrams) }

    /// The engine's label for a stat, else the same formatting from the gram value.
    func statLabel(_ key: String) -> String? {
        if let label = statLabels[key] { return label }
        switch key {
        case "current": return stats.currentGrams.map(unit.label)
        case "avg7": return stats.avg7Grams.map(unit.label)
        case "weekly_change": return stats.weeklyChangeGrams.map(unit.signedLabel)
        case "min": return stats.minGrams.map(unit.label)
        case "max": return stats.maxGrams.map(unit.label)
        case "trend30": return stats.trend30Grams.map(unit.signedLabel)
        case "trend90": return stats.trend90Grams.map(unit.signedLabel)
        case "goal": return (stats.goalGrams ?? settings.goalGrams).map(unit.label)
        case "goal_distance": return stats.goalDistanceGrams.map(unit.signedLabel)
        default: return nil
        }
    }
}

struct BodyResponse: Decodable, Equatable, EngineResponse {
    let ok: Bool
    let replayed: Bool
    let code: String?
    let error: String?
    let view: BodyView?
    let weightGrams: Int?

    var viewRevision: Int? { view?.revision }

    init(view: BodyView) {
        ok = true; replayed = false; code = nil; error = nil; self.view = view; weightGrams = nil
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ok = c.lenient(Bool.self, .ok, false)
        replayed = c.lenient(Bool.self, .replayed, false)
        code = c.lenientOptional(String.self, .code)
        error = c.lenientOptional(String.self, .error)
        view = try c.decodeIfPresent(BodyView.self, forKey: .view)
        weightGrams = c.lenientOptional(Int.self, .weightGrams)
    }

    private enum CodingKeys: String, CodingKey {
        case ok, replayed, code, error, view
        case weightGrams = "weight_grams"
    }
}

enum BodyCommand: Encodable, Equatable, Sendable {
    case logEntry(weightGrams: Int, measuredMs: Int, note: String)
    case editEntry(entryId: String, weightGrams: Int, measuredMs: Int, note: String)
    case deleteEntry(entryId: String)
    /// `goalGrams: nil` clears the goal (sent as an explicit null).
    case updateSettings(unit: BodyUnit, goalGrams: Int?)

    private enum Keys: String, CodingKey {
        case type, note, unit
        case weightGrams = "weight_grams"
        case measuredMs = "measured_ms"
        case entryId = "entry_id"
        case goalGrams = "goal_grams"
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .logEntry(grams, measured, note):
            try c.encode("log_entry", forKey: .type)
            try c.encode(grams, forKey: .weightGrams)
            try c.encode(measured, forKey: .measuredMs)
            try c.encode(note, forKey: .note)
        case let .editEntry(id, grams, measured, note):
            try c.encode("edit_entry", forKey: .type)
            try c.encode(id, forKey: .entryId)
            try c.encode(grams, forKey: .weightGrams)
            try c.encode(measured, forKey: .measuredMs)
            try c.encode(note, forKey: .note)
        case let .deleteEntry(id):
            try c.encode("delete_entry", forKey: .type)
            try c.encode(id, forKey: .entryId)
        case let .updateSettings(unit, goal):
            try c.encode("update_settings", forKey: .type)
            try c.encode(unit.rawValue, forKey: .unit)
            if let goal { try c.encode(goal, forKey: .goalGrams) } else { try c.encodeNil(forKey: .goalGrams) }
        }
    }
}

/// The body engine over HTTP. Like `GymTransport`, engine outcomes are returned and only
/// transport failures throw.
struct BodyClient: Sendable {
    let client: APIClient

    func state(timezone: String) async throws -> BodyResponse {
        let response = try await client.send("GET", "/v1/body/state", query: [URLQueryItem(name: "timezone", value: timezone)], body: nil)
        return try Self.decodeStateBody(response.data)
    }

    func send(_ request: EngineRequest<BodyCommand>) async throws -> BodyResponse {
        let body: Data
        do {
            body = try APIClient.encoder.encode(request)
        } catch {
            throw APIError.decoding(message: "Could not encode the body command: \(error)")
        }
        let response = try await client.send("POST", "/v1/body/command", body: body, passThrough: HTTPGymTransport.engineStatuses)
        if response.status == 500, !HTTPGymTransport.looksLikeEngineBody(response.data) {
            throw APIClient.error(for: response)
        }
        do {
            return try APIClient.decoder.decode(BodyResponse.self, from: response.data)
        } catch {
            if (200..<300).contains(response.status) { throw APIError.decoding(message: String(describing: error)) }
            throw APIClient.error(for: response)
        }
    }

    static func decodeStateBody(_ data: Data) throws -> BodyResponse {
        if let response = try? APIClient.decoder.decode(BodyResponse.self, from: data), response.view != nil {
            return response
        }
        do {
            return BodyResponse(view: try APIClient.decoder.decode(BodyView.self, from: data))
        } catch {
            throw APIError.decoding(message: String(describing: error))
        }
    }
}
