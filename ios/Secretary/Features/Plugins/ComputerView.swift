import SwiftUI
import WebKit
import os

// Lucina's computer (plugin card "Lucina's computer"): her Linux desktop and Android sandbox, live, from
// /v1/computer/* (secretary-server → Lucina → the Cua sandboxes on nezopt).
//
// The picture is Cua's own web viewer (H.264 over a WebSocket) in a WKWebView, at a capability URL that
// POST /v1/computer/view mints for this device: watch-only until the owner takes over, then a control
// view with touch, keyboard and text clipboard. Taking over stops Lucina's running task and refuses her
// and the agents' next steps; it is a 45 s lease this screen renews every 15 s and gives back when it
// closes. Lucina logs every takeover and input batch. The web plugin (/computer) has the same controls
// plus a screenshot mode. The screen shows "Connecting to her screen…" until Cua's status dot says
// `streaming` (read by a user script in an isolated content world).
//
// Views live in Lucina's memory: when she restarts (a deploy) or the phone was in the background, the view
// is gone and Cua's viewer can only report that it "could not be re-attached". So a dropped, closed or
// stalled stream opens a fresh view by itself, with backoff (1, 2, 4, 8, 16 s), and so does coming back
// to the foreground. While it does, our overlay covers Cua's page completely: one message, in the app's
// language. "Her screen didn't load" + Try again appears only after the fifth failed try.
//
// Full screen: Cua's toolbar button asks the page (`lucina:fullscreen`, crates/lucina-server/viewer/lucina-shim.js),
// and on the iPhone the viewer IS the page, with no element Fullscreen API in WebKit, so nothing happened. The app now
// catches that click (and has its own button): the same web view fills the screen without the status, navigation and
// tab bars, in landscape, with an exit button in the corner. The stream is not reopened.

struct ComputerControl: Decodable, Equatable, Sendable {
    struct Owner: Decodable, Equatable, Sendable { let device: String }
    struct Work: Decodable, Equatable, Sendable { let who: String; let what: String }
    struct Agent: Decodable, Equatable, Sendable { let who: String }
    let driver: String
    let owner: Owner?
    let task: Work?
    let lastAgent: Agent?
}

struct ComputerControlReply: Decodable, Sendable {
    let state: ComputerControl
    let stoppedTask: String?
}

struct ComputerViewTicket: Decodable, Equatable, Sendable {
    let viewId: String
    /// `/v1/computer/view/<id>/viewer/#ticket=…`: a secret; never logged.
    let viewUrl: String
    let mode: String
    let ttlSecs: Int
}

extension SecretaryAPI {
    func computerControl(_ sandbox: String) async throws -> ComputerControlReply {
        try await client.get("/v1/computer/control", query: [URLQueryItem(name: "sandbox", value: sandbox)])
    }

    /// `take`, `heartbeat` or `release`.
    func computerControl(_ sandbox: String, action: String) async throws -> ComputerControlReply {
        struct Body: Encodable { let sandbox: String; let action: String }
        return try await client.send("POST", "/v1/computer/control", body: Body(sandbox: sandbox, action: action))
    }

    func computerView(_ sandbox: String, mode: String) async throws -> ComputerViewTicket {
        struct Body: Encodable { let sandbox: String; let mode: String }
        return try await client.send("POST", "/v1/computer/view", body: Body(sandbox: sandbox, mode: mode))
    }

    /// The absolute viewer URL on the current server.
    func computerViewURL(_ ticket: ComputerViewTicket) -> URL? {
        let base = client.baseURL.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        return URL(string: base + ticket.viewUrl)
    }
}

struct LucinaComputerView: View {
    @Environment(AppModel.self) private var app
    @State private var sandbox = "workspace"
    @State private var state: ComputerControl?
    @State private var mine = false
    @State private var viewURL: URL?
    @State private var stream = ""
    @State private var viewExpires = Date.distantPast
    @State private var error: String?
    @State private var notice: String?
    @State private var busy = false
    @State private var confirmTake = false
    /// Fresh views opened in a row without reaching `streaming`.
    @State private var failures = 0
    /// After `maxRetries` failures: show the error and Try again.
    @State private var gaveUp = false
    @State private var reconnect: Task<Void, Never>?
    /// When the current view was opened (a stream still not live 20 s later is treated as dropped).
    @State private var openedAt = Date.distantPast
    /// The stream fills the screen (no bars, landscape allowed).
    @State private var fullScreen = false
    @Environment(\.scenePhase) private var scenePhase

    static let maxRetries = 5
    static let connectTimeout: TimeInterval = 20

    var body: some View {
        // One layout for both modes (only modifiers and the hidden rows change), so the web view and its stream
        // stay the same when full screen starts or ends.
        GeometryReader { geo in
            ScrollView {
                VStack(alignment: .leading, spacing: LucinaTokens.Space.md) {
                    if !fullScreen {
                        Picker("Which computer", selection: $sandbox) {
                            Text("Desktop").tag("workspace")
                            Text("Android").tag("android")
                        }
                        .pickerStyle(.segmented)
                    }

                    if !fullScreen {
                        Card(spacing: LucinaTokens.Space.sm) {
                            HStack(spacing: LucinaTokens.Space.sm) {
                                Circle()
                                    .fill(dotColor)
                                    .frame(width: 10, height: 10)
                                    .accessibilityHidden(true)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(verbatim: driverLabel)
                                        .font(LucinaTokens.Fonts.headline)
                                    if let task = state?.task {
                                        Text("Working on: \(task.what)")
                                            .font(LucinaTokens.Fonts.footnote)
                                            .foregroundStyle(Theme.textSecondary)
                                            .lineLimit(2)
                                    } else if !mine {
                                        Text("You are watching. Take over to use touch and the keyboard.")
                                            .font(LucinaTokens.Fonts.footnote)
                                            .foregroundStyle(Theme.textSecondary)
                                    }
                                }
                                Spacer(minLength: 0)
                            }
                            if mine {
                                Button {
                                    Task { await giveBack() }
                                } label: {
                                    Label("Give back", systemImage: "hand.raised.slash").frame(maxWidth: .infinity)
                                }
                                .buttonStyle(.bordered)
                                .disabled(busy)
                            } else {
                                Button {
                                    if state?.driver == "lucina" || state?.driver == "agent" { confirmTake = true } else { Task { await takeOver() } }
                                } label: {
                                    Label("Take over", systemImage: "hand.point.up.left").frame(maxWidth: .infinity)
                                }
                                .buttonStyle(.borderedProminent)
                                .tint(Theme.accent)
                                .disabled(busy || state == nil)
                            }
                            if let notice {
                                Text(verbatim: notice).font(LucinaTokens.Fonts.footnote).foregroundStyle(Theme.textSecondary)
                            }
                            if let error {
                                Text(verbatim: error).font(LucinaTokens.Fonts.footnote).foregroundStyle(Theme.negative)
                            }
                        }
                        .overlay(
                            RoundedRectangle(cornerRadius: LucinaTokens.Radius.card, style: .continuous)
                                .stroke(mine ? Theme.warning : .clear, lineWidth: 2)
                        )
                    }

                    ZStack {
                        Color.black
                        if let viewURL {
                            ComputerWebView(url: viewURL, onState: { streamChanged($0) },
                                            onFullScreen: { setFullScreen(!fullScreen) })
                                .accessibilityLabel(Text("Lucina's screen"))
                        }
                        streamOverlay
                    }
                    .aspectRatio(1280.0 / 800.0, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: fullScreen ? 0 : 10, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .stroke(fullScreen ? .clear : Theme.separator, lineWidth: 1)
                    )
                    .frame(maxWidth: .infinity, minHeight: fullScreen ? geo.size.height : nil)
                }
                .padding(.horizontal, fullScreen ? 0 : LucinaTokens.Space.gutter)
                .padding(.top, fullScreen ? 0 : LucinaTokens.Space.sm)
                .padding(.bottom, fullScreen ? 0 : LucinaTokens.Space.xxl)
            }
            .scrollDisabled(fullScreen)
        }
        .background { if fullScreen { Color.black.ignoresSafeArea() } }
        .ignoresSafeArea(edges: fullScreen ? .all : [])
        // The way out stays inside the safe area (clear of the notch in landscape).
        .overlay(alignment: .topTrailing) {
            if fullScreen {
                Button { setFullScreen(false) } label: {
                    Image(systemName: "arrow.down.right.and.arrow.up.left")
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.white)
                        .frame(width: 44, height: 44)
                        .background(.black.opacity(0.55), in: Circle())
                        .overlay(Circle().stroke(.white.opacity(0.35), lineWidth: 1))
                }
                .padding(LucinaTokens.Space.sm)
                .accessibilityLabel(Text("Exit full screen"))
                .accessibilityIdentifier("computer-exit-fullscreen")
            }
        }
        .lucinaScreen("Lucina's computer")
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { setFullScreen(true) } label: {
                    Label("Full screen", systemImage: "arrow.up.left.and.arrow.down.right")
                }
                .disabled(viewURL == nil)
                .accessibilityIdentifier("computer-fullscreen")
            }
        }
        .toolbar(fullScreen ? .hidden : .automatic, for: .navigationBar)
        .toolbar(fullScreen ? .hidden : .automatic, for: .tabBar)
        .statusBarHidden(fullScreen)
        .persistentSystemOverlays(fullScreen ? .hidden : .automatic)
        .confirmationDialog("Take over Lucina's computer?", isPresented: $confirmTake, titleVisibility: .visible) {
            Button("Take over", role: .destructive) { Task { await takeOver() } }
        } message: {
            if let task = state?.task {
                Text("Lucina is working on “\(task.what)”. Taking over stops her task.")
            } else {
                Text("An agent is using this computer. Taking over refuses its next steps.")
            }
        }
        // While visible: who drives every 3 s, the lease every 15 s, a fresh view before it expires.
        .task(id: sandbox) {
            if mine { await release(other: true) }
            state = nil
            viewURL = nil
            stream = ""
            await openView(mode: "watch")
            var tick = 0
            while !Task.isCancelled {
                await refresh()
                if mine, tick % 5 == 4 { await heartbeat() }
                if Date() > viewExpires { await openView(mode: mine ? "control" : "watch") }
                // Connected but never streaming (the socket hangs): count it as a drop.
                if viewURL != nil, stream != "streaming", reconnect == nil, !gaveUp,
                   Date().timeIntervalSince(openedAt) > Self.connectTimeout {
                    scheduleReconnect()
                }
                tick += 1
                try? await Task.sleep(for: .seconds(3))
            }
        }
        // Back from the background: the socket is gone (iOS suspends it), so open a fresh view at once.
        .onChange(of: scenePhase) { old, phase in
            guard phase == .active, old == .background, viewURL != nil else { return }
            restartReconnecting()
        }
        .onDisappear {
            if fullScreen { setFullScreen(false) }
            reconnect?.cancel()
            reconnect = nil
            if mine {
                let api = app.api, sb = sandbox
                Task { _ = try? await api.computerControl(sb, action: "release") }
            }
        }
    }

    @ViewBuilder private var streamOverlay: some View {
        Group {
            if viewURL == nil {
                if error == nil { ProgressView().tint(.white) }
            } else if stream != "streaming" {
                // Opaque: Cua's own connecting and error screens never show through (one message, the app's language).
                ZStack {
                    Color.black
                    if gaveUp {
                        VStack(spacing: LucinaTokens.Space.sm) {
                            Text("Her screen didn't load.")
                                .font(LucinaTokens.Fonts.footnote)
                                .foregroundStyle(.white.opacity(0.8))
                            Button("Try again") { restartReconnecting() }
                                .buttonStyle(.bordered)
                        }
                    } else {
                        VStack(spacing: LucinaTokens.Space.sm) {
                            ProgressView().tint(.white)
                            Group {
                                if failures > 0 || stream == "reconnecting" {
                                    Text("Reconnecting…")
                                } else {
                                    Text("Connecting to her screen…")
                                }
                            }
                            .font(LucinaTokens.Fonts.footnote)
                            .foregroundStyle(.white.opacity(0.8))
                        }
                        .allowsHitTesting(false)
                    }
                }
            }
        }
        .accessibilityElement(children: .combine)
    }

    /// Full screen turns landscape on (and asks for it: the desktop is wide); leaving it goes back to portrait.
    private func setFullScreen(_ on: Bool) {
        guard on != fullScreen else { return }
        ComputerWebView.log.notice("ComputerFullScreen \(on ? "on" : "off", privacy: .public)")
        withAnimation(.easeInOut(duration: 0.2)) { fullScreen = on }
        OrientationLock.set(on ? .allButUpsideDown : .portrait)
        if on, UIDevice.current.userInterfaceIdiom == .phone {
            for scene in UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }) {
                scene.requestGeometryUpdate(.iOS(interfaceOrientations: .landscapeRight)) { _ in }
            }
        }
    }

    /// Cua's status dot changed.
    private func streamChanged(_ state: String) {
        stream = state
        switch state {
        case "streaming":
            failures = 0
            gaveUp = false
            reconnect?.cancel()
            reconnect = nil
        case "failed", "closed":
            scheduleReconnect()
        default:
            break
        }
    }

    /// A fresh view after a short wait; the wait doubles with each failure in a row, and the fifth gives up.
    private func scheduleReconnect() {
        guard reconnect == nil, !gaveUp else { return }
        failures += 1
        guard failures <= Self.maxRetries else {
            gaveUp = true
            return
        }
        let delay = Double(1 << (failures - 1))
        reconnect = Task {
            try? await Task.sleep(for: .seconds(delay))
            guard !Task.isCancelled else { return }
            await openView(mode: mine ? "control" : "watch")
            reconnect = nil
        }
    }

    /// Try again, or back from the background: start over with a fresh view now.
    private func restartReconnecting() {
        reconnect?.cancel()
        reconnect = nil
        failures = 0
        gaveUp = false
        Task { await openView(mode: mine ? "control" : "watch") }
    }

    private var driverLabel: String {
        guard let state else { return String(localized: "Connecting…") }
        switch state.driver {
        case "owner":
            return mine ? String(localized: "You are driving") : String(localized: "Driving from \(state.owner?.device ?? "?")")
        case "lucina":
            if let who = state.task?.who, who != "lucina" { return String(localized: "\(Self.callerName(who)) is driving") }
            return String(localized: "Lucina is driving")
        case "agent":
            return String(localized: "\(Self.callerName(state.lastAgent?.who ?? "agent")) is driving")
        default:
            return String(localized: "Idle")
        }
    }

    private var dotColor: Color {
        switch state?.driver {
        case "owner": return Theme.warning
        case "lucina", "agent": return Theme.accent
        case "none": return Theme.positive
        default: return Theme.textTertiary
        }
    }

    /// `mcp:claude` → "Claude".
    static func callerName(_ who: String) -> String {
        let name = who.replacingOccurrences(of: "mcp:", with: "").replacingOccurrences(of: "-", with: " ")
        return name.prefix(1).uppercased() + name.dropFirst()
    }

    private func refresh() async {
        do {
            let reply = try await app.api.computerControl(sandbox)
            state = reply.state
            error = nil
            if mine, reply.state.driver != "owner" { lost() }
        } catch {
            self.error = String(localized: "Lucina's computer is not reachable right now.")
        }
    }

    private func openView(mode: String) async {
        do {
            let ticket = try await app.api.computerView(sandbox, mode: mode)
            stream = ""
            openedAt = Date()
            viewURL = app.api.computerViewURL(ticket)
            viewExpires = Date().addingTimeInterval(TimeInterval(max(ticket.ttlSecs - 60, 30)))
        } catch {
            viewURL = nil
            self.error = String(localized: "Lucina's computer is not reachable right now.")
        }
    }

    private func takeOver() async {
        busy = true
        defer { busy = false }
        do {
            let reply = try await app.api.computerControl(sandbox, action: "take")
            state = reply.state
            mine = true
            notice = reply.stoppedTask.map { String(localized: "Stopped: \($0)") }
            await openView(mode: "control")
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func giveBack() async {
        busy = true
        defer { busy = false }
        await release(other: false)
        await openView(mode: "watch")
        await refresh()
    }

    private func release(other: Bool) async {
        _ = try? await app.api.computerControl(sandbox, action: "release")
        mine = false
        notice = nil
    }

    private func heartbeat() async {
        do {
            _ = try await app.api.computerControl(sandbox, action: "heartbeat")
        } catch let e as APIError where e.code == "not_driving" {
            lost()
        } catch {}
    }

    /// The lease ended elsewhere (expired, or given back from another device).
    private func lost() {
        mine = false
        notice = String(localized: "Your takeover ended. Lucina can use her computer again.")
        Task { await openView(mode: "watch") }
    }
}

/// Cua's viewer: a non-persistent WKWebView (no cookies or storage shared with anything else).
struct ComputerWebView: UIViewRepresentable {
    static let dataStore = WKWebsiteDataStore.nonPersistent()
    static let log = Logger(subsystem: "tech.getsecretary.app", category: "Computer")
    let url: URL
    let onState: @MainActor (String) -> Void
    /// Cua's toolbar full-screen button was tapped.
    var onFullScreen: @MainActor () -> Void = {}

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = Self.dataStore
        configuration.userContentController.add(WeakMessageHandler(context.coordinator), contentWorld: .defaultClient, name: "lucinaView")
        configuration.userContentController.addUserScript(WKUserScript(source: """
        (function () {
          if (window.top !== window) return;
          var last = "";
          function report(s) {
            if (!s || s === last) return;
            last = s;
            try { window.webkit.messageHandlers.lucinaView.postMessage(s); } catch (e) {}
          }
          var timer = setInterval(function () {
            var dot = document.querySelector(".cua-dot");
            if (!dot) return;
            clearInterval(timer);
            report(dot.getAttribute("data-state") || "connecting");
            new MutationObserver(function () { report(dot.getAttribute("data-state") || ""); })
              .observe(dot, { attributes: true, attributeFilter: ["data-state"] });
          }, 250);
          // Cua's full-screen button: lucina-shim.js stops the click and asks the parent window, which on the
          // iPhone is this page itself. The DOM is shared across worlds, so a capture listener on the window
          // (it runs before the shim's on the document) tells the app.
          window.addEventListener("click", function (event) {
            var target = event.target;
            if (target && target.closest && target.closest('[data-id="fullscreen"]')) {
              try { window.webkit.messageHandlers.lucinaView.postMessage("fullscreen"); } catch (e) {}
            }
          }, true);
        })();
        """, injectionTime: .atDocumentEnd, forMainFrameOnly: true, in: .defaultClient))
        configuration.allowsInlineMediaPlayback = true
        configuration.mediaTypesRequiringUserActionForPlayback = []
        let view = WKWebView(frame: .zero, configuration: configuration)
        context.coordinator.webView = view
        view.isOpaque = false
        view.backgroundColor = .black
        view.scrollView.isScrollEnabled = false
        view.scrollView.contentInsetAdjustmentBehavior = .never
        view.load(URLRequest(url: url))
        context.coordinator.loaded = url
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {
        context.coordinator.onState = onState
        context.coordinator.onFullScreen = onFullScreen
        guard context.coordinator.loaded != url else { return }
        context.coordinator.loaded = url
        Task { @MainActor in onState("") }
        view.load(URLRequest(url: url))
    }

    static func dismantleUIView(_ view: WKWebView, coordinator: Coordinator) {
        view.configuration.userContentController.removeScriptMessageHandler(forName: "lucinaView", contentWorld: .defaultClient)
    }

    func makeCoordinator() -> Coordinator { Coordinator(onState: onState, onFullScreen: onFullScreen) }

    @MainActor final class Coordinator {
        var loaded: URL?
        weak var webView: WKWebView?
        private var clickedFullScreen = false
        var onState: @MainActor (String) -> Void
        var onFullScreen: @MainActor () -> Void

        init(onState: @escaping @MainActor (String) -> Void, onFullScreen: @escaping @MainActor () -> Void) {
            self.onState = onState
            self.onFullScreen = onFullScreen
        }

        /// A message from the page script: `fullscreen`, or the status dot's state.
        func received(_ message: String) {
            if message == "fullscreen" { onFullScreen() } else { onState(message) }
            if message == "streaming", !clickedFullScreen, let hook = PluginScreenshotHooks.computerFullScreen, hook.hasPrefix("page") {
                clickedFullScreen = true
                Task { @MainActor [weak self] in
                    try? await Task.sleep(for: .seconds(3))
                    self?.clickCuaFullScreen()
                    guard hook == "page-twice" else { return }
                    try? await Task.sleep(for: .seconds(20))
                    self?.clickCuaFullScreen()
                }
            }
        }

        /// Screenshot builds only (the hook is nil otherwise): the button in Cua's toolbar, clicked as a tap would.
        private func clickCuaFullScreen() {
            let script = "var b = document.querySelector('[data-id=fullscreen]'); if (b) b.click(); "
                + "Array.from(document.querySelectorAll('[data-id]')).map(function (e) { return e.getAttribute('data-id'); }).join(',') + ' clicked=' + !!b"
            webView?.evaluateJavaScript(script, in: nil, in: .defaultClient) { result in
                ComputerWebView.log.notice("ComputerFullScreen hook: \(String(describing: result), privacy: .public)")
            }
        }
    }

    final class WeakMessageHandler: NSObject, WKScriptMessageHandler {
        weak var coordinator: Coordinator?

        init(_ coordinator: Coordinator) { self.coordinator = coordinator }

        func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
            guard let state = message.body as? String else { return }
            Task { @MainActor [weak coordinator] in coordinator?.received(state) }
        }
    }
}
