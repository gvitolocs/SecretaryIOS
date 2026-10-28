import SwiftUI

/// The alert and the confirmation of the Approvals section live on the whole Settings form (a presentation
/// attached to one row of a lazy list can disappear with it).
@Observable
@MainActor
final class ApprovalsSettingsPrompts {
    var askPassword = false
    var password = ""
    var confirmRemove = false
}

/// Settings → Approvals: which device gets the prompt first, and this iPhone's Face ID key.
struct ApprovalsSettingsSection: View {
    @Environment(AppModel.self) private var app
    let prompts: ApprovalsSettingsPrompts

    private var store: ApprovalsStore { app.approvals }

    var body: some View {
        Section {
            if store.availability == .unsupported {
                Label("Approvals aren't available on this server yet.", systemImage: "hourglass")
                    .font(.footnote).foregroundStyle(.secondary)
            }
                Toggle(isOn: Binding(
                    get: { store.settings?.thisIsMain ?? false },
                    set: { on in Task { await store.setThisDeviceMain(on) } })
                ) {
                    Label("Main device for approvals", systemImage: "iphone.badge.checkmark")
                }
                .disabled(store.settings?.thisDeviceId == nil || store.settingsBusy)
                .accessibilityIdentifier("approvalsMainDevice")

                Button {
                    prompts.password = ""
                    prompts.askPassword = true
                } label: {
                    HStack {
                        Label(store.keyStatus == .ready ? LocalizedStringKey("Set up Face ID or passcode again") : LocalizedStringKey("Use Face ID or passcode for sensitive approvals"), systemImage: "faceid")
                        Spacer()
                        if store.settingsBusy { ProgressView() }
                    }
                }
                .disabled(store.settingsBusy)
                .accessibilityIdentifier("approvalsSetUpFaceID")

                if store.keyStatus == .ready || store.keyStatus == .notRegistered {
                    Button("Remove the approval key", systemImage: "trash", role: .destructive) { prompts.confirmRemove = true }
                        .disabled(store.settingsBusy)
                }
            status
        } header: {
            Text("Approvals").task { await store.loadSettings() }
        } footer: {
            Text("When Lucina needs your OK, the main device gets the prompt first. Your other devices get it after \(store.settings?.escalateAfterSecs ?? 30) seconds. Secrets, root commands, messages and payments need Face ID or the passcode on the device that approves them.")
        }
    }

    @ViewBuilder private var status: some View {
        if let message = store.settingsMessage {
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .font(.footnote).foregroundStyle(Theme.negative)
        }
        switch store.keyStatus {
        case .ready:
            Label("Face ID or passcode is set up for approvals on this iPhone.", systemImage: "checkmark.shield.fill")
                .font(.footnote).foregroundStyle(Theme.positive)
        case .notSetUp:
            Label("Optional. Without it this iPhone can't approve secrets, root commands, messages or payments.", systemImage: "shield.slash")
                .font(.footnote).foregroundStyle(.secondary)
        case .notRegistered:
            Label("This iPhone's key isn't registered on the server. Set it up again.", systemImage: "exclamationmark.shield")
                .font(.footnote).foregroundStyle(.secondary)
        case .lostLocally:
            Label("The server has a key for this device that this iPhone no longer has. Set it up again.", systemImage: "exclamationmark.shield")
                .font(.footnote).foregroundStyle(.secondary)
        case .unavailable, .unknown:
            if !store.settingsLoaded {
                Label("Checking…", systemImage: "ellipsis").font(.footnote).foregroundStyle(.secondary)
            }
        }
    }
}

/// The alert asking for the password and the confirmation of removing the key.
struct ApprovalsSettingsPromptsModifier: ViewModifier {
    @Environment(AppModel.self) private var app
    @Bindable var prompts: ApprovalsSettingsPrompts

    func body(content: Content) -> some View {
        content
            .alert("Your Secretary password", isPresented: $prompts.askPassword) {
                SecureField("Password", text: $prompts.password)
                Button("Set up") {
                    let entered = prompts.password
                    prompts.password = ""
                    Task { await app.approvals.setUpFaceID(password: entered) }
                }
                Button("Cancel", role: .cancel) { prompts.password = "" }
            } message: {
                Text("This proves it's you. It's sent once to your server to register this iPhone's approval key, and never stored.")
            }
            .confirmationDialog("Remove the approval key?", isPresented: $prompts.confirmRemove, titleVisibility: .visible) {
                Button("Remove", role: .destructive) { Task { await app.approvals.removeFaceID() } }
            } message: {
                Text("This iPhone will no longer be able to approve secrets, root commands, messages or payments.")
            }
    }
}
