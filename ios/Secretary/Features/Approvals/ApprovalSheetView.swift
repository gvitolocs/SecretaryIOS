import SwiftUI

/// The prompt: who is asking, what for, the exact details, and Approve / Deny. It is shown by
/// `ApprovalPresenter` in a window above everything, so it appears over any tab and over any other sheet.
struct ApprovalSheetView: View {
    let store: ApprovalsStore
    /// Closes the prompt and opens Settings (Face ID is not set up).
    var openSettings: () -> Void = {}

    var body: some View {
        Group {
            if let item = store.sheetItem {
                if let outcome = store.outcome, outcome.isFinal {
                    OutcomeView(item: item, outcome: outcome, autoClose: !store.isFixture, close: close)
                } else {
                    RequestView(store: store, item: item, openSettings: openSettings, close: close)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .lucinaSheet()
    }

    private func close() { Task { await store.dismissSheet() } }
}

// MARK: - The request

private struct RequestView: View {
    let store: ApprovalsStore
    let item: ApprovalItem
    var openSettings: () -> Void
    var close: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    requester
                    Text(item.summary.isEmpty ? item.action : item.summary)
                        .font(.title3.weight(.semibold))
                        .fixedSize(horizontal: false, vertical: true)
                        .accessibilityIdentifier("approvalSummary")
                    badges
                    if !item.details.isEmpty { details }
                    if store.pending.count > 1 {
                        Text("\(store.pending.count - 1) more waiting")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                }
                .padding(.horizontal, 20)
                .padding(.top, 22)
                .padding(.bottom, 12)
            }
            Divider()
            buttons
                .padding(.horizontal, 20)
                .padding(.top, 12)
                .padding(.bottom, 16)
        }
        .task(id: item.id) {
            // Whether a PIN is set decides which buttons a sensitive request shows.
            if item.sensitive { await store.loadSettings() }
        }
    }

    private var requester: some View {
        HStack(spacing: 12) {
            Image(systemName: item.requester.symbol)
                .font(.title3)
                .foregroundStyle(Theme.accent)
                .frame(width: 40, height: 40)
                .background(Theme.accentSoft, in: Circle())
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text("\(item.requester.displayName) is asking")
                    .font(.subheadline.weight(.semibold))
                if !item.requester.detail.isEmpty {
                    Text(item.requester.detail).font(.footnote).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 8)
            ApprovalCountdown(expiresAt: item.expiresAt)
        }
    }

    private var badges: some View {
        HStack(spacing: 8) {
            Badge(text: riskText, symbol: "exclamationmark.triangle.fill", tint: riskTint)
            if item.sensitive {
                Badge(text: String(localized: "Sensitive"), symbol: "lock.fill", tint: Theme.accent)
            }
        }
    }

    private var riskText: String {
        switch item.risk {
        case .low: return String(localized: "Low risk")
        case .medium: return String(localized: "Medium risk")
        case .high: return String(localized: "High risk")
        }
    }

    private var riskTint: Color {
        switch item.risk {
        case .low: return Theme.positive
        case .medium: return Theme.warning
        case .high: return Theme.negative
        }
    }

    private var details: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Exactly what will happen")
                .font(.footnote.weight(.semibold)).foregroundStyle(.secondary)
            ScrollView([.vertical, .horizontal]) {
                Text(item.details)
                    .font(.system(.footnote, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier("approvalDetails")
            }
            .frame(maxHeight: 220)
            .background(Theme.field, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
    }

    private var hasKey: Bool { store.signer.keyId != nil }

    /// A sensitive approve with neither Face ID nor a PIN set up on this iPhone.
    private var needsSetup: Bool {
        guard item.sensitive else { return false }
        if store.outcome == .needsSetup { return true }
        return !hasKey && store.settings != nil && !store.hasPin
    }

    /// The PIN pad replaces the buttons: the owner asked for it, Face ID isn't usable, or there is no key.
    private var showsPad: Bool {
        guard item.sensitive, !needsSetup else { return false }
        return store.pinEntry || (!hasKey && store.hasPin)
    }

    @ViewBuilder private var buttons: some View {
        let working = store.working
        let expired = item.isExpired(at: Date())
        if showsPad {
            VStack(spacing: 12) {
                PinPad(title: "Enter your PIN", message: store.outcome?.pinMessage, busy: working != nil || expired) { pin in
                    Task { await store.decide(.approve, pin: pin) }
                }
                HStack {
                    if hasKey {
                        Button("Use Face ID instead") { store.showPinPad(false) }
                            .accessibilityIdentifier("approvalUseFaceID")
                    }
                    Spacer()
                    Button("Deny", role: .destructive) { Task { await store.decide(.deny) } }
                        .disabled(working != nil || expired)
                        .accessibilityIdentifier("approvalDeny")
                }
                .font(.subheadline.weight(.semibold))
            }
        } else {
            VStack(spacing: 10) {
                message
                Button {
                    Task { await store.decide(.approve) }
                } label: {
                    HStack(spacing: 8) {
                        if working == .approve {
                            ProgressView().tint(.white)
                        } else {
                            Image(systemName: item.sensitive ? "faceid" : "checkmark")
                        }
                        Text(item.sensitive ? LocalizedStringKey("Approve with Face ID") : LocalizedStringKey("Approve"))
                    }
                    .font(.headline)
                    .frame(maxWidth: .infinity, minHeight: 28)
                }
                .buttonStyle(.borderedProminent)
                .tint(Theme.accentFill)
                .disabled(working != nil || expired || needsSetup)
                .accessibilityIdentifier("approvalApprove")
                if item.sensitive, hasKey, store.hasPin {
                    Button("Use PIN instead") { store.showPinPad(true) }
                        .font(.subheadline.weight(.semibold))
                        .disabled(working != nil || expired)
                        .accessibilityIdentifier("approvalUsePIN")
                }
                Button(role: .destructive) {
                    Task { await store.decide(.deny) }
                } label: {
                    HStack(spacing: 8) {
                        if working == .deny { ProgressView() } else { Image(systemName: "xmark") }
                        Text("Deny")
                    }
                    .font(.headline)
                    .frame(maxWidth: .infinity, minHeight: 28)
                }
                .buttonStyle(.bordered)
                .disabled(working != nil || expired)
                .accessibilityIdentifier("approvalDeny")
            }
        }
    }

    @ViewBuilder private var message: some View {
        if needsSetup {
            VStack(spacing: 6) {
                Text("Set up approvals in Settings → Approvals")
                    .font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
                Button("Open Settings") { openSettings() }.font(.footnote.weight(.semibold))
                    .accessibilityIdentifier("approvalOpenSettings")
            }
        } else if case let .failed(text) = store.outcome {
            Text(text).font(.footnote).foregroundStyle(Theme.negative).multilineTextAlignment(.center)
        }
    }
}

private struct Badge: View {
    let text: String
    let symbol: String
    let tint: Color

    var body: some View {
        Label(text, systemImage: symbol)
            .font(.caption.weight(.semibold))
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .foregroundStyle(tint)
            .background(tint.opacity(0.14), in: Capsule())
    }
}

/// "4:48 left", counting down; "Expired" at zero.
private struct ApprovalCountdown: View {
    let expiresAt: Date?

    var body: some View {
        if let expiresAt {
            TimelineView(.periodic(from: .now, by: 1)) { context in
                let left = Int(expiresAt.timeIntervalSince(context.date).rounded(.down))
                if left > 0 {
                    Text(String(localized: "\(String(format: "%d:%02d", left / 60, left % 60)) left"))
                        .font(.footnote.monospacedDigit())
                        .foregroundStyle(left < 30 ? Theme.negative : .secondary)
                } else {
                    Text("Expired").font(.footnote.weight(.semibold)).foregroundStyle(Theme.negative)
                }
            }
        }
    }
}

// MARK: - The answer

private struct OutcomeView: View {
    let item: ApprovalItem
    let outcome: ApprovalOutcome
    let autoClose: Bool
    var close: () -> Void

    var body: some View {
        VStack(spacing: 16) {
            Spacer()
            Image(systemName: symbol).font(.system(size: 60)).foregroundStyle(tint)
                .accessibilityHidden(true)
            Text(title).font(.title3.weight(.semibold)).multilineTextAlignment(.center)
            if !item.summary.isEmpty {
                Text(item.summary).font(.subheadline).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
            Spacer()
            Button("Done", action: close)
                .buttonStyle(.borderedProminent)
                .tint(Theme.accentFill)
                .accessibilityIdentifier("approvalDone")
        }
        .padding(24)
        .task(id: outcome) {
            // An answer you just gave closes by itself; "already answered" stays until you read it.
            guard outcome == .approved || outcome == .denied, autoClose else { return }
            try? await Task.sleep(for: .seconds(1.4))
            if !Task.isCancelled { close() }
        }
    }

    private var symbol: String {
        switch outcome {
        case .approved: return "checkmark.circle.fill"
        case .denied: return "xmark.circle.fill"
        case .alreadyDecided: return "person.crop.circle.badge.checkmark"
        case .expired: return "clock.badge.xmark"
        default: return "questionmark.circle"
        }
    }

    private var tint: Color {
        switch outcome {
        case .approved: return Theme.positive
        case .denied: return Theme.negative
        default: return .secondary
        }
    }

    private var title: String {
        switch outcome {
        case .approved: return String(localized: "Approved")
        case .denied: return String(localized: "Denied")
        case let .alreadyDecided(status, by):
            switch (status, by) {
            case (.approved?, let by?): return String(localized: "Already approved on \(by)")
            case (.denied?, let by?): return String(localized: "Already denied on \(by)")
            case (.approved?, nil): return String(localized: "Already approved")
            case (.denied?, nil): return String(localized: "Already denied")
            case (_, let by?): return String(localized: "Already answered on \(by)")
            default: return String(localized: "Already answered")
            }
        case .expired: return String(localized: "This request expired")
        default: return String(localized: "This request is no longer there")
        }
    }
}

// MARK: - Above everything

/// Shows the prompt as a native bottom sheet in a window of its own, above every tab and every other sheet
/// (SwiftUI cannot present a second sheet from a view that already presents one).
@MainActor
final class ApprovalPresenter: NSObject, UIAdaptivePresentationControllerDelegate {
    private var window: UIWindow?
    private var host: UIViewController?
    private weak var store: ApprovalsStore?

    func sync(store: ApprovalsStore, visible: Bool, openSettings: @escaping () -> Void) {
        self.store = store
        if visible {
            guard host == nil else { return }
            present(store: store, openSettings: openSettings)
        } else {
            hide()
        }
    }

    private func present(store: ApprovalsStore, openSettings: @escaping () -> Void) {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        guard let scene = scenes.first(where: { $0.activationState == .foregroundActive }) ?? scenes.first else { return }
        let window = UIWindow(windowScene: scene)
        window.windowLevel = .alert
        window.backgroundColor = .clear
        let root = UIViewController()
        root.view.backgroundColor = .clear
        window.rootViewController = root
        window.makeKeyAndVisible()

        let sheet = UIHostingController(rootView: ApprovalSheetView(store: store, openSettings: { [weak self] in
            self?.hide()
            Task { await store.dismissSheet() }
            openSettings()
        }))
        sheet.modalPresentationStyle = .pageSheet
        if let controller = sheet.sheetPresentationController {
            controller.detents = [.medium(), .large()]
            controller.selectedDetentIdentifier = (store.sheetItem?.sensitive ?? true) ? .large : .medium
            controller.prefersGrabberVisible = true
        }
        sheet.presentationController?.delegate = self
        self.window = window
        host = sheet
        root.present(sheet, animated: true)
    }

    private func hide() {
        guard let window, let host else { return }
        self.host = nil
        host.dismiss(animated: true) { [weak self] in
            window.isHidden = true
            if self?.host == nil { self?.window = nil }
        }
    }

    /// The owner swiped the sheet away without answering.
    func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
        host = nil
        window?.isHidden = true
        window = nil
        if let store { Task { await store.dismissSheet() } }
    }
}

/// Keeps the prompt window in step with the store; attached once, to the tabs.
struct ApprovalHost: ViewModifier {
    let store: ApprovalsStore
    var openSettings: () -> Void
    @State private var presenter = ApprovalPresenter()

    func body(content: Content) -> some View {
        content
            .onChange(of: store.sheetItem?.id, initial: true) { _, id in
                presenter.sync(store: store, visible: id != nil, openSettings: openSettings)
            }
    }
}
