import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var app
    @Environment(HealthStore.self) private var health
    @State private var confirmSignOut = false
    @State private var showPassword = false
    @State private var developerTaps = 0
    @AppStorage("secretary.debug.showDeveloper") private var showDeveloper = false
    @State private var serverDraft = ""
    @State private var serverError: String?

    var body: some View {
        Form {
            Section("Server") {
                LabeledContent("Status") {
                    HStack(spacing: 6) {
                        StatusDot().padding(-8)
                        Text(health.summary).foregroundStyle(.secondary)
                    }
                }
                .accessibilityElement(children: .combine)
                LabeledContent("Address", value: app.baseURL.host ?? app.baseURL.absoluteString)
                if let version = health.version { LabeledContent("Server version", value: version) }
                Button("Check again", systemImage: "arrow.clockwise") { Task { await health.refresh() } }
            }

            Section("This device") {
                LabeledContent("Device name", value: app.deviceName ?? "—")
                Button("Change password", systemImage: "key") { showPassword = true }
                Button("Sign out", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) { confirmSignOut = true }
                    .accessibilityIdentifier("signOut")
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

    @ViewBuilder private var developerSection: some View {
        Section {
            if app.canOverrideServer {
                TextField("https://… or http://localhost:8791", text: $serverDraft)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                Button("Use this server") {
                    serverError = app.setServerOverride(serverDraft) ? nil : "Use https://, or http:// for a local host."
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
                    SecureField("New password", text: $new).textContentType(.newPassword)
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
        guard new == confirm else { message = "The new passwords don't match."; return }
        busy = true
        defer { busy = false }
        do {
            try await app.changePassword(current: current, new: new)
            done = true
            message = "Password changed. Other devices keep their sign-in."
            current = ""; new = ""; confirm = ""
        } catch {
            let apiError = APIError.wrap(error)
            switch (apiError.status, apiError.code) {
            case (_, "invalid_credentials"): message = "The current password is wrong."
            case (404, _): message = "This server doesn't support changing the password yet."
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
