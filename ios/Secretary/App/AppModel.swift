import Foundation
import Observation
import UIKit

/// App-wide state: the server, the stored device token, sign-in/out, the shared
/// "Lucina is offline" banner, and one store per feature.
@Observable
@MainActor
final class AppModel {
    enum Phase: Equatable { case checking, signedOut, signedIn }

    enum Tab: String, Hashable, CaseIterable { case chat, shopping, gym, weight, settings }

    private(set) var phase: Phase = .checking
    private(set) var deviceName: String?
    private(set) var baseURL: URL
    /// Why the user is looking at the sign-in screen (expired token, ...).
    var signedOutReason: String?
    /// Shown above the chat when Lucina is unreachable or slow.
    var lucinaNotice: String?
    var selectedTab: Tab = .chat

    @ObservationIgnored let client: APIClient
    @ObservationIgnored let keychain: KeychainStore

    let chat: ChatStore
    let shopping: ShoppingStore
    let gym: GymStore
    let weight: WeightStore
    let health: HealthStore

    var api: SecretaryAPI { SecretaryAPI(client: client) }

    init(keychain: KeychainStore = .accessKey, defaults: UserDefaults = .standard) {
        let baseURL = ServerConfig.resolvedBaseURL(defaults: defaults)
        let launch = LaunchOptions(defaults: defaults)
        if launch.resetState {
            try? keychain.delete()
            try? FileManager.default.removeItem(at: ChatStore.defaultFileURL)
            defaults.removeObject(forKey: ChatStore.sessionIdKey)
        }
        let client = APIClient(baseURL: baseURL, accessKey: try? keychain.read())
        self.client = client
        self.keychain = keychain
        self.baseURL = baseURL
        chat = ChatStore(api: { SecretaryAPI(client: client) })
        shopping = ShoppingStore(api: { SecretaryAPI(client: client) })
        gym = GymStore(transport: HTTPGymTransport(client: client))
        weight = WeightStore(client: { BodyClient(client: client) })
        health = HealthStore(api: { SecretaryAPI(client: client) })
        if let tab = launch.initialTab { selectedTab = tab }

        let onError: (APIError) -> Void = { [weak self] error in self?.handle(error) }
        let onSuccess: () -> Void = { [weak self] in self?.lucinaNotice = nil }
        chat.onTransportError = onError
        chat.onTransportSuccess = onSuccess
        shopping.onTransportError = onError
        shopping.onTransportSuccess = onSuccess
        gym.onTransportError = onError
        weight.onTransportError = onError
    }

    // MARK: - Session

    /// On launch: with a stored token go straight in and confirm it in the background.
    func start() async {
        guard client.accessKey?.isEmpty == false else {
            phase = .signedOut
            return
        }
        phase = .signedIn
        await health.refresh()
        do {
            let session = try await api.session()
            if session.authenticated {
                deviceName = session.deviceName
            } else {
                signOut(reason: "Your sign-in expired. Please sign in again.")
            }
        } catch {
            // Offline or server trouble: stay signed in; the stores show their own errors.
            handle(APIError.wrap(error))
        }
    }

    /// Email + password → device token (`POST /v1/auth/device`).
    func signIn(email: String, password: String) async throws {
        let request = DeviceLoginRequest(email: email.trimmingCharacters(in: .whitespacesAndNewlines),
                                         password: password, deviceName: UIDevice.current.name)
        let token = try await api.login(request)
        try await adopt(token: token.token, deviceName: token.deviceName)
    }

    /// A `sec_…` device key issued on the server (`secretary-server token issue`).
    func signIn(accessKey raw: String) async throws {
        let key = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        try await adopt(token: key, deviceName: nil)
    }

    private func adopt(token: String, deviceName: String?) async throws {
        let previous = client.accessKey
        client.accessKey = token
        do {
            let session = try await api.session()
            guard session.authenticated else {
                client.accessKey = previous
                throw APIError.unauthorized(code: "unauthorized", message: "unknown device token")
            }
            try keychain.save(token)
            self.deviceName = session.deviceName ?? deviceName
            signedOutReason = nil
            phase = .signedIn
            await health.refresh()
        } catch {
            client.accessKey = previous
            throw error
        }
    }

    func changePassword(current: String, new: String) async throws {
        try await api.changePassword(ChangePasswordRequest(currentPassword: current, newPassword: new))
    }

    /// Deletes the token locally and forgets everything on this device.
    func signOut(reason: String? = nil) {
        try? keychain.delete()
        client.accessKey = nil
        deviceName = nil
        chat.clear()
        shopping.reset()
        gym.reset()
        weight.reset()
        lucinaNotice = nil
        signedOutReason = reason
        selectedTab = .chat
        phase = .signedOut
    }

    /// Shared handling of transport failures from any store.
    func handle(_ error: APIError) {
        if error.endsSession, phase == .signedIn {
            signOut(reason: "This device was signed out. Please sign in again.")
            return
        }
        if error.isLucinaOffline {
            lucinaNotice = error.code == "lucina_timeout"
                ? "Lucina took too long to answer. She may be busy; try again in a moment."
                : "Lucina is offline right now. Your messages stay here, so you can retry once she's back."
        }
    }

    // MARK: - Debug server override

    var canOverrideServer: Bool { ServerConfig.isDebugBuild && !ServerConfig.isPinnedByLaunchArgument() }

    /// Debug builds only: point the app at another server (signs out first).
    func setServerOverride(_ raw: String?) -> Bool {
        guard ServerConfig.isDebugBuild else { return false }
        if let raw, !raw.isEmpty {
            guard ServerConfig.validated(raw) != nil else { return false }
            UserDefaults.standard.set(raw, forKey: ServerConfig.overrideKey)
        } else {
            UserDefaults.standard.removeObject(forKey: ServerConfig.overrideKey)
        }
        signOut()
        baseURL = ServerConfig.resolvedBaseURL()
        client.baseURL = baseURL
        Task { await health.refresh() }
        return true
    }
}

/// Debug-only launch arguments (read from the UserDefaults argument domain):
/// `-SecretaryResetState YES` wipes the token and the local chat (UI tests),
/// `-SecretaryInitialTab gym` opens a tab (screenshots).
struct LaunchOptions {
    var resetState = false
    var initialTab: AppModel.Tab?

    init(defaults: UserDefaults) {
        #if DEBUG
        resetState = defaults.bool(forKey: "SecretaryResetState")
        initialTab = defaults.string(forKey: "SecretaryInitialTab").flatMap(AppModel.Tab.init(rawValue:))
        #endif
    }
}
