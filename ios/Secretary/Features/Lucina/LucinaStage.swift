import AVFoundation
import Foundation
import Observation
import os
import Speech
import SwiftUI
import WebKit

// Persistent 3D Lucina: the shared three.js stage (packages/lucina-stage, bundled as
// Resources/Stage/lucina-stage.standalone.js) in one WKWebView, shown as a small
// draggable avatar over every tab on iPhone and as a full-body side column on iPad or in
// landscape. Voice input is native (SFSpeechRecognizer, it-IT); the transcript goes to
// /v1/chat like a typed message; the reply is spoken through the stage only when the
// owner has turned the sound on. Muted by default, never plays anything by itself.

/// Serves the stage under `lucina-app://stage/`:
/// - `stage.html`, `lucina-stage.standalone.js` from the app bundle;
/// - `v1/assets/<name>`: the private model and manifest, fetched from the server with the
///   device token (it never reaches the page) and cached on disk by ETag, so a swapped
///   model is picked up by the next load;
/// - `audio/<id>`: speech Swift prepared for one `speak` call.
@MainActor
final class LucinaSchemeHandler: NSObject, WKURLSchemeHandler {
    static let scheme = "lucina-app"
    static let base = "\(scheme)://stage/"

    private let client: APIClient
    private var stopped = Set<ObjectIdentifier>()
    private var audio: [String: Data] = [:]

    init(client: APIClient) {
        self.client = client
    }

    /// Makes `data` available at a one-shot URL for the page.
    func stash(_ data: Data) -> String {
        let id = UUID().uuidString.lowercased()
        audio = [id: data] // only the clip being spoken is kept
        return Self.base + "audio/\(id).mp3"
    }

    func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
        let key = ObjectIdentifier(task)
        stopped.remove(key)
        guard let url = task.request.url else { return finish(task, status: 400, data: Data(), type: "text/plain") }
        let path = url.path.hasPrefix("/") ? String(url.path.dropFirst()) : url.path
        switch path {
        case "stage.html":
            serveBundle(task, name: "stage", ext: "html", type: "text/html")
        case "lucina-stage.standalone.js":
            serveBundle(task, name: "lucina-stage.standalone", ext: "js", type: "text/javascript")
        case let p where p.hasPrefix("audio/"):
            let id = String(p.dropFirst("audio/".count)).replacingOccurrences(of: ".mp3", with: "")
            if let data = audio[id] {
                finish(task, status: 200, data: data, type: "audio/mpeg")
            } else {
                finish(task, status: 404, data: Data(), type: "text/plain")
            }
        case let p where p.hasPrefix("v1/assets/"):
            let name = String(p.dropFirst("v1/assets/".count))
            guard name.range(of: #"^[a-z0-9][a-z0-9._-]{0,127}$"#, options: .regularExpression) != nil, !name.contains("..") else {
                return finish(task, status: 404, data: Data(), type: "text/plain")
            }
            Task { await self.serveAsset(task, name: name) }
        default:
            finish(task, status: 404, data: Data(), type: "text/plain")
        }
    }

    func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {
        stopped.insert(ObjectIdentifier(task))
    }

    private func serveBundle(_ task: any WKURLSchemeTask, name: String, ext: String, type: String) {
        guard let url = Bundle.main.url(forResource: name, withExtension: ext), let data = try? Data(contentsOf: url) else {
            return finish(task, status: 404, data: Data(), type: "text/plain")
        }
        finish(task, status: 200, data: data, type: type)
    }

    private static var cacheDirectory: URL {
        let base = (try? FileManager.default.url(for: .cachesDirectory, in: .userDomainMask, appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("lucina-assets", isDirectory: true)
    }

    private func serveAsset(_ task: any WKURLSchemeTask, name: String) async {
        let file = Self.cacheDirectory.appendingPathComponent(name)
        let tagFile = Self.cacheDirectory.appendingPathComponent(name + ".etag")
        let cached = try? Data(contentsOf: file)
        let tag = cached == nil ? nil : (try? String(contentsOf: tagFile, encoding: .utf8))
        let type = name.hasSuffix(".json") ? "application/json" : name.hasSuffix(".glb") ? "model/gltf-binary" : "application/octet-stream"
        do {
            var headers: [String: String] = [:]
            if let tag { headers["If-None-Match"] = tag }
            let response = try await client.send("GET", "/v1/assets/\(name)", body: nil, timeout: 180, passThrough: [304], headers: headers)
            if response.status == 304, let cached {
                return finish(task, status: 200, data: cached, type: type)
            }
            try? FileManager.default.createDirectory(at: Self.cacheDirectory, withIntermediateDirectories: true)
            try? response.data.write(to: file, options: .atomic)
            if let etag = response.headers["etag"] { try? etag.write(to: tagFile, atomically: true, encoding: .utf8) }
            finish(task, status: 200, data: response.data, type: response.headers["content-type"] ?? type)
        } catch {
            if let cached, APIError.wrap(error).isConnectivity {
                return finish(task, status: 200, data: cached, type: type) // offline: the last model we had
            }
            finish(task, status: APIError.wrap(error).status == 0 ? 502 : APIError.wrap(error).status, data: Data(), type: "text/plain")
        }
    }

    private func finish(_ task: any WKURLSchemeTask, status: Int, data: Data, type: String) {
        guard !stopped.contains(ObjectIdentifier(task)), let url = task.request.url else { return }
        let headers = ["Content-Type": type, "Content-Length": String(data.count), "Cache-Control": "no-store"]
        guard let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers) else { return }
        task.didReceive(response)
        task.didReceive(data)
        task.didFinish()
    }
}

/// Native speech-to-text for one turn: starts on tap, ends after a short silence.
@MainActor
final class VoiceInput {
    enum VoiceError: LocalizedError {
        case notAuthorized, unavailable
        var errorDescription: String? {
            switch self {
            case .notAuthorized: return String(localized: "Allow the microphone and speech recognition in Settings to talk to Lucina.")
            case .unavailable: return String(localized: "Speech recognition isn't available right now.")
            }
        }
    }

    private let recognizer = SFSpeechRecognizer(locale: Locale(identifier: "it-IT"))
    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var silence: Task<Void, Never>?
    private var finished: ((String) -> Void)?
    private(set) var transcript = ""
    var onPartial: ((String) -> Void)?

    static func authorize() async -> Bool {
        let speech = await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
        }
        guard speech else { return false }
        return await AVAudioApplication.requestRecordPermission()
    }

    func start(onFinish: @escaping (String) -> Void) throws {
        guard let recognizer, recognizer.isAvailable else { throw VoiceError.unavailable }
        stop(deliver: false)
        transcript = ""
        finished = onFinish
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker, .duckOthers, .allowBluetoothHFP])
        try session.setActive(true, options: .notifyOthersOnDeactivation)
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        if recognizer.supportsOnDeviceRecognition { request.requiresOnDeviceRecognition = false }
        self.request = request
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in request.append(buffer) }
        engine.prepare()
        try engine.start()
        task = recognizer.recognitionTask(with: request) { [weak self] result, error in
            Task { @MainActor in
                guard let self else { return }
                if let result {
                    self.transcript = result.bestTranscription.formattedString
                    self.onPartial?(self.transcript)
                    if result.isFinal { self.stop(deliver: true); return }
                    self.armSilenceTimer()
                }
                if error != nil { self.stop(deliver: true) }
            }
        }
        armSilenceTimer(initial: true)
    }

    private func armSilenceTimer(initial: Bool = false) {
        silence?.cancel()
        silence = Task { [weak self] in
            try? await Task.sleep(for: .seconds(initial ? 6 : 1.6))
            guard !Task.isCancelled else { return }
            self?.stop(deliver: true)
        }
    }

    /// Ends the turn; with `deliver`, hands the transcript to `onFinish`.
    func stop(deliver: Bool) {
        silence?.cancel()
        silence = nil
        if engine.isRunning {
            engine.stop()
            engine.inputNode.removeTap(onBus: 0)
        }
        request?.endAudio()
        task?.cancel()
        request = nil
        task = nil
        let callback = finished
        finished = nil
        if deliver { callback?(transcript.trimmingCharacters(in: .whitespacesAndNewlines)) }
    }
}

/// Owns the one WKWebView, the bridge to the stage, mute, and the voice turn.
@Observable
@MainActor
final class LucinaStageController {
    enum Phase: Equatable { case idle, loading, ready, failed(String) }
    enum Framing: String { case bust, full }

    static let mutedKey = "secretary.lucina.muted"
    static let enabledKey = "secretary.lucina.enabled"

    private(set) var phase: Phase = .idle
    private(set) var placeholder = false
    /// idle, listening, thinking, speaking (from the stage).
    private(set) var stageState = "idle"
    private(set) var listening = false
    private(set) var thinking = false
    private(set) var transcript = ""
    /// The stage is loading or playing an outfit change.
    private(set) var changingOutfit = false
    var message: String?
    var expanded = false

    /// Muted by default; the choice persists. Nothing plays while muted.
    var muted: Bool {
        didSet {
            UserDefaults.standard.set(muted, forKey: Self.mutedKey)
            run("lucina.setMuted(\(muted))")
            if muted { run("lucina.stop()") }
        }
    }

    var enabled: Bool {
        didSet { UserDefaults.standard.set(enabled, forKey: Self.enabledKey) }
    }

    @ObservationIgnored private var webViewStorage: WKWebView?
    @ObservationIgnored private var schemeHandler: LucinaSchemeHandler?
    @ObservationIgnored private var framing: Framing = .bust
    /// The wardrobe and the look she should wear (JSON for the page).
    @ObservationIgnored private var wardrobeJSON = "{}"
    @ObservationIgnored private var lookJSON = "{}"
    @ObservationIgnored private let voice = VoiceInput()
    @ObservationIgnored let client: APIClient
    /// While set (the AR view is open), her speech goes here instead of the web stage.
    @ObservationIgnored var speechSink: ((Data) -> Void)?
    @ObservationIgnored private let chat: ChatStore

    init(client: APIClient, chat: ChatStore) {
        self.client = client
        self.chat = chat
        let defaults = UserDefaults.standard
        muted = defaults.object(forKey: Self.mutedKey) as? Bool ?? true
        enabled = defaults.object(forKey: Self.enabledKey) as? Bool ?? true
    }

    /// The shared web view, created on first use.
    var webView: WKWebView {
        if let webViewStorage { return webViewStorage }
        let configuration = WKWebViewConfiguration()
        let handler = LucinaSchemeHandler(client: client)
        configuration.setURLSchemeHandler(handler, forURLScheme: LucinaSchemeHandler.scheme)
        configuration.userContentController.add(MessageProxy(owner: self), name: "lucina")
        configuration.allowsInlineMediaPlayback = true
        // Audio only ever starts from a speak() the app makes after the owner unmuted.
        configuration.mediaTypesRequiringUserActionForPlayback = []
        configuration.websiteDataStore = .nonPersistent()
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.isOpaque = false
        view.backgroundColor = .clear
        view.scrollView.backgroundColor = .clear
        view.scrollView.isScrollEnabled = false
        view.scrollView.contentInsetAdjustmentBehavior = .never
        // The stage handles every gesture itself (orbit, pinch, taps): no WebKit zoom or bounce.
        view.scrollView.bounces = false
        view.scrollView.alwaysBounceVertical = false
        view.scrollView.alwaysBounceHorizontal = false
        view.scrollView.minimumZoomScale = 1
        view.scrollView.maximumZoomScale = 1
        view.scrollView.pinchGestureRecognizer?.isEnabled = false
        view.scrollView.delaysContentTouches = false
        view.allowsLinkPreview = false
        view.accessibilityLabel = "Lucina"
        #if DEBUG
        if #available(iOS 16.4, *) { view.isInspectable = true }
        #endif
        schemeHandler = handler
        webViewStorage = view
        phase = .loading
        view.load(URLRequest(url: URL(string: LucinaSchemeHandler.base + "stage.html")!))
        return view
    }

    /// Hands the wardrobe to the stage and dresses her: outfit, hairstyle, hair colour (with the
    /// change effect once she is on stage; before that, she simply starts in it).
    func dress(_ wardrobe: WardrobeFile, outfit: String?, hair: String, hairColor: String?) {
        let file: [String: Any] = [
            "outfits": wardrobe.outfits.map { outfit -> [String: Any] in
                var entry: [String: Any] = ["id": outfit.id, "glb": outfit.glb, "name": outfit.names]
                if let manifest = outfit.manifest { entry["manifest"] = manifest }
                if !outfit.tags.isEmpty { entry["tags"] = outfit.tags }
                return entry
            },
            "hairstyles": wardrobe.hairstyles.map { hair -> [String: Any] in
                var entry: [String: Any] = ["id": hair.id, "name": hair.names, "hideTiara": hair.hideTiara]
                if let glb = hair.glb { entry["glb"] = glb }
                if !hair.hideMaterials.isEmpty { entry["hideMaterials"] = hair.hideMaterials }
                return entry
            },
            "colors": wardrobe.colors.map { ["id": $0.id, "hex": $0.hex, "name": $0.names] as [String: Any] },
        ]
        let look: [String: Any] = ["outfit": outfit as Any, "hair": hair, "hairColor": hairColor as Any]
        let encode: (Any) -> String = { (try? JSONSerialization.data(withJSONObject: $0)).flatMap { String(data: $0, encoding: .utf8) } ?? "{}" }
        let (w, l) = (encode(file), encode(look))
        guard w != wardrobeJSON || l != lookJSON else { return }
        wardrobeJSON = w
        lookJSON = l
        run("lucina.dress(\(w), \(l))")
    }

    func setFraming(_ framing: Framing) {
        guard framing != self.framing else { return }
        self.framing = framing
        run("lucina.setFraming('\(framing.rawValue)'); lucina.resize()")
    }

    fileprivate func receive(_ body: [String: Any]) {
        switch body["type"] as? String {
        case "loaded":
            let options: [String: Any] = [
                "modelUrl": LucinaSchemeHandler.base + "v1/assets/lucina.glb",
                "manifestUrl": LucinaSchemeHandler.base + "v1/assets/manifest.json",
                "framing": framing.rawValue,
                "muted": muted,
                "reducedMotion": UIAccessibility.isReduceMotionEnabled,
                "assetBase": LucinaSchemeHandler.base + "v1/assets/",
            ]
            if let data = try? JSONSerialization.data(withJSONObject: options), var json = String(data: data, encoding: .utf8) {
                // The wardrobe and the look are already JSON: splice them in.
                json = String(json.dropLast()) + ",\"wardrobe\":\(wardrobeJSON),\"look\":\(lookJSON)}"
                run("lucina.start(\(json))")
            }
        case "ready":
            placeholder = body["placeholder"] as? Bool ?? false
            phase = .ready
            runDemoIfAsked()
            restoreStudio()
            // The look may have changed while she was loading.
            run("lucina.dress(\(wardrobeJSON), \(lookJSON))")
        case "state":
            stageState = body["state"] as? String ?? "idle"
        case "spoken":
            if !listening && !thinking { run("lucina.setState('idle')") }
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        case "error":
            let text = body["message"] as? String ?? "unknown error"
            if body["fatal"] as? Bool == true { phase = .failed(text) }
        case "changing":
            changingOutfit = body["changing"] as? Bool ?? false
        case "touch":
            // She already reacted on stage; a short line only when her voice is on and she is free.
            if !muted, !listening, !thinking, stageState != "speaking", let key = body["line"] as? String, let line = Self.touchLine(key) {
                Task { await say(line) }
            }
        case "photo":
            if let url = body["dataUrl"] as? String { sharePhoto(dataURL: url) }
        case "stats", "demo":
            Self.log.notice("stage \(String(describing: body), privacy: .public)")
        default:
            break
        }
    }

    static let log = Logger(subsystem: "tech.getsecretary.lucina", category: "stage")

    /// Lines for touch reactions (keys from lucina-stage interact/reactions.ts).
    static func touchLine(_ key: String) -> String? {
        switch key {
        case "head": return String(localized: "Hm? Yes?")
        case "face": return String(localized: "Ah! My face…")
        case "chest": return String(localized: "Hey! Hands to yourself.")
        case "hips": return String(localized: "W-what are you doing?")
        case "skirt": return String(localized: "Not the skirt!")
        case "arm": return String(localized: "Mm? Need something?")
        case "hand": return String(localized: "Hi there!")
        case "leg": return String(localized: "Eek! That tickles.")
        case "pat": return String(localized: "Ehehe…")
        case "pat_more": return String(localized: "That feels nice.")
        case "pat_love": return String(localized: "I love head pats!")
        case "enough": return String(localized: "Okay, that's enough now.")
        default: return nil
        }
    }

    /// Device check without a user: `-lucinaDemo` opens the stage and runs scripted gestures.
    private func runDemoIfAsked() {
        guard ProcessInfo.processInfo.arguments.contains("-lucinaDemo") else { return }
        expanded = true
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(4))
            run("lucina.demo()")
            try? await Task.sleep(for: .seconds(32))
            run("lucina.stats()")
        }
    }

    func run(_ script: String) {
        guard let webViewStorage, phase == .ready || script.hasPrefix("lucina.start") else { return }
        webViewStorage.evaluateJavaScript(script, completionHandler: nil)
    }

    // MARK: - Voice turn

    func toggleListening() async {
        if listening {
            voice.stop(deliver: true)
            return
        }
        guard await VoiceInput.authorize() else {
            message = VoiceInput.VoiceError.notAuthorized.errorDescription
            return
        }
        run("lucina.stop()")
        do {
            transcript = ""
            voice.onPartial = { [weak self] text in
                self?.transcript = text
                self?.run("lucina.nod(0.3)")
            }
            try voice.start { [weak self] text in
                Task { await self?.finishTurn(text) }
            }
            listening = true
            message = nil
            run("lucina.setState('listening')")
        } catch {
            message = error.localizedDescription
        }
    }

    private func finishTurn(_ text: String) async {
        listening = false
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        guard !text.isEmpty else {
            run("lucina.setState('idle')")
            return
        }
        thinking = true
        run("lucina.setState('thinking')")
        let reply = await chat.ask(text)
        thinking = false
        guard let reply, !reply.isEmpty else {
            run("lucina.setState('idle')")
            message = reply == nil ? String(localized: "Lucina is busy with another message.") : String(localized: "That message wasn't delivered; see the chat.")
            return
        }
        await say(reply)
    }

    /// Speaks through the stage, only when the sound is on.
    func say(_ text: String) async {
        guard !muted, speechSink != nil || (phase == .ready && schemeHandler != nil) else {
            run("lucina.setState('idle')")
            return
        }
        do {
            let audio = try await SecretaryAPI(client: client).tts(text)
            guard !muted else { return }
            if let speechSink {
                run("lucina.setState('idle')")
                speechSink(audio)
                return
            }
            guard let schemeHandler else { return }
            try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .spokenAudio, options: [.duckOthers])
            try? AVAudioSession.sharedInstance().setActive(true)
            let url = schemeHandler.stash(audio)
            run("lucina.speak('\(url)')")
        } catch {
            run("lucina.setState('idle')")
            let apiError = APIError.wrap(error)
            message = apiError.code == "tts_unavailable" ? String(localized: "Lucina's voice isn't available right now.") : apiError.userMessage
        }
    }

    func stopEverything() {
        voice.stop(deliver: false)
        listening = false
        run("lucina.stop(); lucina.setState('idle')")
    }
}

/// Avoids the WKUserContentController → handler retain cycle.
private final class MessageProxy: NSObject, WKScriptMessageHandler {
    weak var owner: LucinaStageController?

    init(owner: LucinaStageController) {
        self.owner = owner
    }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let body = message.body as? [String: Any] else { return }
        MainActor.assumeIsolated { owner?.receive(body) }
    }
}

// MARK: - Views

/// Hosts the shared web view; whichever host is on screen owns it.
struct StageHost: UIViewRepresentable {
    let controller: LucinaStageController
    let framing: LucinaStageController.Framing

    func makeUIView(context: Context) -> UIView {
        let container = UIView()
        container.backgroundColor = .clear
        container.isAccessibilityElement = false
        return container
    }

    func updateUIView(_ container: UIView, context: Context) {
        let webView = controller.webView
        if webView.superview !== container {
            webView.removeFromSuperview()
            webView.frame = container.bounds
            webView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
            container.addSubview(webView)
        }
        controller.setFraming(framing)
    }
}

/// Mute, talk and status, shared by the expanded panel and the side column.
struct LucinaControls: View {
    @Bindable var controller: LucinaStageController
    @Environment(WardrobeStore.self) private var wardrobe

    var body: some View {
        VStack(spacing: 10) {
            LucinaFaceBadge(tracker: LucinaFaceTracker.shared)
            if let message = LucinaFaceTracker.shared.message, LucinaFaceTracker.shared.state == .off {
                Text(message).font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
            if let message = controller.message {
                Text(message).font(.footnote).foregroundStyle(.secondary).multilineTextAlignment(.center)
            } else if controller.listening {
                Text(controller.transcript.isEmpty ? "Listening…" : controller.transcript)
                    .font(.subheadline).multilineTextAlignment(.center).lineLimit(3)
            } else if controller.thinking {
                Text("Lucina is thinking…").font(.subheadline).foregroundStyle(.secondary)
            } else if controller.changingOutfit {
                Text("Changing…").font(.subheadline).foregroundStyle(.secondary)
            } else if case let .failed(reason) = controller.phase {
                Text("Lucina couldn't load: \(reason)").font(.footnote).foregroundStyle(.secondary).lineLimit(2)
            }
            HStack(spacing: 18) {
                Button {
                    wardrobe.presented = true
                } label: {
                    Image(systemName: "hanger")
                        .font(.title3)
                        .frame(width: 52, height: 52)
                        .background(.thinMaterial, in: Circle())
                }
                .accessibilityLabel("Lucina's wardrobe")
                .accessibilityIdentifier("lucinaWardrobe")

                LucinaStudioMenu(controller: controller)

                Button {
                    controller.muted.toggle()
                } label: {
                    Image(systemName: controller.muted ? "speaker.slash.fill" : "speaker.wave.2.fill")
                        .font(.title3)
                        .frame(width: 52, height: 52)
                        .background(.thinMaterial, in: Circle())
                }
                .accessibilityLabel("Lucina's voice")
                .accessibilityValue(controller.muted ? "Muted" : "Sound on")
                .accessibilityIdentifier("lucinaMute")

                Button {
                    Task { await controller.toggleListening() }
                } label: {
                    Image(systemName: controller.listening ? "stop.fill" : "mic.fill")
                        .font(.title2)
                        .foregroundStyle(.white)
                        .frame(width: 68, height: 68)
                        .background(controller.listening ? Color.red.gradient : Theme.accentFill.gradient, in: Circle())
                }
                .disabled(controller.thinking)
                .accessibilityLabel(controller.listening ? String(localized: "Stop listening") : String(localized: "Talk to Lucina"))
                .accessibilityIdentifier("lucinaMic")
            }
        }
    }
}

/// iPhone portrait: a small draggable Lucina over every tab; tap for full body.
struct LucinaOverlay: View {
    @Bindable var controller: LucinaStageController
    @AppStorage("secretary.lucina.bubbleX") private var savedX: Double = -1
    @AppStorage("secretary.lucina.bubbleY") private var savedY: Double = -1
    @GestureState private var drag: CGSize = .zero

    private let size: CGFloat = 86

    var body: some View {
        GeometryReader { proxy in
            ZStack {
                if controller.expanded {
                    expanded
                        .transition(.opacity.combined(with: .scale(scale: 0.96)))
                } else {
                    bubble(in: proxy.size)
                }
            }
            .animation(.spring(duration: 0.3), value: controller.expanded)
        }
        // Face tracking only runs while her full view is open.
        .onChange(of: controller.expanded) { _, expanded in
            if !expanded { LucinaFaceTracker.shared.stop() }
        }
    }

    private func position(in area: CGSize) -> CGPoint {
        let x = savedX < 0 ? area.width - size / 2 - 14 : savedX
        let y = savedY < 0 ? area.height - size / 2 - 140 : savedY
        return CGPoint(x: x + drag.width, y: y + drag.height)
    }

    private func bubble(in area: CGSize) -> some View {
        StageHost(controller: controller, framing: .bust)
            .allowsHitTesting(false)
            .frame(width: size, height: size)
            .background(.ultraThinMaterial)
            .clipShape(Circle())
            .overlay(Circle().strokeBorder(stateColor, lineWidth: controller.stageState == "idle" ? 1 : 3))
            .shadow(color: .black.opacity(0.2), radius: 8, y: 3)
            .contentShape(Circle())
            .position(position(in: area))
            .gesture(
                DragGesture(minimumDistance: 6)
                    .updating($drag) { value, state, _ in state = value.translation }
                    .onEnded { value in
                        let start = position(in: area)
                        let end = CGPoint(x: start.x + value.translation.width - drag.width, y: start.y + value.translation.height - drag.height)
                        // Snap to the nearer side, keep it on screen.
                        savedX = end.x < area.width / 2 ? size / 2 + 10 : area.width - size / 2 - 10
                        savedY = min(max(end.y, size / 2 + 50), area.height - size / 2 - 90)
                    }
            )
            .onTapGesture { controller.expanded = true }
            .accessibilityElement()
            .accessibilityLabel("Lucina")
            .accessibilityHint("Opens Lucina full screen")
            .accessibilityAddTraits(.isButton)
            .accessibilityIdentifier("lucinaBubble")
    }

    private var expanded: some View {
        ZStack(alignment: .top) {
            Rectangle().fill(.regularMaterial).ignoresSafeArea()
            VStack(spacing: 12) {
                HStack {
                    Spacer()
                    Button {
                        controller.expanded = false
                    } label: {
                        Image(systemName: "xmark").font(.headline).frame(width: 40, height: 40).background(.thinMaterial, in: Circle())
                    }
                    .accessibilityLabel("Close Lucina")
                    .accessibilityIdentifier("lucinaClose")
                }
                .padding(.horizontal, 16)
                StageHost(controller: controller, framing: .full)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityIdentifier("lucinaStage")
                LucinaViewBar(controller: controller)
                LucinaControls(controller: controller)
                    .padding(.bottom, 24)
            }
        }
    }

    private var stateColor: Color {
        switch controller.stageState {
        case "listening": return .red
        case "thinking": return .orange
        case "speaking": return Theme.accent
        default: return .white.opacity(0.6)
        }
    }
}

/// iPad or landscape: a full-body column beside the tabs.
struct LucinaColumn: View {
    @Bindable var controller: LucinaStageController

    var body: some View {
        VStack(spacing: 12) {
            StageHost(controller: controller, framing: .full)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityIdentifier("lucinaStage")
            LucinaViewBar(controller: controller)
            LucinaControls(controller: controller)
                .padding(.bottom, 16)
        }
        .padding(.top, 8)
        .background(Theme.page)
        .onDisappear { LucinaFaceTracker.shared.stop() }
    }
}
