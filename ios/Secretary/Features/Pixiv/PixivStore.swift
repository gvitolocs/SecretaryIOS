import AuthenticationServices
import Foundation
import Observation
import UIKit

/// Settings → Plugins → Pixiv: the connection, the defaults, and the Connect / Disconnect actions.
@Observable
@MainActor
final class PixivStore {
    enum Busy: Equatable { case connecting, disconnecting }

    private(set) var connection: PixivConnection?
    private(set) var settings = PixivSettings()
    private(set) var busy: Busy?
    private(set) var isLoading = false
    /// The last load failed (the last data stays on screen).
    private(set) var loadError: String?
    /// Why the last Connect or Disconnect failed.
    private(set) var actionError: String?
    /// Why the last save of the defaults failed.
    private(set) var settingsError: String?

    @ObservationIgnored private let api: () -> PixivAPI?
    @ObservationIgnored private let onSessionEnded: (APIError) -> Void
    @ObservationIgnored private let auth: any PixivWebAuthenticating
    @ObservationIgnored private var settingsGeneration = 0
    @ObservationIgnored private var pendingSaves = 0

    init(api: @escaping () -> PixivAPI?, onSessionEnded: @escaping (APIError) -> Void = { _ in },
         auth: (any PixivWebAuthenticating)? = nil) {
        self.api = api
        self.onSessionEnded = onSessionEnded
        self.auth = auth ?? PixivWebAuth()
    }

    var isWaitingForApproval: Bool { connection?.isWaitingForApproval ?? false }

    func load() async {
        guard !Task.isCancelled, let api = api() else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            let status = try await api.status()
            guard !Task.isCancelled else { return }
            connection = status.connection
            // A save in flight owns the pickers: the answer would put the old value back for a moment.
            if pendingSaves == 0 { settings = status.settings }
            loadError = nil
        } catch {
            guard !Task.isCancelled, !(error is CancellationError) else { return }
            let api = APIError.wrap(error)
            if api.endsSession { onSessionEnded(api) }
            loadError = api.userMessage
        }
    }

    /// Refreshes every `interval` seconds while a save or removal waits for Face ID, for at most `maxSeconds`.
    func pollWhileWaiting(interval: Duration = .seconds(3), maxSeconds: TimeInterval = 300) async {
        let deadline = Date().addingTimeInterval(maxSeconds)
        while !Task.isCancelled, isWaitingForApproval, Date() < deadline {
            try? await Task.sleep(for: interval)
            guard !Task.isCancelled else { return }
            await load()
        }
    }

    // MARK: Connect

    func connect() async {
        guard busy == nil, let api = api() else { return }
        busy = .connecting
        actionError = nil
        defer { busy = nil }
        // Told apart: a bare 502 from `start` is the gateway, from `finish` it is pixiv refusing the code.
        var finishing = false
        do {
            let start = try await api.connectStart()
            guard let loginURL = start.url else {
                actionError = String(localized: "Couldn't finish the Pixiv sign-in.")
                return
            }
            let callback: URL
            do {
                callback = try await auth.authenticate(url: loginURL)
            } catch {
                // Closing the sign-in sheet is not an error.
                if !Self.isCancel(error) { actionError = String(localized: "Couldn't finish the Pixiv sign-in.") }
                return
            }
            guard let code = PixivCallback.code(from: callback) else {
                actionError = String(localized: "Couldn't finish the Pixiv sign-in.")
                return
            }
            finishing = true
            // At once: pixiv's code lives well under a minute.
            connection = try await api.connectFinish(state: start.state, code: code)
        } catch {
            let apiError = APIError.wrap(error)
            if apiError.endsSession { onSessionEnded(apiError) }
            actionError = PixivText.connectError(apiError, finishing: finishing)
        }
    }

    static func isCancel(_ error: Error) -> Bool {
        if error is CancellationError { return true }
        let ns = error as NSError
        return ns.domain == ASWebAuthenticationSessionErrorDomain && ns.code == ASWebAuthenticationSessionError.canceledLogin.rawValue
    }

    // MARK: Disconnect

    func disconnect() async {
        guard busy == nil, let api = api() else { return }
        busy = .disconnecting
        actionError = nil
        defer { busy = nil }
        do {
            connection = try await api.disconnect()
        } catch {
            let api = APIError.wrap(error)
            if api.endsSession { onSessionEnded(api) }
            actionError = api.userMessage
        }
    }

    // MARK: Defaults

    func setDefaults(content: PixivSettings.Content? = nil, sort: PixivSettings.Sort? = nil) async {
        guard let api = api() else { return }
        let before = settings
        var next = settings
        if let content { next.defaultContent = content }
        if let sort { next.defaultSort = sort }
        guard next != before else { return }
        settings = next
        settingsGeneration += 1
        let mine = settingsGeneration
        pendingSaves += 1
        defer { pendingSaves -= 1 }
        settingsError = nil
        do {
            let saved = try await api.saveSettings(next)
            if mine == settingsGeneration { settings = saved }
        } catch {
            guard mine == settingsGeneration else { return }
            settings = before
            let apiError = APIError.wrap(error)
            if apiError.endsSession { onSessionEnded(apiError) }
            settingsError = apiError.isConnectivity ? apiError.userMessage : String(localized: "Couldn't save the defaults. Try again.")
        }
    }
}

@MainActor
protocol PixivWebAuthenticating {
    /// Shows pixiv's login page and returns the `pixiv://` callback URL.
    func authenticate(url: URL) async throws -> URL
}

/// Opens pixiv's login page in an ephemeral web authentication session and returns the `pixiv://` callback.
@MainActor
final class PixivWebAuth: PixivWebAuthenticating {
    private var session: ASWebAuthenticationSession?
    private let presenter = PixivAuthPresenter()

    func authenticate(url: URL) async throws -> URL {
        defer { session = nil }
        return try await withCheckedThrowingContinuation { continuation in
            let session = ASWebAuthenticationSession(url: url, callbackURLScheme: "pixiv") { callback, error in
                if let callback {
                    continuation.resume(returning: callback)
                } else {
                    continuation.resume(throwing: error ?? URLError(.unknown))
                }
            }
            session.presentationContextProvider = presenter
            // A fresh browser: nothing from a previous pixiv sign-in is reused or kept.
            session.prefersEphemeralWebBrowserSession = true
            self.session = session
            if !session.start() { continuation.resume(throwing: URLError(.cannotOpenFile)) }
        }
    }
}

/// Presents the sign-in sheet over the key window.
private final class PixivAuthPresenter: NSObject, ASWebAuthenticationPresentationContextProviding {
    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        return scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? ASPresentationAnchor()
    }
}
