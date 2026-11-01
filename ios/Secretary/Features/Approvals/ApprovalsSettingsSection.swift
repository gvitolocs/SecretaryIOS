import SwiftUI

enum PinFlowKind: String, Identifiable {
    case choose, change, remove
    var id: String { rawValue }
}

/// The sheets and confirmations of the Approvals section live on the whole Settings form (a presentation
/// attached to one row of a lazy list can disappear with it).
@Observable
@MainActor
final class ApprovalsSettingsPrompts {
    var confirmRemoveKey = false
    var confirmRemovePin = false
    var pinFlow: PinFlowKind?
}

/// Settings → Approvals: which device gets the prompt first, this iPhone's Face ID key, and the optional PIN.
struct ApprovalsSettingsSection: View {
    @Environment(AppModel.self) private var app
    let prompts: ApprovalsSettingsPrompts

    private var store: ApprovalsStore { app.approvals }
    private var faceIDOn: Bool { store.keyStatus == .ready }

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

            Label(faceIDOn ? "Face ID or passcode: on" : "Face ID or passcode: off", systemImage: "faceid")
                .foregroundStyle(faceIDOn ? Theme.positive : Color.primary)
                .accessibilityIdentifier("approvalsFaceIDState")
            Button {
                Task {
                    let ok = await store.setUpFaceID()
                    // Face ID is the whole setup. A PIN is the optional fallback.
                    if ok, !store.hasPin { prompts.pinFlow = .choose }
                }
            } label: {
                HStack {
                    Text(faceIDOn ? "Set up Face ID again" : "Set up")
                    Spacer()
                    if store.settingsBusy { ProgressView() }
                }
            }
            .disabled(store.settingsBusy || store.availability == .unsupported)
            .accessibilityIdentifier("approvalsSetUpFaceID")
            if store.keyStatus == .ready || store.keyStatus == .notRegistered {
                Button("Remove Face ID", systemImage: "trash", role: .destructive) { prompts.confirmRemoveKey = true }
                    .disabled(store.settingsBusy)
            }

            Label(store.hasPin ? "PIN: set" : "PIN: not set", systemImage: "number.square")
                .foregroundStyle(store.hasPin ? Theme.positive : Color.primary)
                .accessibilityIdentifier("approvalsPinState")
            if store.hasPin {
                Button("Change PIN") { prompts.pinFlow = .change }
                    .disabled(store.settingsBusy)
                    .accessibilityIdentifier("approvalsChangePIN")
                Button("Remove PIN", role: .destructive) {
                    // With a key, Face ID proves it's the owner; without one the current PIN does.
                    if store.signer.keyId != nil { prompts.confirmRemovePin = true } else { prompts.pinFlow = .remove }
                }
                .disabled(store.settingsBusy)
                .accessibilityIdentifier("approvalsRemovePIN")
            } else if store.settingsLoaded, store.availability != .unsupported {
                Button("Choose a PIN (optional)") { prompts.pinFlow = .choose }
                    .disabled(store.settingsBusy)
                    .accessibilityIdentifier("approvalsChoosePIN")
            }
            status
        } header: {
            Text("Approvals").task { await store.loadSettings() }
        } footer: {
            Text("When Lucina needs your OK, the main device gets the prompt first. Your other devices get it after \(store.settings?.escalateAfterSecs ?? 30) seconds. Secrets, root commands, messages and payments need Face ID or the passcode, or your PIN, on the device that approves them.")
        }
    }

    @ViewBuilder private var status: some View {
        if let message = store.settingsMessage {
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .font(.footnote).foregroundStyle(Theme.negative)
        }
        switch store.keyStatus {
        case .notRegistered:
            Label("This iPhone's key isn't registered on the server. Set it up again.", systemImage: "exclamationmark.shield")
                .font(.footnote).foregroundStyle(.secondary)
        case .lostLocally:
            Label("The server has a key for this device that this iPhone no longer has. Set it up again.", systemImage: "exclamationmark.shield")
                .font(.footnote).foregroundStyle(.secondary)
        case .unavailable, .unknown:
            if !store.settingsLoaded {
                Label { Text(verbatim: "Checking…") } icon: { Image(systemName: "ellipsis") }
                    .font(.footnote).foregroundStyle(.secondary)
            }
        case .notSetUp, .ready:
            EmptyView()
        }
    }
}

/// The confirmations and the PIN sheet of the Approvals section.
struct ApprovalsSettingsPromptsModifier: ViewModifier {
    @Environment(AppModel.self) private var app
    @Bindable var prompts: ApprovalsSettingsPrompts

    func body(content: Content) -> some View {
        content
            .confirmationDialog("Remove Face ID for approvals?", isPresented: $prompts.confirmRemoveKey, titleVisibility: .visible) {
                Button("Remove", role: .destructive) { Task { await app.approvals.removeFaceID() } }
            } message: {
                Text("This iPhone will approve secrets, root commands, messages and payments only with your PIN, if you set one.")
            }
            .confirmationDialog("Remove the PIN?", isPresented: $prompts.confirmRemovePin, titleVisibility: .visible) {
                Button("Remove PIN", role: .destructive) {
                    Task {
                        if case let .refused(text, _) = await app.approvals.removePIN() { app.approvals.settingsMessage = text }
                    }
                }
            } message: {
                Text("Face ID or your passcode will confirm it.")
            }
            .sheet(item: $prompts.pinFlow) { kind in
                PinFlowSheet(kind: kind, store: app.approvals)
            }
    }
}

/// Choose, change or remove the 6-digit PIN. Changing or removing needs the current PIN, unless this iPhone has a
/// Face ID key: that signs the change instead.
struct PinFlowSheet: View {
    let kind: PinFlowKind
    let store: ApprovalsStore
    @Environment(\.dismiss) private var dismiss

    private enum Step { case current, new, confirm }

    @State private var step: Step
    @State private var current: String?
    @State private var newPin: String?
    @State private var message: String?
    @State private var busy = false

    init(kind: PinFlowKind, store: ApprovalsStore) {
        self.kind = kind
        self.store = store
        _step = State(initialValue: kind != .choose && store.signer.keyId == nil ? .current : .new)
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 18) {
                Text(explanation)
                    .font(.subheadline).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    .padding(.horizontal, 24)
                PinPad(title: title, message: message, busy: busy) { entered($0) }
                    .id(step)
                Spacer()
            }
            .padding(.top, 24)
            .navigationTitle(kind == .remove ? "Remove PIN" : (kind == .change ? "Change PIN" : "Choose a PIN"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(kind == .choose ? "Skip" : "Cancel") { dismiss() }
                }
            }
        }
        .presentationDetents([.large])
        .interactiveDismissDisabled(busy)
    }

    private var title: String {
        switch step {
        case .current: return "Enter your current PIN"
        case .new: return kind == .choose ? "Choose a 6-digit PIN" : "Enter a new PIN"
        case .confirm: return "Enter it again"
        }
    }

    private var explanation: String {
        switch kind {
        case .choose: return "Optional. Your PIN approves sensitive requests when Face ID isn't available."
        case .change: return "Your PIN approves sensitive requests when Face ID isn't available."
        case .remove: return "Without a PIN, sensitive requests need Face ID or your passcode."
        }
    }

    private func entered(_ pin: String) {
        message = nil
        switch step {
        case .current:
            if kind == .remove { submit(new: nil, current: pin) } else { current = pin; step = .new }
        case .new:
            newPin = pin
            step = .confirm
        case .confirm:
            if pin == newPin {
                submit(new: pin, current: current)
            } else {
                message = "The PINs don't match. Try again."
                newPin = nil
                step = .new
            }
        }
    }

    private func submit(new: String?, current: String?) {
        busy = true
        Task {
            let result: ApprovalsStore.PinChange
            if let new { result = await store.setPIN(new, currentPin: current) } else { result = await store.removePIN(currentPin: current) }
            busy = false
            switch result {
            case .done:
                dismiss()
            case .cancelled:
                newPin = nil
                step = kind == .remove ? .current : .new
            case let .refused(text, currentWasWrong):
                message = text
                newPin = nil
                self.current = nil
                _ = currentWasWrong
                step = kind != .choose && store.signer.keyId == nil ? .current : .new
            }
        }
    }
}
