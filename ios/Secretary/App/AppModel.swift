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

    /// The owner's four tabs (2026-10-24): Chat, Shopping, Training + Weight, Plugins.
    /// Settings opens as a sheet from the gear button; everything else is a plugin card.
    enum Tab: String, Hashable, CaseIterable {
        case chat, shopping, fitness, plugins

        /// A launch argument or route name (`gym` and `weight` are the training tab).
        init?(name: String) {
            switch name {
            case "gym", "weight", "fitness", "training": self = .fitness
            default: self.init(rawValue: name)
            }
        }
    }

    private(set) var phase: Phase = .checking
    private(set) var deviceName: String?
    /// The endpoint requests currently go to first (public or the tailnet name).
    private(set) var baseURL: URL
    /// Why the user is looking at the sign-in screen (expired token, ...).
    var signedOutReason: String?
    /// Shown above the chat when Lucina is unreachable or slow.
    var lucinaNotice: String?
    var selectedTab: Tab = .chat
    /// Settings, as a sheet from the gear button in every tab's toolbar.
    var showSettings = false
    /// Notes, a plugin (pushed on the Plugins tab).
    var showNotes = false
    /// Tasks, a plugin like Notes.
    var showTasks = false
    /// The grocery deals map, pushed on the Shopping tab (`/shopping/map`).
    var showDealsMap = false
    /// The media library and player, a plugin.
    var showMedia = false
    /// The Gallery, a plugin.
    var showGallery = false
    /// Voice memos, a plugin.
    var showMemos = false

    @ObservationIgnored let client: APIClient
    /// /v1/media/* (Gallery, photo backup, player) lives in p1's Caddy, not in secretary-server, so it
    /// must use the primary URL: the tailnet fallback goes straight to secretary-server (404).
    @ObservationIgnored let mediaClient: APIClient
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
    /// Tasks synced with Todoist (/v1/tasks).
    let tasks: TasksStore
    let health: HealthStore
    let lucina: LucinaStageController
    /// Lucina's outfit (GET/PUT /v1/avatar), applied to the stage.
    let wardrobe: WardrobeStore
    let device: DeviceHub
    /// Inbox, settings and local notifications.
    let notifications: NotificationCenterStore
    /// Videos, audio and photos on the server (/v1/media), played with AVPlayer.
    let media: MediaPlayer
    /// The photo library on the server (/v1/media), shown by the Gallery plugin.
    let gallery: GalleryStore
    /// Automatic backup of this iPhone's photos and videos to the server.
    let backup: PhotoBackup
    /// Voice memos: recorder, upload queue, list (/v1/memos on lucina-memo).
    let memos: MemosStore
    /// Memo requests go to the primary URL only: the tailnet fallback reaches secretary-server
    /// directly, and the /v1/memos route lives in p1's Caddy.
    @ObservationIgnored let memoClient: APIClient
    /// The scene is active (set by `MainTabs`): Lucina's messages alert only when the chat is not in view.
    @ObservationIgnored var isForeground = false

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
        tasks = TasksStore(api: { TasksAPI(client: client) })
        health = HealthStore(api: { SecretaryAPI(client: client) })
        lucina = LucinaStageController(client: client, chat: chat)
        wardrobe = WardrobeStore(api: { AvatarAPI(client: client) })
        device = DeviceHub(client: client, weight: weight)
        notifications = NotificationCenterStore(api: { NotificationsClient(client: client) })
        let mediaClient = APIClient(baseURL: client.primaryURL, accessKey: client.accessKey)
        self.mediaClient = mediaClient
        // The key can change (sign-in): copy it on every use.
        let mediaAPI = { () -> APIClient in
            mediaClient.accessKey = client.accessKey
            return mediaClient
        }
        media = MediaPlayer(api: { MediaAPI(client: mediaAPI()) })
        gallery = GalleryStore(api: { GalleryAPI(client: mediaAPI()) })
        backup = PhotoBackup(api: { GalleryAPI(client: mediaAPI()) })
        let memoClient = APIClient(baseURL: client.primaryURL, accessKey: client.accessKey)
        self.memoClient = memoClient
        memos = MemosStore(api: {
            guard let key = client.accessKey, !key.isEmpty else { return nil }
            memoClient.accessKey = key
            return MemosAPI(client: memoClient)
        })
        if let tab = launch.initialTab { selectedTab = tab }
        #if DEBUG
        MemoSynthetic.runIfRequested(into: memos, defaults: defaults)
        #endif
        // The Live Activity's pause/resume/stop buttons (LiveActivityIntents run in this process).
        MemoRecorder.endStaleActivities()
        let memoStore = memos
        MemoRecordingCommands.handle = { command in
            switch command {
            case .pause: memoStore.recorder.pause()
            case .resume: memoStore.recorder.resume()
            case .stop: memoStore.finishRecording()
            }
        }
        if launch.openSettings { showSettings = true }
        if launch.openNotes {
            selectedTab = .plugins
            showNotes = true
        }
        if launch.openTasks {
            selectedTab = .plugins
            showTasks = true
        }
        if launch.openMemos {
            selectedTab = .plugins
            showMemos = true
        }
        if launch.openGallery {
            selectedTab = .plugins
            showGallery = true
        }
        if launch.openMedia {
            selectedTab = .plugins
            showMedia = true
        }

        let onError: (APIError) -> Void = { [weak self] error in self?.handle(error) }
        let onSuccess: () -> Void = { [weak self] in self?.lucinaNotice = nil }
        chat.onTransportError = onError
        chat.onTransportSuccess = onSuccess
        // Attachments sent from the chat report their upload progress and outcome to its bubbles.
        ShareUploads.shared.observer = chat
        AttachmentStore.purgePending()
        let device = device
        let wardrobe = wardrobe
        let notes = notes
        let notifications = notifications
        notifications.calendarEvents = { await device.calendarAlerts() }
        chat.onItems = { [weak self] items in
            device.bridge.consider(items)
            wardrobe.consider(items)
            notes.consider(items)
            let chatVisible = self.map { $0.isForeground && $0.selectedTab == .chat } ?? false
            Task { await notifications.consider(timeline: items, chatVisible: chatVisible) }
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
        tasks.onTransportError = onError
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
        device.syncCredentials()
        LucinaDonations.donate(daily: true)
        Task { await ShareUploads.shared.retryPending() }
        if backup.settings.enabled { Task { await backup.requestAccessAndRun() } }
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
            device.syncCredentials()
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
        notifications.reset()
        memos.reset()
        showMemos = false
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

    /// `/memos/<id>` (the "memo ready" notification): the memo's detail screen.
    func openMemo(_ id: String?) {
        selectedTab = .plugins
        showMemos = true
        if let id { memos.openMemoId = id }
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
/// `-SecretaryInitialTab gym` opens a tab (screenshots). Release builds read them only when
/// compiled with the SCREENSHOTS condition (the device screenshots: Debug apps do not start on
/// iOS 27 from Xcode 26.6).
struct LaunchOptions {
    var resetState = false
    var initialTab: AppModel.Tab?
    /// `-SecretaryInitialTab settings`: start with the Settings sheet open.
    var openSettings = false
    /// `-SecretaryInitialTab notes`: start on the Notes screen.
    var openNotes = false
    /// `-SecretaryInitialTab tasks`: start on the Tasks screen.
    var openTasks = false
    /// `-SecretaryInitialTab media`: start on the media library.
    var openMedia = false
    /// `-SecretaryInitialTab gallery`: start on the Gallery.
    var openGallery = false
    /// `-SecretaryInitialTab memos`: start on the voice memos.
    var openMemos = false

    init(defaults: UserDefaults) {
        #if DEBUG || SCREENSHOTS
        resetState = defaults.bool(forKey: "SecretaryResetState")
        initialTab = defaults.string(forKey: "SecretaryInitialTab").flatMap(AppModel.Tab.init(name:))
        openSettings = defaults.string(forKey: "SecretaryInitialTab") == "settings"
        openNotes = defaults.string(forKey: "SecretaryInitialTab") == "notes"
        openTasks = defaults.string(forKey: "SecretaryInitialTab") == "tasks"
        openMedia = defaults.string(forKey: "SecretaryInitialTab") == "media"
        openGallery = defaults.string(forKey: "SecretaryInitialTab") == "gallery"
        openMemos = defaults.string(forKey: "SecretaryInitialTab") == "memos"
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
