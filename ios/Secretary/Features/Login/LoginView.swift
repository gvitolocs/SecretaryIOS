import SwiftUI

/// Sign-in for the one owner: email + password (`POST /v1/auth/device`), or a `sec_…`
/// device key as a fallback (servers without password sign-in, or a key issued with
/// `secretary-server token issue`).
struct LoginView: View {
    @Environment(AppModel.self) private var app

    enum Mode: String, CaseIterable, Identifiable {
        case password = "Email & password"
        case accessKey = "Access key"
        var id: String { rawValue }
    }

    @State private var mode: Mode = .password
    @State private var email = ""
    @State private var password = ""
    @State private var accessKey = ""
    @State private var busy = false
    @State private var error: String?
    /// Seconds until a locked or rate-limited sign-in may be retried.
    @State private var lockedUntil: Date?
    @FocusState private var focus: Field?

    enum Field { case email, password, key }

    var body: some View {
        ScrollView {
            VStack(spacing: 24) {
                header
                if let reason = app.signedOutReason {
                    ErrorBanner(message: reason, systemImage: "person.crop.circle.badge.exclamationmark")
                }
                Picker("Sign in with", selection: $mode) {
                    ForEach(Mode.allCases) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("loginMode")

                VStack(spacing: 12) {
                    switch mode {
                    case .password: passwordFields
                    case .accessKey: keyField
                    }
                }

                if let error {
                    Text(error)
                        .font(.subheadline)
                        .foregroundStyle(.red)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: .infinity)
                        .accessibilityIdentifier("loginError")
                }

                TimelineView(.periodic(from: .now, by: 1)) { context in
                    let wait = remaining(at: context.date)
                    PrimaryButton(title: wait > 0 ? "Try again in \(wait) s" : "Sign in",
                                  icon: "arrow.right.circle.fill", isBusy: busy) {
                        Task { await submit() }
                    }
                    .disabled(!canSubmit || wait > 0)
                    .accessibilityIdentifier("signIn")
                }

                Text(ServerConfig.isDebugBuild ? "Server: \(app.baseURL.absoluteString)" : "Your data stays between this iPhone, your server and Lucina.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding(24)
            .frame(maxWidth: 480)
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
        .background(Theme.page)
        .onChange(of: mode) { _, _ in error = nil }
    }

    private var header: some View {
        VStack(spacing: 12) {
            Image(systemName: "sparkles")
                .font(.system(size: 34, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 76, height: 76)
                .background(Theme.accent.gradient, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
                .accessibilityHidden(true)
            Text("Secretary").font(.largeTitle.bold())
            Text("Sign in to talk to Lucina, keep the shopping list, and log your training.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(.top, 40)
    }

    private var passwordFields: some View {
        VStack(spacing: 12) {
            TextField("Email", text: $email)
                .textContentType(.username)
                .keyboardType(.emailAddress)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .submitLabel(.next)
                .focused($focus, equals: .email)
                .onSubmit { focus = .password }
                .modifier(LoginFieldStyle())
                .accessibilityIdentifier("loginEmail")
            SecureField("Password", text: $password)
                .textContentType(.password)
                .submitLabel(.go)
                .focused($focus, equals: .password)
                .onSubmit { Task { await submit() } }
                .modifier(LoginFieldStyle())
                .accessibilityIdentifier("loginPassword")
        }
    }

    private var keyField: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                SecureField("sec_…", text: $accessKey)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.go)
                    .focused($focus, equals: .key)
                    .onSubmit { Task { await submit() } }
                    .accessibilityLabel("Access key")
                    .accessibilityIdentifier("accessKey")
                PasteButton(payloadType: String.self) { strings in
                    if let value = strings.first { accessKey = value.trimmingCharacters(in: .whitespacesAndNewlines) }
                }
                .labelStyle(.iconOnly)
                .buttonBorderShape(.capsule)
            }
            .modifier(LoginFieldStyle())
            Text("A device key from `secretary-server token issue`. It is stored in this iPhone's Keychain only.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private var canSubmit: Bool {
        guard !busy else { return false }
        switch mode {
        case .password: return email.contains("@") && !password.isEmpty
        case .accessKey: return !accessKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        }
    }

    private func remaining(at date: Date) -> Int {
        guard let lockedUntil else { return 0 }
        return max(0, Int(lockedUntil.timeIntervalSince(date).rounded(.up)))
    }

    private func submit() async {
        guard canSubmit, remaining(at: Date()) == 0 else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            switch mode {
            case .password: try await app.signIn(email: email, password: password)
            case .accessKey: try await app.signIn(accessKey: accessKey)
            }
            password = ""
            accessKey = ""
        } catch {
            show(APIError.wrap(error))
        }
    }

    private func show(_ error: APIError) {
        switch error {
        case .unauthorized:
            // One generic message: never say which of email or password was wrong.
            self.error = mode == .password ? "That email and password don't match." : "That access key isn't valid."
        case let .rateLimited(_, retryAfter, _):
            lockedUntil = Date().addingTimeInterval(TimeInterval(retryAfter ?? 60))
            self.error = error.userMessage
        case let .server(status, code, _) where mode == .password && (status == 404 || code == "not_found"):
            self.error = "This server doesn't offer password sign-in yet. Use your access key."
            mode = .accessKey
            self.error = "This server doesn't offer password sign-in yet. Use your access key."
        default:
            self.error = error.isConnectivity ? "Can't reach the server. Check your connection." : error.userMessage
        }
    }
}

private struct LoginFieldStyle: ViewModifier {
    func body(content: Content) -> some View {
        content
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}
