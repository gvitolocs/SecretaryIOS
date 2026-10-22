import Foundation
import Observation
import UIKit

/// App-wide state: the server, the stored device token, sign-in/out, the shared
/// "Lucina is offline" banner, and one store per feature.
@Observable
@MainActor
final class AppModel {
    /// One app model per process: views, App Intents and the Focus filter share it.
    static let shared = AppModel()

    enum Phase: Equatable { case checking, signedOut, signedIn }

    enum Tab: String, Hashable, CaseIterable { case chat, shopping, gym, weight, settings }

    private(set) var phase: Phase = .checking
    private(set) var deviceName: String?
    /// The endpoint requests currently go to first (public or the tailnet name).
    private(set) var baseURL: URL
    /// Why the user is looking at the sign-in screen (expired token, ...).
    var signedOutReason: String?
    /// Shown above the chat when Lucina is unreachable or slow.
    var lucinaNotice: String?
    var selectedTab: Tab = .chat
    /// Notes open on top of the chat (the tab bar has no room for a sixth tab on iPhone).
    var showNotes = false

    @ObservationIgnored let client: APIClient
    @ObservationIgnored let keychain: KeychainStore

    let chat: ChatStore
    let shopping: ShoppingStore
    let shoppingV2: ShoppingV2Store
    /// Replaced when the owner moves training between the iPhone and the server.
    private(set) var gym: GymStore
    private(set) var gymLocation: GymLocation
    /// Set when the on-device engine could not open (the server engine is used instead).
    private(set) var gymEngineError: String?
    let weight: WeightStore
    /// Notes, briefings and documents (/v1/notes).
    let notes: NotesStore
    let health: HealthStore
    let lucina: LucinaStageController
    /// Lucina's outfit (GET/PUT /v1/avatar), applied to the stage.
    let wardrobe: WardrobeStore
    let device: DeviceHub

    var api: SecretaryAPI { SecretaryAPI(client: client) }

    init(keychain: KeychainStore = .accessKey, defaults: UserDefaults = .standard) {
        let launch = LaunchOptions(defaults: defaults)
        if launch.resetState {
            try? keychain.delete()
            try? FileManager.default.removeItem(at: ChatStore.defaultFileURL)
            try? FileManager.default.removeItem(at: FFIGymEngine.defaultFileURL)
            try? FileManager.default.removeItem(at: ShoppingV2Store.defaultFileURL)
            try? FileManager.default.removeItem(at: DeviceBridge.defaultFileURL)
            defaults.removeObject(forKey: GymLocation.key)
            defaults.removeObject(forKey: ChatStore.sessionIdKey)
        }
        let client = APIClient(endpoints: ServerConfig.resolvedEndpoints(defaults: defaults), accessKey: try? keychain.read(),
                               defaults: defaults)
        self.client = client
        self.keychain = keychain
        baseURL = client.baseURL
        chat = ChatStore(api: { SecretaryAPI(client: client) })
        shopping = ShoppingStore(api: { SecretaryAPI(client: client) })
        shoppingV2 = ShoppingV2Store(api: { ShoppingV2Client(client: client) })
        let location = GymLocation(rawValue: defaults.string(forKey: GymLocation.key) ?? "") ?? .device
        let engine = Result { try FFIGymEngine(fileURL: FFIGymEngine.defaultFileURL) }
        let gymEngine = try? engine.get()
        deviceEngine = gymEngine
        if location == .device, let gymEngine {
            gym = GymStore(transport: FFIGymTransport(engine: gymEngine), clockInterval: .seconds(1))
            gymLocation = .device
            gymEngineError = nil
        } else {
            gym = GymStore(transport: HTTPGymTransport(client: client))
            gymLocation = .server
            if case let .failure(error) = engine { gymEngineError = error.localizedDescription } else { gymEngineError = nil }
        }
        weight = WeightStore(client: { BodyClient(client: client) })
        notes = NotesStore(api: { NotesAPI(client: client) })
        health = HealthStore(api: { SecretaryAPI(client: client) })
        lucina = LucinaStageController(client: client, chat: chat)
        wardrobe = WardrobeStore(api: { AvatarAPI(client: client) })
        device = DeviceHub(client: client, weight: weight)
        if let tab = launch.initialTab { selectedTab = tab }
        if launch.openNotes {
            selectedTab = .chat
            showNotes = true
        }

        let onError: (APIError) -> Void = { [weak self] error in self?.handle(error) }
        let onSuccess: () -> Void = { [weak self] in self?.lucinaNotice = nil }
        chat.onTransportError = onError
        chat.onTransportSuccess = onSuccess
        let device = device
        let wardrobe = wardrobe
        let notes = notes
        chat.onItems = { items in
            device.bridge.consider(items)
            wardrobe.consider(items)
            notes.consider(items)
        }
        weight.onLogged = { grams, date in Task { await device.health.exportWeight(grams: grams, at: date) } }
        gym.onFinished = { start, end in Task { await device.health.exportWorkout(start: start, end: end) } }
        shopping.onTransportError = onError
        shopping.onTransportSuccess = onSuccess
        shoppingV2.onTransportError = onError
        gym.onTransportError = onError
        transportErrorHandler = onError
        weight.onTransportError = onError
        notes.onTransportError = onError
        client.onEndpointChange = { [weak self] url in
            Task { @MainActor in self?.baseURL = url }
        }
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
                                         password: password, deviceName: String(UIDevice.current.name.prefix(64)))
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
        lucina.stopEverything()
        device.bridge.reset()
        BridgeCredentials.write(nil)
        lucina.expanded = false
        chat.clear()
        shopping.reset()
        shoppingV2.reset()
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
                ? String(localized: "Lucina took too long to answer. She may be busy; try again in a moment.")
                : String(localized: "Lucina is offline right now. Your messages stay here, so you can retry once she's back.")
        }
    }

    // MARK: - Gym engine

    @ObservationIgnored private var transportErrorHandler: ((APIError) -> Void)?
    /// One engine handle per file, shared by the store and sync.
    @ObservationIgnored private var deviceEngine: FFIGymEngine?

    private func makeGymStore(_ location: GymLocation) -> GymStore {
        let store: GymStore
        if location == .device, let engine = deviceEngine {
            store = GymStore(transport: FFIGymTransport(engine: engine), clockInterval: .seconds(1))
        } else {
            store = GymStore(transport: HTTPGymTransport(client: client))
        }
        store.onTransportError = transportErrorHandler
        store.onFinished = gym.onFinished
        return store
    }

    func setGymLocation(_ location: GymLocation) {
        guard location != gymLocation else { return }
        UserDefaults.standard.set(location.rawValue, forKey: GymLocation.key)
        guard location == .server || deviceEngine != nil else { return }
        let store = makeGymStore(location)
        gym.reset()
        gym = store
        gymLocation = location
        Task { await store.refresh() }
    }

    private func gymSync() throws -> GymSync {
        guard let engine = deviceEngine else { throw GymSync.SyncError.failed(gymEngineError ?? String(localized: "The on-device gym engine isn't available.")) }
        return GymSync(device: FFIGymTransport(engine: engine), server: HTTPGymTransport(client: client))
    }

    /// Replaces the server's gym data with this iPhone's.
    func uploadGymToServer() async throws {
        try await gymSync().upload()
        await gym.refresh()
    }

    /// Replaces this iPhone's gym data with the server's.
    func downloadGymFromServer() async throws {
        try await gymSync().download()
        await gym.refresh()
    }

    // MARK: - Endpoints

    var endpointLabel: String { ServerConfig.label(for: baseURL) }

    var endpoints: [URL] { client.endpoints }

    /// While a fallback endpoint is active, re-probes the primary every few minutes and
    /// switches back when it answers.
    func watchPrimary(every interval: Duration = .seconds(180)) async {
        while !Task.isCancelled {
            try? await Task.sleep(for: interval)
            if Task.isCancelled { break }
            await client.reprobePrimary()
        }
    }

    /// Probes every endpoint (no fallback) for the Settings "Test connection" button.
    func testConnection() async -> [APIClient.Probe] {
        var results: [APIClient.Probe] = []
        for url in client.endpoints { results.append(await client.probe(url)) }
        if let first = results.first(where: \.ok), !results.contains(where: { $0.ok && $0.url == baseURL }) {
            client.markWorking(first.url)
        }
        return results
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
        client.setEndpoints(ServerConfig.resolvedEndpoints())
        baseURL = client.baseURL
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
    /// `-SecretaryInitialTab notes`: start on the Notes screen.
    var openNotes = false

    init(defaults: UserDefaults) {
        #if DEBUG
        resetState = defaults.bool(forKey: "SecretaryResetState")
        initialTab = defaults.string(forKey: "SecretaryInitialTab").flatMap(AppModel.Tab.init(rawValue:))
        openNotes = defaults.string(forKey: "SecretaryInitialTab") == "notes"
        #endif
    }
}

/// Where the gym engine runs.
enum GymLocation: String, CaseIterable, Sendable {
    /// `secretary-ffi` on this iPhone: works offline (like the original LucinaGym).
    case device
    /// `/v1/gym/*` on secretary-server: shared with the web app, needs the network.
    case server

    static let key = "secretary.gym.location"

    var title: String { self == .device ? String(localized: "On this iPhone") : String(localized: "On the server") }
}
