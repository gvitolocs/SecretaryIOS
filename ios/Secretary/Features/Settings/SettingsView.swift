import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var app
    @Environment(HealthStore.self) private var health
    @AppStorage("share.lastResultMessage", store: UserDefaults(suiteName: ShareRequestBuilder.group)) private var lastShareResult = ""
    @State private var confirmSignOut = false
    @State private var showPassword = false
    @State private var developerTaps = 0
    @AppStorage("secretary.debug.showDeveloper") private var showDeveloper = false
    @AppStorage(LucinaVoiceReplies.settingKey) private var voiceReplies = true
    @State private var serverDraft = ""
    @State private var serverError: String?
    @State private var languageChanged = false
    @State private var testing = false
    @State private var probes: [APIClient.Probe] = []

    var body: some View {
        Form {
            Section("Share to Lucina") {
                Label("Share to Lucina", systemImage: "square.and.arrow.up")
                if !lastShareResult.isEmpty { Text(lastShareResult).font(.caption).foregroundStyle(.secondary) }
                Text("Lucina is in the share sheet. To keep her first: Share → scroll the app row to the end → More → Edit → add Lucina to Favorites and drag her to the top. iOS orders the rest by how often you use them.")
                    .font(.footnote).foregroundStyle(.secondary)
            }

            Section {
                Toggle(isOn: $voiceReplies) {
                    Label("Lucina answers voice notes with her voice", systemImage: "speaker.wave.2")
                }
                .accessibilityIdentifier("voiceRepliesToggle")
            } header: {
                Text("Voice notes")
            } footer: {
                Text("Hold the mic in the chat to send Lucina a voice note. When she answers one, she reads the answer aloud while Secretary is open; tap the speaker under it to hear it again.")
            }

            Section {
                NavigationLink {
                    LinksView()
                } label: {
                    Label {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Links")
                            Text("Calendars, chat channels, AI clients and your own links")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    } icon: {
                        Image(systemName: "link")
                    }
                }
                .accessibilityIdentifier("openLinks")
            }

            Section("Server") {
                LabeledContent("Status") {
                    HStack(spacing: 6) {
                        StatusDot().padding(-8)
                        Text(health.summary).foregroundStyle(.secondary)
                    }
                }
                .accessibilityElement(children: .combine)
                LabeledContent("Connection", value: app.endpointLabel)
                    .accessibilityIdentifier("activeEndpoint")
                LabeledContent("Address", value: app.baseURL.host ?? app.baseURL.absoluteString)
                if let version = health.version { LabeledContent("Server version", value: version) }
                Button {
                    Task { await runConnectionTest() }
                } label: {
                    HStack {
                        Label("Test connection", systemImage: "antenna.radiowaves.left.and.right")
                        Spacer()
                        if testing { ProgressView() }
                    }
                }
                .disabled(testing)
                .accessibilityIdentifier("testConnection")
                ForEach(probes, id: \.url) { probe in
                    HStack(alignment: .firstTextBaseline) {
                        Image(systemName: probe.ok ? "checkmark.circle.fill" : "xmark.circle.fill")
                            .foregroundStyle(probe.ok ? .green : .red)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(ServerConfig.label(for: probe.url)).font(.subheadline.weight(.semibold))
                            Text(probe.url.host ?? probe.url.absoluteString).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Text(probe.latencyMs.map { "\($0) ms" } ?? (probe.error ?? "Failed"))
                            .font(.caption)
                            .foregroundStyle(probe.ok ? Color.secondary : Color.red)
                            .multilineTextAlignment(.trailing)
                    }
                    .accessibilityElement(children: .combine)
                }
            }

            Section {
                NavigationLink {
                    NotificationSettingsScreen()
                } label: {
                    Label("Notifications", systemImage: "bell.badge")
                }
                .accessibilityIdentifier("notificationSettings")
            }

            Section("This device") {
                LabeledContent("Device name", value: app.deviceName ?? "—")
                Button("Change password", systemImage: "key") { showPassword = true }
                Button("Sign out", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) { confirmSignOut = true }
                    .accessibilityIdentifier("signOut")
            }

            Section {
                Picker("Language", selection: Binding(get: { AppLanguage.current }, set: { AppLanguage.set($0); languageChanged = true })) {
                    ForEach(AppLanguage.allCases) { Text($0.title).tag($0) }
                }
                .accessibilityIdentifier("appLanguage")
            } header: {
                Text("Language")
            } footer: {
                Text(languageChanged ? "Close and reopen Secretary to switch the language." : "Follows the iPhone's language unless you pick one. Lucina answers in the language you write in.")
            }

            Section {
                NavigationLink {
                    AccessView()
                } label: {
                    Label("Lucina's access", systemImage: "hand.raised")
                }
                .accessibilityIdentifier("lucinaAccess")
            } footer: {
                Text("Alarms, reminders, calendar, Health, places, Focus, Safari and Screen Time: choose what Lucina may see and do.")
            }

            Section {
                Toggle("Show Lucina", isOn: Binding(get: { app.lucina.enabled }, set: { app.lucina.enabled = $0 }))
                    .accessibilityIdentifier("lucinaEnabled")
                Toggle("Voice replies", isOn: Binding(get: { !app.lucina.muted }, set: { app.lucina.muted = !$0 }))
                    .accessibilityIdentifier("lucinaVoice")
                Button {
                    app.wardrobe.presented = true
                } label: {
                    HStack {
                        Label("Lucina's wardrobe", systemImage: "hanger")
                        Spacer()
                        Text(app.wardrobe.current?.name ?? "").foregroundStyle(.secondary)
                        Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(.tertiary)
                    }
                }
                .foregroundStyle(.primary)
                .accessibilityIdentifier("openWardrobe")
                .accessibilityIdentifier("openWardrobe")
            } header: {
                Text("Lucina")
            } footer: {
                Text("Lucina floats over every tab (a column on iPad and in landscape). Her voice is off until you turn it on; she only speaks to answer you.")
            }

            Section("About") {
                LabeledContent("Version", value: Bundle.main.versionLabel)
                    .contentShape(Rectangle())
                    .onTapGesture {
                        guard ServerConfig.isDebugBuild else { return }
                        developerTaps += 1
                        if developerTaps >= 5 { showDeveloper = true }
                    }
                Text("Secretary is the owner's personal assistant. Lucina keeps the long-term memory; gym and weight data stay on your own server.")
                    .font(.footnote).foregroundStyle(.secondary)
            }

            if ServerConfig.isDebugBuild && showDeveloper {
                developerSection
            }
        }
        .navigationTitle("Settings")
        .confirmationDialog("Sign out of Secretary on this iPhone?", isPresented: $confirmSignOut, titleVisibility: .visible) {
            Button("Sign out", role: .destructive) { app.signOut() }
        } message: {
            Text("The device token is deleted from this iPhone. The local chat history is cleared too.")
        }
        .sheet(isPresented: $showPassword) { ChangePasswordView() }
    }

    private func runConnectionTest() async {
        testing = true
        defer { testing = false }
        probes = await app.testConnection()
        await health.refresh()
    }

    @ViewBuilder private var developerSection: some View {
        Section {
            if app.canOverrideServer {
                TextField("https://… or http://localhost:8791", text: $serverDraft)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                Button("Use this server") {
                    serverError = app.setServerOverride(serverDraft) ? nil : String(localized: "Use https://, or http:// for a local host.")
                }
                Button("Back to \(ServerConfig.production.host ?? "production")") { _ = app.setServerOverride(nil) }
                if let serverError { Text(serverError).font(.footnote).foregroundStyle(.red) }
            } else {
                Text("The server is set by the -SecretaryBaseURL launch argument.").font(.footnote).foregroundStyle(.secondary)
            }
            Button("Hide developer options") { showDeveloper = false; developerTaps = 0 }
        } header: {
            Text("Developer")
        } footer: {
            Text("Debug builds only. Changing the server signs you out.")
        }
    }
}

private struct ChangePasswordView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var current = ""
    @State private var new = ""
    @State private var confirm = ""
    @State private var busy = false
    @State private var message: String?
    @State private var done = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    SecureField("Current password", text: $current).textContentType(.password)
                    SecureField("New password (8+ characters)", text: $new).textContentType(.newPassword)
                    SecureField("Repeat new password", text: $confirm).textContentType(.newPassword)
                }
                if let message {
                    Text(message).font(.footnote).foregroundStyle(done ? Color.secondary : Color.red)
                }
            }
            .navigationTitle("Change password")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button(done ? "Done" : "Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { Task { await save() } }
                        .disabled(busy || current.isEmpty || new.isEmpty || done)
                }
            }
        }
        .presentationDetents([.medium])
    }

    private func save() async {
        guard new == confirm else { message = String(localized: "The new passwords don't match."); return }
        guard new.count >= 8 else { message = String(localized: "Use at least 8 characters."); return }
        busy = true
        defer { busy = false }
        do {
            try await app.changePassword(current: current, new: new)
            done = true
            message = String(localized: "Password changed. Other devices keep their sign-in.")
            current = ""; new = ""; confirm = ""
        } catch {
            let apiError = APIError.wrap(error)
            switch (apiError.status, apiError.code) {
            case (_, "invalid_credentials"): message = String(localized: "The current password is wrong.")
            case (404, _): message = String(localized: "This server doesn't support changing the password yet.")
            default:
                if apiError.endsSession { app.handle(apiError) }
                message = apiError.userMessage
            }
        }
    }
}

extension Bundle {
    var versionLabel: String {
        let version = infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let build = infoDictionary?["CFBundleVersion"] as? String ?? "?"
        return "\(version) (\(build))"
    }
}

/// The app's language: the iPhone's (Italian, English or Danish), or an override.
/// iOS applies `AppleLanguages` for the app on the next launch.
enum AppLanguage: String, CaseIterable, Identifiable {
    case system, en, it, da

    var id: String { rawValue }

    var title: LocalizedStringKey {
        switch self {
        case .system: return "Same as iPhone"
        case .en: return "English"
        case .it: return "Italiano"
        case .da: return "Dansk"
        }
    }

    private static let key = "AppleLanguages"
    private static let overrideKey = "secretary.languageOverride"

    static var current: AppLanguage {
        AppLanguage(rawValue: UserDefaults.standard.string(forKey: overrideKey) ?? "") ?? .system
    }

    static func set(_ language: AppLanguage) {
        let defaults = UserDefaults.standard
        if language == .system {
            defaults.removeObject(forKey: key)
            defaults.removeObject(forKey: overrideKey)
        } else {
            defaults.set([language.rawValue], forKey: key)
            defaults.set(language.rawValue, forKey: overrideKey)
        }
    }
}
