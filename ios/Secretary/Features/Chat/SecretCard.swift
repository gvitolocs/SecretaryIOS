import SwiftUI

// "Enter the secret for NAME": the inline card of a request in Lucina's queue. Paste, OK, saved; the
// server then posts the next card as a new timeline item (crates/lucina-server `secret_requests`).
// No confirm step when the card names a known project and the secret doesn't exist yet: replacing an
// existing one (the broker answers `exists`) takes one more tap.
//
// The value lives in this view's @State only: a SecureField, no autocorrect or suggestions, cleared when
// the card closes or leaves the screen and when the app goes to the background. It never reaches the
// chat store, the composer, a draft, a push or DebugEvents, which logs a request's path and status only.

// MARK: - Model

/// A `secret` action's payload: names only, never a value (`held` names one Lucina holds in memory).
struct SecretAction: Codable, Equatable, Hashable, Sendable {
    var mode = "save"
    var name: String?
    var project: String?
    var environment: String?
    var path: String?
    var target: String?
    var held: String?
    var projectName: String?
    /// A request in the queue: the chat draws its inline card.
    var request: String?

    private static let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.@/-")

    static func parse(_ payload: JSONValue) -> SecretAction? {
        guard let mode = payload["mode"]?.stringValue, ["save", "project"].contains(mode) else { return nil }
        func field(_ key: String) -> String? {
            guard let v = payload[key]?.stringValue, (1...256).contains(v.count),
                  v.unicodeScalars.allSatisfy(allowed.contains) else { return nil }
            return v
        }
        let projectName = RichMessage.line(payload["projectName"], 64)
        return SecretAction(mode: mode, name: field("name"), project: field("project"), environment: field("environment"),
                            path: field("path"), target: field("target"), held: field("held"),
                            projectName: projectName.isEmpty ? nil : projectName, request: field("request"))
    }

    /// What the form shows for a card without a request (`/secret`, "Save as secret"): `target` is
    /// `project[/env[/folder]]` as the owner wrote it.
    var entry: SecretRequestEntry {
        let parts = (target ?? "").split(separator: "/", maxSplits: 2).map(String.init)
        return SecretRequestEntry(id: "", name: name ?? "", project: project ?? parts.first ?? "",
                                  environment: environment ?? (parts.count > 1 ? parts[1] : ""),
                                  path: path ?? (parts.count > 2 ? "/" + parts[2] : "/"),
                                  reason: "", who: "", status: "active", fingerprint: nil, action: nil)
    }
}

struct SecretRequestEntry: Decodable, Equatable, Sendable, Identifiable {
    let id: String
    let name: String
    let project: String
    let environment: String
    let path: String
    let reason: String
    let who: String
    /// `queued`, `active`, `saved`, `dismissed` or `expired`.
    let status: String
    let fingerprint: String?
    let action: String?
}

struct SecretQueue: Decodable, Equatable, Sendable {
    let requests: [SecretRequestEntry]
    let active: String?
    let queued: Int
}

struct SecretProject: Decodable, Equatable, Sendable, Identifiable {
    struct Environment: Decodable, Equatable, Sendable { let slug: String }
    let id: String
    let name: String
    let slug: String?
    let alias: String?
    let environments: [Environment]

    /// Lowercase words a project answers to: its alias, slug, name and the name's first word.
    var keys: [String] {
        [alias, slug, name, name.split(whereSeparator: \.isWhitespace).first.map(String.init)]
            .compactMap { $0?.lowercased() }
    }

    /// A project by id, or by alias, slug or name (any case): what a card's `project` names.
    static func find(_ ref: String, in list: [SecretProject]) -> SecretProject? {
        let key = ref.lowercased()
        return list.first { $0.id == ref } ?? list.first { $0.keys.contains(key) }
    }
}

struct SecretSaved: Decodable, Sendable {
    let key: String?
    let fingerprint: String?
    let nextItemId: String?
}

extension SecretaryAPI {
    private struct Catalog: Decodable { let projects: [SecretProject]? }
    private struct Dismissed: Decodable { let nextItemId: String? }
    private struct SaveBody: Encodable {
        let name: String, project: String, environment: String, path: String
        let overwrite: Bool, createFolder: Bool, confirmed = true
        let value: String?, held: String?, request: String?
    }
    private struct ProjectBody: Encodable { let name: String; let confirmed = true }

    func secretQueue() async throws -> SecretQueue { try await client.get("/v1/secrets/requests") }

    func dismissSecretRequest(_ id: String) async throws -> String? {
        let path = "/v1/secrets/requests/\(id)/dismiss"
        let out: Dismissed = try await client.send("POST", path, body: [String: String]())
        return out.nextItemId
    }

    func secretCatalog() async throws -> [SecretProject] {
        let c: Catalog = try await client.get("/v1/secrets/catalog")
        return (c.projects ?? []).sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// The only place the value leaves the device: this request's body, once. Never retried on a timeout.
    func saveSecret(name: String, project: String, environment: String, path: String, overwrite: Bool,
                    value: String?, held: String?, request: String?) async throws -> SecretSaved {
        let body = SaveBody(name: name, project: project, environment: environment, path: path, overwrite: overwrite,
                            createFolder: path != "/", value: value, held: held, request: request)
        return try await client.send("POST", "/v1/secrets", body: body, timeout: 60, idempotent: false)
    }

    func createSecretProject(_ name: String) async throws {
        let _: JSONValue = try await client.send("POST", "/v1/secrets/projects", body: ProjectBody(name: name), timeout: 60, idempotent: false)
    }
}

/// The queue as the cards see it.
@MainActor
@Observable
final class SecretQueueStore {
    private(set) var queue: SecretQueue?
    private(set) var failed = false
    /// The next card comes with the keyboard up, ready to paste.
    @ObservationIgnored var focusNext = false
    @ObservationIgnored private let api: () -> SecretaryAPI
    @ObservationIgnored private var running: Task<Void, Never>?

    init(api: @escaping () -> SecretaryAPI) { self.api = api }

    func refresh() async {
        if let running { return await running.value }
        let task = Task { [weak self, api] in
            do {
                let q = try await api().secretQueue()
                self?.queue = q
                self?.failed = false
            } catch {
                self?.failed = true
            }
        }
        running = task
        await task.value
        running = nil
    }

    func takeFocus() -> Bool {
        defer { focusNext = false }
        return focusNext
    }
}

private enum SecretPlace {
    static let key = "secretary.secret.last.v1"

    /// The last project / environment / folder used (names only).
    static var last: (project: String, environment: String, path: String)? {
        guard let d = UserDefaults.standard.dictionary(forKey: key) as? [String: String],
              let p = d["project"], let e = d["environment"], let f = d["path"] else { return nil }
        return (p, e, f)
    }

    static func remember(project: String, environment: String, path: String) {
        UserDefaults.standard.set(["project": project, "environment": environment, "path": path], forKey: key)
    }
}

enum SecretFolder {
    /// `/`, `/a/b`, `a/b/` -> `/a/b`; nil when a segment is not allowed.
    static func normalize(_ path: String) -> String? {
        let t = path.trimmingCharacters(in: CharacterSet.whitespaces.union(CharacterSet(charactersIn: "/")))
        if t.isEmpty { return "/" }
        let segments = t.split(separator: "/", omittingEmptySubsequences: false).map(String.init)
        let ok = segments.allSatisfy { s in
            (1...128).contains(s.count) && s != "." && s != ".." && SecretName.isValid(s)
        }
        return ok ? "/" + segments.joined(separator: "/") : nil
    }
}

enum SecretName {
    private static let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.@-")

    /// `[A-Za-z0-9_.@-]`, 1-256.
    static func isValid(_ s: String) -> Bool {
        (1...256).contains(s.count) && s.unicodeScalars.allSatisfy(allowed.contains)
    }
}

// MARK: - The card of a request

struct SecretRequestCard: View {
    @Environment(ChatStore.self) private var store
    let item: TimelineItem
    let request: String
    /// What the message itself says this request is for, when the queue doesn't know it any more.
    let fallbackName: String

    private var entry: SecretRequestEntry? { store.secretQueue.queue?.requests.first { $0.id == request } }

    var body: some View {
        content
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Theme.accent.opacity(0.28)))
            .task { await store.secretQueue.refresh() }
            // Saved or dismissed on another device: a new timeline item may change this card.
            .onChange(of: store.items.last?.id) { _, _ in Task { await store.secretQueue.refresh() } }
            .accessibilityIdentifier("secretCard")
    }

    @ViewBuilder private var content: some View {
        let queue = store.secretQueue
        if queue.queue == nil {
            if queue.failed {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Can't reach Lucina to open this request.").font(.subheadline).foregroundStyle(.secondary)
                    Button("Try again") { Task { await queue.refresh() } }.font(.subheadline.weight(.semibold))
                }
            } else {
                ProgressView()
            }
        } else if let entry, entry.status == "active" {
            SecretForm(entry: entry, held: nil, more: queue.queue?.queued ?? 0, autoFocus: { store.consumeReveal(item.id) || queue.takeFocus() },
                       onSaved: { next in await advance(to: next) }, onDismiss: { await dismiss(entry) })
        } else if let entry, entry.status == "saved" {
            let fingerprint = entry.fingerprint ?? "?"
            Label {
                Text(entry.action == "updated" ? "\(entry.name) replaced. Fingerprint \(fingerprint)." : "\(entry.name) saved. Fingerprint \(fingerprint).")
                    .font(.subheadline)
            } icon: {
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
            }
        } else if let entry, entry.status == "dismissed" {
            Text("\(entry.name.isEmpty ? String(localized: "Secret") : entry.name): not saved.")
                .font(.subheadline).foregroundStyle(.secondary)
        } else {
            Text("\(fallbackName): no longer asked.").font(.subheadline).foregroundStyle(.secondary)
        }
    }

    /// The card turned "saved" or "not saved"; the next one comes through the stream, or this catch-up.
    private func advance(to next: String?) async {
        store.secretQueue.focusNext = next != nil
        await store.secretQueue.refresh()
        if next != nil { await store.refresh() }
    }

    private func dismiss(_ entry: SecretRequestEntry) async -> String? {
        do {
            let next = try await store.secretsAPI.dismissSecretRequest(entry.id)
            await advance(to: next)
            return nil
        } catch {
            return APIError.wrap(error).userMessage
        }
    }
}

// MARK: - The form

/// Name and place when the card doesn't say them, the value, OK and "Not now". The card of a request
/// and the sheet of an older action (`/secret`, "Save as secret") share it.
struct SecretForm: View {
    @Environment(ChatStore.self) private var store
    @Environment(\.scenePhase) private var scenePhase
    let entry: SecretRequestEntry
    /// A value Lucina holds for the owner: the form sends its id and has no field.
    let held: String?
    let more: Int
    let autoFocus: () -> Bool
    let onSaved: (String?) async -> Void
    /// "Not now": a message when it failed.
    let onDismiss: () async -> String?

    private enum Phase { case idle, saving, replace }

    @State private var value = ""
    @State private var heldGone = false
    @State private var name = ""
    @State private var projects: [SecretProject]?
    @State private var projectId: String?
    @State private var env: String?
    @State private var path = "/"
    @State private var phase = Phase.idle
    @State private var error: String?
    @State private var catalogError: String?
    @State private var creating = false
    @FocusState private var focused: Bool

    private var project: SecretProject? { projects?.first { $0.id == projectId } }
    private var asksPlace: Bool { entry.project.isEmpty || entry.environment.isEmpty }
    /// The card names a project the catalog doesn't have: never save it somewhere else.
    private var missing: Bool {
        guard !entry.project.isEmpty, let projects else { return false }
        return SecretProject.find(entry.project, in: projects) == nil
    }
    private var envMissing: Bool {
        guard !entry.environment.isEmpty, let project else { return false }
        return !project.environments.contains { $0.slug == entry.environment }
    }
    private var usesHeld: Bool { held != nil && !heldGone }
    private var cleanName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }
    private var folder: String? { SecretFolder.normalize(path) }
    private var ready: Bool {
        (usesHeld || (!value.isEmpty && value.utf8.count <= 65_536)) && SecretName.isValid(cleanName)
            && projectId != nil && env != nil && folder != nil && !envMissing
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(entry.name.isEmpty ? String(localized: "Enter a secret") : String(localized: "Enter the secret for \(entry.name)"))
                .font(.headline)
                .accessibilityAddTraits(.isHeader)
            if !entry.reason.isEmpty, !entry.name.isEmpty {
                Text(verbatim: "\(entry.who): \(entry.reason)").font(.caption).foregroundStyle(.secondary)
            }
            if !asksPlace {
                Text(verbatim: "\(project?.name ?? entry.project) / \(entry.environment) / \(folder ?? path)")
                    .font(.caption.monospaced()).foregroundStyle(.secondary)
            }
            if entry.name.isEmpty {
                TextField("Name", text: $name, prompt: Text(verbatim: "OPENAI_API_KEY"))
                    .textFieldStyle(.roundedBorder)
                    .font(.body.monospaced())
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                    .onChange(of: name) { _, new in
                        let cleaned = new.replacingOccurrences(of: " ", with: "_")
                        if cleaned != new { name = cleaned }
                    }
            }
            if asksPlace { placeControls }
            if missing { missingProject }
            if let catalogError {
                VStack(alignment: .leading, spacing: 4) {
                    Text(catalogError).font(.caption).foregroundStyle(.red)
                    Button("Try again") { Task { await loadCatalog() } }.font(.caption.weight(.semibold))
                }
            }
            if envMissing {
                Text("This project has no environment \(entry.environment).").font(.caption).foregroundStyle(.red)
            }
            valueField
            if let error { Text(error).font(.caption).foregroundStyle(.red) }
            if phase == .replace {
                Label("\(cleanName) already exists there. Replace it?", systemImage: "exclamationmark.triangle.fill")
                    .font(.caption).foregroundStyle(.orange)
            }
            buttons
            if more > 0 { Text("\(more) more after this one.").font(.caption).foregroundStyle(.secondary) }
        }
        .privacySensitive()
        .task(id: entry.id) {
            name = entry.name
            env = entry.environment.isEmpty ? nil : entry.environment
            path = entry.path.isEmpty ? "/" : entry.path
            if held == nil, autoFocus() { focused = true }
            await loadCatalog()
        }
        .onDisappear { value = "" }
        .onChange(of: scenePhase) { _, phase in if phase == .background { value = "" } }
    }

    @ViewBuilder private var valueField: some View {
        if usesHeld {
            Text("Lucina holds this value for you. Her model never saw it.").font(.caption).foregroundStyle(.secondary)
        } else {
            SecureField("Paste it here", text: $value)
                .textFieldStyle(.roundedBorder)
                .textContentType(nil)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .focused($focused)
                .disabled(phase == .saving)
                .submitLabel(.done)
                .onSubmit { if phase == .idle { Task { await save(overwrite: false) } } }
                .onChange(of: value) { _, _ in if phase == .replace { phase = .idle } }
                .accessibilityLabel(Text("Value of \(cleanName.isEmpty ? String(localized: "the secret") : cleanName)"))
                .accessibilityIdentifier("secretValue")
        }
    }

    @ViewBuilder private var placeControls: some View {
        if let projects {
            VStack(alignment: .leading, spacing: 6) {
                Picker("Project", selection: Binding(get: { projectId ?? "" }, set: { pick($0) })) {
                    ForEach(projects) { Text($0.name).tag($0.id) }
                }
                .pickerStyle(.menu)
                Picker("Environment", selection: Binding(get: { env ?? "" }, set: { env = $0 })) {
                    ForEach(project?.environments ?? [], id: \.slug) { Text($0.slug).tag($0.slug) }
                }
                .pickerStyle(.menu)
                TextField("Folder", text: $path)
                    .textFieldStyle(.roundedBorder)
                    .font(.body.monospaced())
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
            }
            .font(.subheadline)
        } else if catalogError == nil {
            ProgressView()
        }
    }

    private var missingProject: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("The project \(entry.project) isn't in your Infisical yet.", systemImage: "exclamationmark.triangle.fill")
                .font(.caption).foregroundStyle(.orange)
            Button(creating ? String(localized: "Creating…") : String(localized: "Create project")) {
                Task { await createProject() }
            }
            .font(.subheadline.weight(.semibold))
            .disabled(creating)
        }
    }

    private var buttons: some View {
        HStack(spacing: 8) {
            Button("Not now") {
                value = ""
                error = nil
                Task { error = await onDismiss() }
            }
            .buttonStyle(.bordered)
            .disabled(phase == .saving)
            .accessibilityIdentifier("secretNotNow")
            if phase == .replace {
                Button("Replace", role: .destructive) { Task { await save(overwrite: true) } }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("secretReplace")
            } else {
                Button(phase == .saving ? String(localized: "Saving…") : String(localized: "OK")) {
                    Task { await save(overwrite: false) }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!ready || phase == .saving)
                .accessibilityIdentifier("secretOK")
            }
        }
    }

    private func pick(_ id: String) {
        projectId = id
        let slugs = projects?.first { $0.id == id }?.environments.map(\.slug) ?? []
        env = slugs.contains("prod") ? "prod" : slugs.first
    }

    private func loadCatalog() async {
        catalogError = nil
        do {
            let list = try await store.secretsAPI.secretCatalog()
            projects = list
            guard projectId == nil else { return }
            if !entry.project.isEmpty {
                projectId = SecretProject.find(entry.project, in: list)?.id
                if entry.environment.isEmpty, let id = projectId { pick(id) }
            } else if let last = SecretPlace.last, list.contains(where: { $0.id == last.project }) {
                projectId = last.project
                env = last.environment
                path = last.path
            } else if let first = list.first {
                pick(first.id)
            }
        } catch {
            catalogError = APIError.wrap(error).userMessage
        }
    }

    private func createProject() async {
        creating = true
        error = nil
        defer { creating = false }
        do {
            try await store.secretsAPI.createSecretProject(entry.project)
            await loadCatalog()
        } catch {
            self.error = APIError.wrap(error).userMessage
        }
    }

    private func save(overwrite: Bool) async {
        guard ready, phase != .saving, let projectId, let env, let folder else { return }
        phase = .saving
        error = nil
        do {
            let saved = try await store.secretsAPI.saveSecret(
                name: cleanName, project: projectId, environment: env, path: folder, overwrite: overwrite,
                value: usesHeld ? nil : value, held: usesHeld ? held : nil, request: entry.id.isEmpty ? nil : entry.id)
            value = ""
            phase = .idle
            SecretPlace.remember(project: projectId, environment: env, path: folder)
            await onSaved(saved.nextItemId)
        } catch {
            let api = APIError.wrap(error)
            switch api.code {
            case "exists":
                phase = .replace
            case "held_expired":
                heldGone = true
                phase = .idle
                self.error = String(localized: "Lucina no longer holds that value. Paste it here.")
            default:
                phase = .idle
                self.error = api.userMessage
            }
        }
    }
}

// MARK: - Older actions: a sheet with the same form

/// `/secret`, "Save as secret" and "Create project": a `secret` action without a request.
struct SecretActionSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(ChatStore.self) private var store
    let action: SecretAction
    let onDone: () -> Void
    @State private var error: String?
    @State private var creating = false

    var body: some View {
        NavigationStack {
            ScrollView {
                if action.mode == "project" {
                    projectForm.padding()
                } else {
                    SecretForm(entry: action.entry, held: action.held, more: 0, autoFocus: { action.held == nil },
                               onSaved: { _ in
                                   onDone()
                                   dismiss()
                               },
                               onDismiss: {
                                   dismiss()
                                   return nil
                               })
                               .padding()
                }
            }
            .navigationTitle(action.mode == "project" ? String(localized: "New Infisical project") : String(localized: "Add secret"))
            .navigationBarTitleDisplayMode(.inline)
        }
        .presentationDetents([.medium, .large])
    }

    private var projectForm: some View {
        let name = action.projectName ?? action.project ?? ""
        return VStack(alignment: .leading, spacing: 12) {
            Text("Create the Infisical project \(name)? Agents and services get no access to it until you say so.")
            if let error { Text(error).font(.caption).foregroundStyle(.red) }
            Button(creating ? String(localized: "Creating…") : String(localized: "Create project")) {
                Task { await create(name) }
            }
            .buttonStyle(.borderedProminent)
            .disabled(creating || name.isEmpty)
        }
    }

    private func create(_ name: String) async {
        creating = true
        defer { creating = false }
        do {
            try await store.secretsAPI.createSecretProject(name)
            onDone()
            dismiss()
        } catch {
            self.error = APIError.wrap(error).userMessage
        }
    }
}
