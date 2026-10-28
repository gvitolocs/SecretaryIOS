import Foundation
import SwiftUI
import UserNotifications

/// A go-ahead an agent asked Lucina for (`/v1/decisions`, docs/design/decisions-api.md). When she asks the owner,
/// her push opens it here (`/decisions/<id>`), and long-pressing the notification shows one button per option.
/// The owner answers by sending `Decision <id>: <n>. <option>` to Lucina, the same text her chat buttons send.
struct DecisionItem: Decodable, Identifiable, Equatable, Sendable {
    struct Evidence: Decodable, Equatable, Sendable {
        let kind: String
        let text: String
        let tier: Int?
    }

    let id: String
    let status: String
    let decision: String?
    let chosenOption: String?
    let chosenIndex: Int?
    let reason: String
    let decidedBy: String?
    let question: String
    let options: [String]
    let recommendation: String?
    let impact: String
    let reversible: Bool
    let urgency: String
    let askingClient: String
    let askingSession: String?
    let createdAt: String
    let evidence: [Evidence]

    var isPending: Bool { status == "pending" }
    var asker: String { askingSession.map { "\(askingClient) (\($0))" } ?? askingClient }

    private enum CodingKeys: String, CodingKey {
        case id = "decision_id", status, decision, chosenOption = "chosen_option", chosenIndex = "chosen_index", reason
        case decidedBy = "decided_by", question, options, recommendation, impact, reversible, urgency
        case askingClient = "asking_client", askingSession = "asking_session", createdAt = "created_at", evidence
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        status = (try? c.decodeIfPresent(String.self, forKey: .status)) ?? "pending"
        decision = try? c.decodeIfPresent(String.self, forKey: .decision)
        chosenOption = try? c.decodeIfPresent(String.self, forKey: .chosenOption)
        chosenIndex = try? c.decodeIfPresent(Int.self, forKey: .chosenIndex)
        reason = (try? c.decodeIfPresent(String.self, forKey: .reason)) ?? ""
        decidedBy = try? c.decodeIfPresent(String.self, forKey: .decidedBy)
        question = (try? c.decodeIfPresent(String.self, forKey: .question)) ?? ""
        options = (try? c.decodeIfPresent([String].self, forKey: .options)) ?? []
        recommendation = try? c.decodeIfPresent(String.self, forKey: .recommendation)
        impact = (try? c.decodeIfPresent(String.self, forKey: .impact)) ?? "medium"
        reversible = (try? c.decodeIfPresent(Bool.self, forKey: .reversible)) ?? true
        urgency = (try? c.decodeIfPresent(String.self, forKey: .urgency)) ?? "normal"
        askingClient = (try? c.decodeIfPresent(String.self, forKey: .askingClient)) ?? "an agent"
        askingSession = try? c.decodeIfPresent(String.self, forKey: .askingSession)
        createdAt = (try? c.decodeIfPresent(String.self, forKey: .createdAt)) ?? ""
        evidence = (try? c.decodeIfPresent(LossyArray<Evidence>.self, forKey: .evidence))?.values ?? []
    }
}

private struct DecisionList: Decodable { let items: LossyArray<DecisionItem> }

/// The answer texts Lucina understands (her `decide::owner_answer`).
enum DecisionAnswer {
    static func option(_ id: String, index: Int, option: String) -> String { "Decision \(id): \(index + 1). \(option)" }
    static func words(_ id: String, _ text: String) -> String { "Decision \(id): \(text)" }

    /// `dc-` + 12 lowercase hex.
    static func isId(_ s: String) -> Bool {
        s.count == 15 && s.hasPrefix("dc-") && s.dropFirst(3).allSatisfy { ("0"..."9").contains($0) || ("a"..."f").contains($0) }
    }
}

struct DecisionsAPI: Sendable {
    let client: APIClient

    func pending() async throws -> [DecisionItem] {
        let list: DecisionList = try await client.get("/v1/decisions", query: [URLQueryItem(name: "status", value: "pending")])
        return list.items.values
    }

    func get(_ id: String) async throws -> DecisionItem {
        guard DecisionAnswer.isId(id) else { throw APIError.wrap(URLError(.badURL)) }
        return try await client.get("/v1/decisions/\(id)")
    }
}

/// Pending decisions and the one on screen.
@MainActor
@Observable
final class DecisionsStore {
    /// The sheet: "" = the pending list, an id = that decision.
    var openId: String?
    private(set) var current: DecisionItem?
    private(set) var pending: [DecisionItem] = []
    private(set) var sending = false
    var errorMessage: String?
    /// Lucina's reply to the answer ("Ok, … gets …").
    private(set) var confirmation: String?

    @ObservationIgnored private let api: () -> DecisionsAPI
    @ObservationIgnored private let send: (String) async throws -> String

    /// `send`: delivers a chat message as the owner and returns Lucina's reply.
    init(api: @escaping () -> DecisionsAPI, send: @escaping (String) async throws -> String) {
        self.api = api
        self.send = send
    }

    var presented: Bool {
        get { openId != nil }
        set { if !newValue { openId = nil; current = nil; confirmation = nil; errorMessage = nil } }
    }

    func open(_ id: String?) {
        confirmation = nil
        errorMessage = nil
        current = nil
        openId = id ?? ""
        Task { await load() }
    }

    func load() async {
        do {
            if let id = openId, !id.isEmpty {
                current = try await api().get(id)
            }
            pending = try await api().pending()
        } catch {
            errorMessage = APIError.wrap(error).userMessage
        }
    }

    func lookup(_ id: String) async -> DecisionItem? { try? await api().get(id) }

    /// Answers with option `index` (0-based) or the owner's own words.
    func answer(id: String, index: Int? = nil, option: String? = nil, words: String? = nil) async {
        let text: String
        if let index, let option {
            text = DecisionAnswer.option(id, index: index, option: option)
        } else if let words, !words.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            text = DecisionAnswer.words(id, words.trimmingCharacters(in: .whitespacesAndNewlines))
        } else {
            return
        }
        sending = true
        defer { sending = false }
        do {
            confirmation = try await send(text)
            await DecisionNotifier.remove(id: id)
            if openId == id || openId == nil { current = try? await api().get(id) }
            pending = (try? await api().pending()) ?? pending
        } catch {
            errorMessage = APIError.wrap(error).userMessage
        }
    }
}

/// Local notifications for decisions: category `DECISION.<id>` with one action per option (up to four) and a
/// text answer, registered next to the approval categories before the notification is posted.
enum DecisionNotifier {
    static let prefix = "DECISION."
    static let optionPrefix = "DECISION_OPTION_"
    static let wordsAction = "DECISION_WORDS"
    /// Keys in `userInfo`.
    static let idKey = "decisionId"
    static let optionsKey = "decisionOptions"

    static func category(_ id: String) -> String { prefix + id }

    static func register(id: String, options: [String], center: UNUserNotificationCenter = .current()) async {
        var actions: [UNNotificationAction] = options.prefix(4).enumerated().map { i, o in
            let title = o.count > 40 ? String(o.prefix(39)) + "…" : o
            return UNNotificationAction(identifier: optionPrefix + String(i), title: "\(i + 1). \(title)", options: [.authenticationRequired])
        }
        actions.append(UNTextInputNotificationAction(identifier: wordsAction, title: String(localized: "Answer in my words"),
                                                     options: [.authenticationRequired],
                                                     textInputButtonTitle: String(localized: "Send"),
                                                     textInputPlaceholder: String(localized: "Your answer to Lucina")))
        let mine = UNNotificationCategory(identifier: category(id), actions: actions, intentIdentifiers: [], options: [])
        let existing = await center.notificationCategories()
        // Keep the approval categories and the latest decisions (a category per pending decision).
        let others = existing.filter { $0.identifier != mine.identifier }
        let decisions = others.filter { $0.identifier.hasPrefix(prefix) }.suffix(15)
        let rest = others.filter { !$0.identifier.hasPrefix(prefix) }
        center.setNotificationCategories(Set(rest).union(decisions).union([mine]))
    }

    /// The notification (identifier `decision.<id>`): tap opens the decision, long-press shows the options.
    static func post(id: String, title: String, body: String, options: [String], previews: Bool,
                     center: UNUserNotificationCenter = .current()) async {
        await register(id: id, options: options, center: center)
        let c = UNMutableNotificationContent()
        c.title = title
        c.body = previews ? body : String(localized: "Lucina needs your decision. Open Secretary to see it.")
        c.sound = .default
        c.threadIdentifier = "decision"
        c.categoryIdentifier = category(id)
        c.userInfo = [idKey: id, optionsKey: Array(options.prefix(6)), LocalNotifier.routeKey: "/decisions/\(id)"]
        try? await center.add(UNNotificationRequest(identifier: "decision.\(id)", content: c, trigger: nil))
    }

    static func remove(id: String, center: UNUserNotificationCenter = .current()) async {
        center.removeDeliveredNotifications(withIdentifiers: ["decision.\(id)"])
    }

    /// What a notification response asks for: open, an option (0-based), or the owner's own words.
    enum Response: Equatable {
        case open, option(Int), words(String)
    }

    static func response(actionIdentifier: String, userText: String?) -> Response? {
        if actionIdentifier == UNNotificationDefaultActionIdentifier { return .open }
        if actionIdentifier == wordsAction { return userText.map { .words($0) } }
        if actionIdentifier.hasPrefix(optionPrefix), let i = Int(actionIdentifier.dropFirst(optionPrefix.count)) { return .option(i) }
        return nil
    }
}

// MARK: - Views

struct DecisionsSheet: View {
    @Environment(AppModel.self) private var app
    @State private var words = ""

    var body: some View {
        let store = app.decisions
        NavigationStack {
            Group {
                if let d = store.current {
                    DecisionDetail(decision: d, words: $words)
                } else if store.openId?.isEmpty == false && store.errorMessage == nil {
                    ProgressView()
                } else {
                    PendingDecisionsList(items: store.pending) { store.open($0.id) }
                }
            }
            .navigationTitle(store.current == nil ? String(localized: "Decisions") : String(localized: "Decision"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { store.presented = false }
                }
                if store.current != nil {
                    ToolbarItem(placement: .navigation) {
                        Button(String(localized: "All")) { store.open(nil) }
                    }
                }
            }
            .safeAreaInset(edge: .bottom) {
                if let e = store.errorMessage {
                    Text(e).font(.footnote).foregroundStyle(.red).padding(8)
                }
            }
        }
        .lucinaSheet()
    }
}

private struct PendingDecisionsList: View {
    let items: [DecisionItem]
    let open: (DecisionItem) -> Void

    var body: some View {
        List {
            if items.isEmpty {
                Text("Nothing is waiting for you.").foregroundStyle(.secondary)
            }
            ForEach(items) { d in
                Button { open(d) } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(d.question).font(.body).lineLimit(3)
                        Text("\(d.asker) · \(d.impact)").font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
    }
}

private struct DecisionDetail: View {
    @Environment(AppModel.self) private var app
    let decision: DecisionItem
    @Binding var words: String

    var body: some View {
        let store = app.decisions
        Form {
            Section {
                Text(decision.question).font(.headline)
                Text(String(localized: "Asked by \(decision.asker)")).font(.caption).foregroundStyle(.secondary)
                HStack(spacing: 8) {
                    Label(decision.impact, systemImage: "gauge.medium")
                    Label(decision.reversible ? String(localized: "reversible") : String(localized: "can't be undone"),
                          systemImage: decision.reversible ? "arrow.uturn.backward" : "exclamationmark.triangle")
                    if decision.urgency == "high" { Label(String(localized: "urgent"), systemImage: "bolt") }
                }
                .font(.caption)
            }
            if !decision.reason.isEmpty {
                Section(String(localized: "Lucina")) { Text(decision.reason).font(.callout) }
            }
            if decision.isPending {
                Section(String(localized: "Your answer")) {
                    ForEach(Array(decision.options.enumerated()), id: \.offset) { i, o in
                        Button {
                            Task { await store.answer(id: decision.id, index: i, option: o) }
                        } label: {
                            HStack {
                                Text("\(i + 1). \(o)").multilineTextAlignment(.leading)
                                Spacer()
                                if decision.recommendation == o {
                                    Text("recommended").font(.caption2).foregroundStyle(.secondary)
                                }
                            }
                        }
                        .disabled(store.sending)
                    }
                    TextField(String(localized: "Or answer in your own words"), text: $words, axis: .vertical)
                    Button(String(localized: "Send my answer")) {
                        Task {
                            await store.answer(id: decision.id, words: words)
                            words = ""
                        }
                    }
                    .disabled(store.sending || words.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            } else {
                Section(String(localized: "Outcome")) {
                    Text(outcome).font(.callout)
                }
            }
            if let c = store.confirmation {
                Section { Text(c).font(.callout) }
            }
            if !decision.evidence.isEmpty {
                Section(String(localized: "What Lucina looked at")) {
                    ForEach(Array(decision.evidence.enumerated()), id: \.offset) { _, e in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(tierName(e.tier)).font(.caption2).foregroundStyle(.secondary)
                            Text(e.text).font(.caption)
                        }
                    }
                }
            }
        }
    }

    private var outcome: String {
        switch decision.status {
        case "decided":
            let who = decision.decidedBy == "owner" ? String(localized: "You") : "Lucina"
            return "\(who): \(decision.chosenOption ?? decision.reason)"
        case "withdrawn": return String(localized: "Withdrawn by the asker.")
        case "expired": return String(localized: "Expired without an answer.")
        default: return decision.status
        }
    }

    private func tierName(_ tier: Int?) -> String {
        switch tier {
        case 1: return String(localized: "your words")
        case 2: return String(localized: "your actions")
        case 3: return String(localized: "Lucina's earlier decision")
        case 4: return String(localized: "agent record")
        default: return ""
        }
    }
}
