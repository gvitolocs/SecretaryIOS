import Foundation
#if canImport(UIKit)
import UIKit
#endif

/// Builds widget sections from the Secretary API. The widget extension uses it with the device
/// token from the shared keychain (`BridgeCredentials`), so widgets stay fresh while the app is
/// closed; the app's Shortcuts use it with the app's own client. Decoding is lenient and local
/// to this file (JSONValue), so a new field in an app model never breaks the widgets.
struct WidgetFetcher: Sendable {
    let client: APIClient
    /// /v1/media/* is served by p1's Caddy: the primary URL only (the tailnet fallback is secretary-server).
    let mediaClient: APIClient

    /// The extension's client: nil when the app is signed out (no token in the shared keychain).
    static func fromBridge() -> WidgetFetcher? {
        guard let credentials = BridgeCredentials.read(), !credentials.token.isEmpty else { return nil }
        let urls = credentials.endpoints.compactMap(URL.init(string:))
        guard let primary = urls.first else { return nil }
        return WidgetFetcher(client: APIClient(endpoints: urls, accessKey: credentials.token, defaults: nil),
                             mediaClient: APIClient(endpoints: [primary], accessKey: credentials.token, defaults: nil))
    }

    static var timeZone: String { TimeZone.current.identifier }

    /// it, en or da: the UI language when the shopping catalogue has it.
    static var language: String {
        let code = Locale.preferredLanguages.first.map { String($0.prefix(2)) } ?? "en"
        return ["it", "en", "da"].contains(code) ? code : "en"
    }

    // MARK: Ask Lucina

    func ask() async throws -> AskSection {
        let page: JSONValue = try await client.get("/v1/timeline", query: [URLQueryItem(name: "limit", value: "20")])
        let items = page["items"]?.arrayValue ?? []
        let replies = items.filter { $0["role"]?.stringValue == "lucina" && $0["text"]?.nonEmptyString != nil }
        let last = replies.max { (Self.date($0["ts"]) ?? .distantPast) < (Self.date($1["ts"]) ?? .distantPast) }
        return AskSection(fetchedAt: Date(), reply: last?["text"]?.nonEmptyString.map(Self.plain), replyAt: Self.date(last?["ts"]))
    }

    // MARK: Next appointment

    func appointment(now: Date = Date()) async throws -> AppointmentSection {
        let page: JSONValue = try await client.get("/v1/calendar/events", query: [URLQueryItem(name: "days", value: "3")])
        let events = (page["events"]?.arrayValue ?? []).compactMap(Self.event).filter { $0.end > now }
        // Timed events first; an all-day event only when nothing timed is left in the window.
        let timed = events.filter { !$0.allDay }.sorted { $0.start < $1.start }
        let ordered = timed.isEmpty ? events.sorted { $0.start < $1.start } : timed
        var next = ordered.first
        if var event = next, let place = event.location, !event.allDay {
            if let travel = try? await travel(to: place, arriveBy: event.start) {
                event.travelMinutes = travel.minutes
                event.travelMode = travel.mode
            }
            next = event
        }
        return AppointmentSection(fetchedAt: Date(), next: next, then: ordered.dropFirst().first)
    }

    /// Travel time from the maps endpoint (Google Routes on the server). Nil when the server has none.
    func travel(to place: String, arriveBy: Date) async throws -> (minutes: Int, mode: String?)? {
        let query = [URLQueryItem(name: "destination", value: place),
                     URLQueryItem(name: "arrivalMs", value: String(Int64(arriveBy.timeIntervalSince1970 * 1000)))]
        guard let json: JSONValue = try? await client.get("/v1/maps/travel", query: query, timeout: 10) else { return nil }
        let seconds = json["durationS"]?.numberValue ?? json["duration_s"]?.numberValue ?? json["seconds"]?.numberValue
        guard let seconds, seconds > 0 else { return nil }
        return (Int((seconds / 60).rounded(.up)), json["mode"]?.nonEmptyString)
    }

    static func event(_ json: JSONValue) -> AppointmentSection.Event? {
        guard let id = json["id"]?.nonEmptyString, let startMs = json["startMs"]?.numberValue,
              let endMs = json["endMs"]?.numberValue else { return nil }
        return AppointmentSection.Event(id: id, title: json["title"]?.nonEmptyString ?? String(localized: "Event"),
                                        location: json["location"]?.nonEmptyString,
                                        start: Date(timeIntervalSince1970: startMs / 1000), end: Date(timeIntervalSince1970: endMs / 1000),
                                        allDay: json["allDay"]?.boolValue ?? false)
    }

    // MARK: Shopping

    /// The list `listId` (else the default list, else the first one with items), and the deals for
    /// its items. Deals are slow (the server asks the chains): `previous` deals are kept for 3 hours.
    func shopping(listId: String?, previous: ShoppingSection?) async throws -> ShoppingSection {
        let listsJSON: JSONValue = try await client.get("/v1/shopping/lists")
        let lists = listsJSON.arrayValue ?? listsJSON["lists"]?.arrayValue ?? []
        let ids = lists.compactMap { $0["id"]?.nonEmptyString }
        let chosen = listId.flatMap { ids.contains($0) ? $0 : nil }
            ?? (ids.contains("default") ? "default" : nil)
            ?? lists.first { ($0["needed"]?.numberValue ?? 0) > 0 }?["id"]?.nonEmptyString
            ?? ids.first ?? "default"
        let state: JSONValue = try await client.get("/v1/shopping/lists/\(Self.segment(chosen))",
                                                    query: [URLQueryItem(name: "lang", value: Self.language)])
        let name = state["list"]?["name"]?.nonEmptyString
            ?? lists.first { $0["id"]?.stringValue == chosen }?["name"]?.nonEmptyString ?? chosen
        let items = Self.neededItems(state["items"])
        var deals = previous?.listId == chosen ? previous?.deals ?? [] : []
        let dealsAge = previous.map { Date().timeIntervalSince($0.fetchedAt) } ?? .infinity
        if deals.isEmpty || dealsAge > 3 * 3600, let fresh = try? await self.deals() { deals = fresh }
        return ShoppingSection(fetchedAt: Date(), listId: chosen, listName: name, items: items, deals: deals)
    }

    static func neededItems(_ json: JSONValue?) -> [ShoppingSection.Item] {
        let rows = (json?.arrayValue ?? []).enumerated().compactMap { index, row -> (Double, ShoppingSection.Item)? in
            guard row["status"]?.stringValue ?? "needed" == "needed" else { return nil }
            let id: String
            switch row["id"] {
            case let .string(value) where !value.isEmpty: id = value
            case let .number(value): id = JSONValue.number(value).displayText
            default: return nil
            }
            let names = row["item"]?["names"]
            guard let name = names?[language]?.nonEmptyString ?? row["name"]?.nonEmptyString else { return nil }
            let icon = row["icon"]?.nonEmptyString
            return (row["position"]?.numberValue ?? Double(index),
                    ShoppingSection.Item(id: id, name: name, spec: row["spec"]?.stringValue ?? "",
                                         icon: icon.flatMap { $0.unicodeScalars.first?.properties.isEmojiPresentation == true ? $0 : nil }))
        }
        return rows.sorted { $0.0 < $1.0 }.map(\.1)
    }

    /// The cheapest deal for each list item, across the nearby stores.
    func deals() async throws -> [ShoppingSection.Deal] {
        let map: JSONValue = try await client.get("/v1/grocery/deals/map", timeout: 30)
        var best: [String: ShoppingSection.Deal] = [:]
        for store in map["stores"]?.arrayValue ?? [] {
            let storeName = store["chain"]?.nonEmptyString ?? store["name"]?.nonEmptyString ?? ""
            for match in store["listMatches"]?.arrayValue ?? [] {
                guard let item = match["item"]?.nonEmptyString, let deal = match["deal"],
                      let name = deal["name"]?.nonEmptyString else { continue }
                let candidate = ShoppingSection.Deal(item: item, name: name, store: storeName,
                                                     priceDkk: Self.number(deal["priceDkk"]), discountPct: Self.number(deal["discountPct"]))
                let key = item.lowercased()
                if let current = best[key], (current.priceDkk ?? .infinity) <= (candidate.priceDkk ?? .infinity) { continue }
                best[key] = candidate
            }
        }
        return best.values.sorted { ($0.discountPct ?? 0) > ($1.discountPct ?? 0) }
    }

    /// Ticks an item off (bought), like the Shopping tab.
    func purchase(listId: String, itemId: String) async throws {
        struct Op: Encodable { let opId: String; let type: String; let itemId: String }
        struct Body: Encodable { let expectedRevision: Int?; let ops: [Op] }
        let body = Body(expectedRevision: nil, ops: [Op(opId: UUID().uuidString.lowercased(), type: "purchase", itemId: itemId)])
        let _: JSONValue = try await client.send("POST", "/v1/shopping/lists/\(Self.segment(listId))/ops", body: body,
                                                 idempotent: true, headers: ["X-Secretary-Client": "ios-widget"])
    }

    // MARK: Weight

    func weight(now: Date = Date()) async throws -> WeightSection {
        let response = try await client.send("GET", "/v1/body/state", query: [URLQueryItem(name: "timezone", value: Self.timeZone)], body: nil)
        let json = try client.decode(JSONValue.self, from: response)
        let view = json["view"] ?? json
        let state = view["state"]
        let unit = (view["settings"] ?? state?["settings"])?["unit"]?.stringValue == "lb" ? "lb" : "kg"
        let entries = (view["entries"] ?? state?["entries"])?.arrayValue ?? []
        let stats = view["stats"]
        let labels = view["stat_labels"]
        let gramsPer = unit == "lb" ? 453.59237 : 1000
        func label(_ grams: Double?, signed: Bool = false) -> String? {
            guard let grams else { return nil }
            var text = String(format: "%.1f", abs(grams) / gramsPer)
            if text.hasSuffix(".0") { text.removeLast(2) }
            let sign = signed ? (grams > 0 ? "+" : grams < 0 ? "-" : "") : (grams < 0 ? "-" : "")
            return "\(sign)\(text) \(unit)"
        }
        let newest = entries.first
        let current = Self.number(stats?["current_grams"]) ?? Self.number(newest?["weight_grams"])
        let weekly = Self.number(stats?["weekly_change_grams"])
        // The last 30 days; with fewer than two weigh-ins there, the last 12 whenever they were.
        let since = now.addingTimeInterval(-30 * 86_400).timeIntervalSince1970 * 1000
        let points = (view["series"]?.arrayValue ?? []).compactMap { point -> (ms: Double, value: Double)? in
            guard let ms = Self.number(point["measured_ms"]),
                  let grams = Self.number(point["avg7_grams"]) ?? Self.number(point["weight_grams"]) else { return nil }
            return (ms, grams / gramsPer)
        }.sorted { $0.ms < $1.ms }
        let recent = points.filter { $0.ms >= since }
        let trend = (recent.count >= 2 ? recent : Array(points.suffix(12))).map(\.value)
        return WeightSection(fetchedAt: Date(), unit: unit, currentLabel: labels?["current"]?.nonEmptyString ?? label(current),
                             lastAt: Self.number(newest?["measured_ms"]).map { Date(timeIntervalSince1970: $0 / 1000) },
                             weeklyChangeLabel: labels?["weekly_change"]?.nonEmptyString ?? label(weekly, signed: true),
                             weeklyChangeGrams: weekly.map { Int($0) }, trend: trend)
    }

    // MARK: Tasks and reminders due today

    func tasks(now: Date = Date()) async throws -> TasksSection {
        async let tasksJSON: JSONValue = client.get("/v1/tasks/state", query: [URLQueryItem(name: "view", value: "today"),
                                                                               URLQueryItem(name: "timezone", value: Self.timeZone)])
        async let remindersJSON: JSONValue? = try? client.get("/v1/reminders", query: [URLQueryItem(name: "scope", value: "today")])
        let view = try await tasksJSON["view"]
        var items: [TasksSection.Item] = []
        for group in view?["list"]?["groups"]?.arrayValue ?? [] where group["kind"]?.stringValue != "completed" {
            for task in group["tasks"]?.arrayValue ?? [] where task["checked"]?.boolValue != true {
                guard let id = task["id"]?.nonEmptyString ?? task["id"]?.numberValue.map({ JSONValue.number($0).displayText }) else { continue }
                let due = task["due"]
                let rel = Int(due?["rel_days"]?.numberValue ?? 0)
                items.append(TasksSection.Item(id: id, kind: .task, title: task["content"]?.nonEmptyString.map(Self.plain) ?? "",
                                               due: Self.dueTime(date: due?["date"]?.stringValue, time: due?["time"]?.stringValue),
                                               overdue: due?["overdue"]?.boolValue ?? (rel < 0),
                                               priority: Int(task["priority"]?.numberValue ?? 4)))
            }
        }
        let endOfDay = Calendar.current.startOfDay(for: now).addingTimeInterval(86_400)
        for reminder in await remindersJSON?["reminders"]?.arrayValue ?? [] {
            guard let id = reminder["id"].map({ $0.stringValue ?? $0.displayText }), let text = reminder["text"]?.nonEmptyString else { continue }
            let due = Self.date(reminder["dueAt"])
            if let due, due >= endOfDay { continue }
            items.append(TasksSection.Item(id: id, kind: .reminder, title: text, due: due, overdue: (due ?? now) < now, priority: 4))
        }
        items.sort { a, b in
            if a.overdue != b.overdue { return a.overdue }
            switch (a.due, b.due) {
            case let (x?, y?) where x != y: return x < y
            case (_?, nil): return true
            case (nil, _?): return false
            default: return a.priority < b.priority
            }
        }
        return TasksSection(fetchedAt: Date(), items: items, completedToday: Int(view?["goal"]?["completed_today"]?.numberValue ?? 0))
    }

    /// Completes a Todoist task, or dismisses one of Lucina's reminders.
    func complete(_ id: String, kind: TasksSection.Item.Kind) async throws {
        switch kind {
        case .task:
            struct Command: Encodable { let type = "complete"; let task_id: String }
            struct Envelope: Encodable { let op_id: String; let timezone: String; let command: Command; let view: [String: String] }
            let body = Envelope(op_id: UUID().uuidString.lowercased(), timezone: Self.timeZone, command: Command(task_id: id),
                                view: ["kind": "today"])
            let _: JSONValue = try await client.send("POST", "/v1/tasks/command", body: body, idempotent: true,
                                                     headers: ["X-Secretary-Client": "ios-widget"])
        case .reminder:
            _ = try await client.send("DELETE", "/v1/reminders/\(Self.segment(id))", body: nil)
        }
    }

    // MARK: Pokémon

    func pokemon() async throws -> PokemonSection {
        let json: JSONValue = try await client.get("/v1/game/state")
        let party = (json["party"]?.arrayValue ?? []).compactMap { mon -> PokemonSection.Mon? in
            guard let name = mon["name"]?.nonEmptyString else { return nil }
            return PokemonSection.Mon(name: name, level: Int(mon["level"]?.numberValue ?? 0), hp: Int(mon["hp"]?.numberValue ?? 0),
                                      maxHp: Int(mon["max_hp"]?.numberValue ?? 0))
        }
        let notes = (json["events"]?.arrayValue ?? []).compactMap(\.nonEmptyString)
            .filter { !$0.hasPrefix("Game server restarted") && !$0.hasPrefix("Lucina is playing") }
        return PokemonSection(fetchedAt: Date(), running: json["running"]?.boolValue ?? false,
                              place: Self.place(json["map_name"]?.stringValue ?? ""), badges: Int(json["badges"]?.numberValue ?? 0),
                              objective: json["objective"]?.nonEmptyString, party: party, lastNote: notes.last)
    }

    /// The live game screen (160×144 PNG), saved for the widget.
    func pokemonScreen() async throws -> URL? {
        let data = try await client.send("GET", "/v1/game/screenshot", body: nil, headers: ["Accept": "image/png"]).data
        guard !data.isEmpty, let url = WidgetStore.imageURL("pokemon-screen.png") else { return nil }
        try data.write(to: url, options: .atomic)
        return url
    }

    /// "ROCKET_HIDEOUT_B4F" → "Rocket Hideout B4F" (as the Pokémon plugin shows it).
    static func place(_ mapName: String) -> String {
        mapName.lowercased().split(separator: "_").map { word -> String in
            let w = String(word)
            return w.range(of: #"^b?\d+f$"#, options: .regularExpression) != nil ? w.uppercased() : w.prefix(1).uppercased() + w.dropFirst()
        }.joined(separator: " ")
    }

    // MARK: Films

    func films() async throws -> FilmsSection {
        let page: JSONValue = try await mediaClient.get("/v1/media/progress", query: [URLQueryItem(name: "since", value: "0")])
        let entries = (page["entries"]?.arrayValue ?? []).filter { entry in
            let position = Self.number(entry["position_ms"]) ?? 0, duration = Self.number(entry["duration_ms"]) ?? 0
            return entry["finished"]?.boolValue != true && position > 60_000 && (duration <= 0 || position < duration * 0.95)
        }.sorted { (Self.number($0["updated_at"]) ?? 0) > (Self.number($1["updated_at"]) ?? 0) }
        var films: [FilmsSection.Film] = []
        for entry in entries.prefix(6) where films.count < 3 {
            guard let id = entry["id"]?.nonEmptyString,
                  let item: JSONValue = try? await mediaClient.get("/v1/media/items/\(Self.segment(id))"),
                  item["kind"]?.stringValue ?? "video" == "video" else { continue }
            let updated = Self.number(entry["updated_at"]) ?? 0
            films.append(FilmsSection.Film(id: id, title: item["title"]?.nonEmptyString ?? id,
                                           positionMs: Int(Self.number(entry["position_ms"]) ?? 0),
                                           durationMs: Int(Self.number(entry["duration_ms"]) ?? Self.number(item["duration_ms"]) ?? 0),
                                           updatedAt: Date(timeIntervalSince1970: updated > 1e11 ? updated / 1000 : updated),
                                           posterPath: item["poster_url"]?.nonEmptyString ?? item["thumb_url"]?.nonEmptyString))
        }
        return FilmsSection(fetchedAt: Date(), films: films)
    }

    /// The poster of `film`, scaled down to `maxPixels` on its long side, cached by film id.
    func poster(_ film: FilmsSection.Film, maxPixels: CGFloat = 360) async -> URL? {
        guard let path = film.posterPath, let target = WidgetStore.imageURL("poster-\(film.id).jpg") else { return nil }
        if FileManager.default.fileExists(atPath: target.path) { return target }
        guard let response = try? await mediaClient.send("GET", path, body: nil, authenticated: !path.contains("/v1/media/s/")) else { return nil }
        #if canImport(UIKit)
        guard let image = UIImage(data: response.data) else { return nil }
        let scale = min(1, maxPixels / max(image.size.width, image.size.height))
        let size = CGSize(width: image.size.width * scale, height: image.size.height * scale)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        let small = UIGraphicsImageRenderer(size: size, format: format).image { _ in image.draw(in: CGRect(origin: .zero, size: size)) }
        guard let data = small.jpegData(compressionQuality: 0.8) else { return nil }
        try? data.write(to: target, options: .atomic)
        return target
        #else
        return nil
        #endif
    }

    // MARK: Helpers

    static func segment(_ id: String) -> String {
        id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? id
    }

    /// A number, or a string holding one.
    static func number(_ value: JSONValue?) -> Double? {
        switch value {
        case let .number(n)?: return n.isFinite ? n : nil
        case let .string(s)?: return Double(s.trimmingCharacters(in: .whitespaces))
        default: return nil
        }
    }

    /// ISO 8601 (with or without fractions) or epoch milliseconds.
    static func date(_ value: JSONValue?) -> Date? {
        if let ms = value?.numberValue { return Date(timeIntervalSince1970: ms > 1e11 ? ms / 1000 : ms) }
        guard let text = value?.nonEmptyString else { return nil }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: text) ?? ISO8601DateFormatter().date(from: text)
    }

    /// A Todoist due date and optional "HH:mm" time, in the phone's zone.
    static func dueTime(date: String?, time: String?) -> Date? {
        guard let date, let time else { return nil }
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd HH:mm"
        return f.date(from: "\(date) \(time.prefix(5))")
    }

    /// Lucina's Markdown, flattened for a widget line.
    static func plain(_ text: String) -> String {
        var s = text.replacingOccurrences(of: #"\[([^\]]+)\]\([^)]+\)"#, with: "$1", options: .regularExpression)
        for mark in ["**", "__", "`", "#"] { s = s.replacingOccurrences(of: mark, with: "") }
        return s.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }.joined(separator: " ")
    }
}
