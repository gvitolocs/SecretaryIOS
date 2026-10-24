import ARKit
import AVFoundation
import Combine
import Observation
import QuickLook
import RealityKit
import SwiftUI

// Lucina in the room (Custom Cast A1–A3) with RealityKit: find the floor, tap to place her, pinch to
// resize, twist to turn, drag to move, two-finger tap to reset. She plays her idle clip, reacts to
// taps with a short clip (and a line when her voice is on), and speaks replies through the usual
// pipeline (/v1/tts), played here with her mouth moving. Devices without world tracking, or the
// owner's choice, get the 3-DoF mode: she stands in front of the camera and only rotation is tracked.
//
// The model is private: `lucina.usdz` and its clip table `lucina-ar.json` come from /v1/assets with the
// device token (built from the live GLB by tools/lucina-usdz on nezopt), cached on disk, never
// bundled. The toon look: every material is turned unlit (flat albedo, like her toon base colour),
// and the USDZ carries an inverted-hull outline. AR Quick Look of the same file is the fallback.

/// The private AR files from /v1/assets, cached next to the stage's model by ETag.
enum LucinaARAssets {
    struct Clip: Decodable {
        let name: String
        let start: Double
        let end: Double
        let loop: Bool
        let tags: [String]?
    }

    struct Table: Decodable {
        let clips: [Clip]
    }

    static var directory: URL {
        let base = (try? FileManager.default.url(for: .cachesDirectory, in: .userDomainMask, appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("lucina-assets", isDirectory: true)
    }

    /// A local copy of `name`, refreshed with If-None-Match; offline, the cached copy.
    static func file(_ name: String, client: APIClient) async throws -> URL {
        let file = directory.appendingPathComponent(name)
        let tagFile = directory.appendingPathComponent(name + ".etag")
        let cached = FileManager.default.fileExists(atPath: file.path)
        let tag = cached ? (try? String(contentsOf: tagFile, encoding: .utf8)) : nil
        do {
            var headers: [String: String] = [:]
            if let tag { headers["If-None-Match"] = tag }
            let response = try await client.send("GET", "/v1/assets/\(name)", body: nil, timeout: 300, passThrough: [304], headers: headers)
            if response.status == 304, cached { return file }
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try response.data.write(to: file, options: .atomic)
            if let etag = response.headers["etag"] { try? etag.write(to: tagFile, atomically: true, encoding: .utf8) }
            return file
        } catch {
            if cached, APIError.wrap(error).isConnectivity { return file }
            throw error
        }
    }
}

/// Opens the AR view (from the studio menu); one per app.
@Observable
@MainActor
final class LucinaARPresenter {
    static let shared = LucinaARPresenter()
    var presented = false

    static var isSupported: Bool { ARWorldTrackingConfiguration.isSupported || AROrientationTrackingConfiguration.isSupported }
}

@Observable
@MainActor
final class LucinaARModel {
    enum Phase: Equatable { case loading, finding, placing, placed, failed(String) }

    var phase: Phase = .loading
    /// 3-DoF: rotation only, she stands in front of the camera (A3).
    var orientationOnly = !ARWorldTrackingConfiguration.isSupported
    var usdzURL: URL?
    var quickLook = false
}

@MainActor
final class LucinaARCoordinator: NSObject {
    let model = LucinaARModel()
    let arView: ARView
    private unowned let controller: LucinaStageController
    /// Gestures and taps act on this: it holds her and a collision box around her.
    private let root = ModelEntity()
    private var her: Entity?
    private var anchor: AnchorEntity?
    private var animated: Entity?
    private var source: (any AnimationDefinition)?
    private var clips: [String: LucinaARAssets.Clip] = [:]
    private var resources: [String: AnimationResource] = [:]
    private var reaction: Task<Void, Never>?
    private var lastReaction = ""
    private var updates: (any Cancellable)?
    private let coaching = ARCoachingOverlayView()
    private var player: AVAudioPlayer?
    private var mouth: Float = 0
    private var blinkClock: Double = 0
    private var nextBlink: Double = 2.5
    /// Blend shapes of her merged mesh (iOS 18+): the entity and the weight indices by name.
    private var shapeEntity: Entity?
    private var shapeIndex: [String: Int] = [:]
    private var started = false

    static let reactions: [(clip: String, line: String)] = [
        ("greet_wave", "hand"),
        ("happy_bounce", "pat"),
        ("nod_yes", "head"),
        ("cheer", "pat_love"),
        ("surprised_step", "leg"),
    ]

    init(controller: LucinaStageController) {
        self.controller = controller
        arView = ARView(frame: .zero, cameraMode: .ar, automaticallyConfigureSession: false)
        super.init()
        arView.renderOptions.formUnion([.disableMotionBlur, .disableDepthOfField, .disableCameraGrain])
        arView.session.delegate = self
        coaching.session = arView.session
        coaching.goal = .horizontalPlane
        coaching.activatesAutomatically = true
        coaching.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        arView.addSubview(coaching)
        let tap = UITapGestureRecognizer(target: self, action: #selector(tapped(_:)))
        arView.addGestureRecognizer(tap)
        let reset = UITapGestureRecognizer(target: self, action: #selector(resetTapped))
        reset.numberOfTouchesRequired = 2
        arView.addGestureRecognizer(reset)
        tap.require(toFail: reset)
    }

    func start() async {
        guard !started else { return }
        started = true
        LucinaFaceTracker.shared.stop() // one camera session at a time
        let granted: Bool
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: granted = true
        case .notDetermined: granted = await AVCaptureDevice.requestAccess(for: .video)
        default: granted = false
        }
        guard granted else {
            model.phase = .failed(String(localized: "Allow the camera for Secretary in Settings to place Lucina in your room."))
            return
        }
        runSession()
        // Replies and touch lines play here while AR is open.
        controller.speechSink = { [weak self] audio in self?.speak(audio) }
        updates = arView.scene.subscribe(to: SceneEvents.Update.self) { [weak self] event in
            MainActor.assumeIsolated { self?.tick(event.deltaTime) }
        }
        do {
            async let usdz = LucinaARAssets.file("lucina.usdz", client: controller.client)
            async let table = LucinaARAssets.file("lucina-ar.json", client: controller.client)
            let (modelURL, tableURL) = try await (usdz, table)
            model.usdzURL = modelURL
            let decoded = try JSONDecoder().decode(LucinaARAssets.Table.self, from: Data(contentsOf: tableURL))
            clips = Dictionary(decoded.clips.map { ($0.name, $0) }, uniquingKeysWith: { a, _ in a })
            try await load(modelURL)
        } catch {
            let apiError = APIError.wrap(error)
            model.phase = .failed(apiError.status == 404
                ? String(localized: "Lucina's AR model isn't on the server yet.")
                : String(localized: "Lucina couldn't load: \(apiError.userMessage)"))
            return
        }
        if model.orientationOnly {
            placeInFront()
        } else if model.phase == .loading {
            model.phase = .finding
        }
    }

    func stop() {
        reaction?.cancel()
        updates?.cancel()
        updates = nil
        player?.stop()
        player = nil
        controller.speechSink = nil
        arView.session.pause()
        arView.scene.anchors.removeAll()
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    // MARK: - Session

    private func runSession() {
        if !model.orientationOnly, ARWorldTrackingConfiguration.isSupported {
            let configuration = ARWorldTrackingConfiguration()
            configuration.planeDetection = [.horizontal]
            configuration.environmentTexturing = .none
            // People walking in front of her hide her.
            if ARWorldTrackingConfiguration.supportsFrameSemantics(.personSegmentationWithDepth) {
                configuration.frameSemantics.insert(.personSegmentationWithDepth)
            }
            arView.session.run(configuration, options: [.resetTracking, .removeExistingAnchors])
            coaching.setActive(true, animated: true)
        } else {
            model.orientationOnly = true
            arView.session.run(AROrientationTrackingConfiguration(), options: [.resetTracking, .removeExistingAnchors])
            coaching.setActive(false, animated: false)
        }
    }

    /// Switches between the room (6-DoF, on the floor) and 3-DoF (in front of the camera).
    func setOrientationOnly(_ on: Bool) {
        guard on != model.orientationOnly, on || ARWorldTrackingConfiguration.isSupported else { return }
        model.orientationOnly = on
        anchor?.removeFromParent()
        anchor = nil
        runSession()
        if her != nil {
            if on { placeInFront() } else { model.phase = .finding }
        }
    }

    // MARK: - Model

    private func load(_ url: URL) async throws {
        let entity: Entity
        if #available(iOS 18.0, *) {
            entity = try await Entity(contentsOf: url)
        } else {
            entity = try Self.loadBlocking(url)
        }
        Self.makeToon(entity)
        her = entity
        root.addChild(entity)
        let bounds = entity.visualBounds(relativeTo: root)
        root.collision = CollisionComponent(shapes: [ShapeResource.generateBox(size: bounds.extents).offsetBy(translation: bounds.center)])
        arView.installGestures([.rotation, .scale, .translation], for: root)
        animated = Self.find(entity) { !$0.availableAnimations.isEmpty }
        source = animated?.availableAnimations.first?.definition
        if #available(iOS 18.0, *) {
            setUpBlendShapes(entity)
            if let mesh = Self.find(entity, { $0.components[ModelComponent.self] != nil }) {
                mesh.components.set(GroundingShadowComponent(castsShadow: true))
            }
        }
        playIdle()
    }

    /// iOS 17 has no async loader for a file URL.
    private static func loadBlocking(_ url: URL) throws -> Entity {
        try Entity.load(contentsOf: url)
    }

    /// Unlit materials: her toon base colours without the room's lighting (RealityKit has no ramp shading).
    static func makeToon(_ entity: Entity) {
        if var model = entity.components[ModelComponent.self] {
            model.materials = model.materials.map(unlit)
            entity.components.set(model)
        }
        for child in entity.children { makeToon(child) }
    }

    static func unlit(_ material: any RealityKit.Material) -> any RealityKit.Material {
        guard let pbr = material as? PhysicallyBasedMaterial else { return material }
        var flat = UnlitMaterial()
        if #available(iOS 18.0, *) {
            // Without the post-process tone map: it pulled her cream skin to a flat grey (owner, 2026-10-24).
            flat = UnlitMaterial(applyPostProcessToneMap: false)
        }
        flat.color = .init(tint: pbr.baseColor.tint, texture: pbr.baseColor.texture)
        flat.blending = pbr.blending
        flat.opacityThreshold = pbr.opacityThreshold
        if #available(iOS 18.0, *) {
            // The outline shell is drawn inside out: keep back-face culling.
            flat.faceCulling = pbr.faceCulling
        }
        return flat
    }

    static func find(_ entity: Entity, _ match: (Entity) -> Bool) -> Entity? {
        if match(entity) { return entity }
        for child in entity.children {
            if let found = find(child, match) { return found }
        }
        return nil
    }

    @available(iOS 18.0, *)
    private func setUpBlendShapes(_ entity: Entity) {
        guard let target = Self.find(entity, { $0.components[ModelComponent.self] != nil }),
              let mesh = target.components[ModelComponent.self]?.mesh else { return }
        let component = BlendShapeWeightsComponent(weightsMapping: BlendShapeWeightsMapping(meshResource: mesh))
        guard let set = component.weightSet.first else { return }
        for (i, name) in set.weightNames.enumerated() {
            for key in ["mouth_a", "blink"] where name == key || name.hasSuffix("/" + key) {
                shapeIndex[key] = i
            }
        }
        guard !shapeIndex.isEmpty else { return }
        target.components.set(component)
        shapeEntity = target
    }

    // MARK: - Clips

    private func resource(_ name: String) -> AnimationResource? {
        if let cached = resources[name] { return cached }
        guard let source, let clip = clips[name] else { return nil }
        let view = AnimationView(source: source, name: name, repeatMode: clip.loop ? .repeat : .none,
                                 trimStart: clip.start, trimEnd: clip.end)
        guard let generated = try? AnimationResource.generate(with: view) else { return nil }
        resources[name] = generated
        return generated
    }

    private func playIdle() {
        let idle = clips.values.first { $0.tags?.contains("idle") == true && $0.loop }?.name ?? "idle_breathe"
        if let animated, let res = resource(idle) {
            animated.playAnimation(res, transitionDuration: 0.35, startsPaused: false)
        } else if let animated, let any = animated.availableAnimations.first {
            animated.playAnimation(any.repeat(), transitionDuration: 0.35, startsPaused: false)
        }
    }

    /// A one-shot clip, then back to idle.
    private func play(_ name: String) {
        guard let animated, let res = resource(name), let clip = clips[name] else { return }
        reaction?.cancel()
        animated.playAnimation(res, transitionDuration: 0.25, startsPaused: false)
        let length = max(0.4, clip.end - clip.start - 0.3)
        reaction = Task { [weak self] in
            try? await Task.sleep(for: .seconds(length))
            guard !Task.isCancelled else { return }
            self?.playIdle()
        }
    }

    private func react() {
        let options = Self.reactions.filter { clips[$0.clip] != nil && $0.clip != lastReaction }
        guard let pick = options.randomElement() else { return }
        lastReaction = pick.clip
        play(pick.clip)
        if !controller.muted, let line = LucinaStageController.touchLine(pick.line) {
            Task { await controller.say(line) }
        }
    }

    // MARK: - Placement

    private func place(at transform: simd_float4x4) {
        guard her != nil else { return }
        let position = SIMD3<Float>(transform.columns.3.x, transform.columns.3.y, transform.columns.3.z)
        let first = anchor == nil
        if let anchor {
            anchor.position = position
        } else {
            let anchor = AnchorEntity(world: position)
            anchor.addChild(root)
            arView.scene.addAnchor(anchor)
            self.anchor = anchor
        }
        root.position = .zero
        faceCamera()
        model.phase = .placed
        if first { play("greet_wave") }
    }

    /// 3-DoF: on the floor about 1.7 m in front of where the phone pointed, the floor guessed 1.3 m below it.
    private func placeInFront() {
        guard her != nil else { return }
        anchor?.removeFromParent()
        let anchor = AnchorEntity(world: SIMD3<Float>(0, -1.3, -1.7))
        anchor.addChild(root)
        arView.scene.addAnchor(anchor)
        self.anchor = anchor
        root.position = .zero
        root.scale = .one
        faceCamera()
        model.phase = .placed
        play("greet_wave")
    }

    /// Turns her (about the vertical) toward the camera; she faces +Z.
    private func faceCamera() {
        let camera = arView.cameraTransform.translation
        let here = root.position(relativeTo: nil)
        let yaw = atan2(camera.x - here.x, camera.z - here.z)
        root.setOrientation(simd_quatf(angle: yaw, axis: [0, 1, 0]), relativeTo: nil)
    }

    private func isHer(_ entity: Entity) -> Bool {
        var e: Entity? = entity
        while let current = e {
            if current === root { return true }
            e = current.parent
        }
        return false
    }

    @objc private func tapped(_ gesture: UITapGestureRecognizer) {
        let point = gesture.location(in: arView)
        if let hit = arView.entity(at: point), isHer(hit) {
            react()
            return
        }
        guard !model.orientationOnly, her != nil else { return }
        if let result = arView.raycast(from: point, allowing: .estimatedPlane, alignment: .horizontal).first {
            place(at: result.worldTransform)
        }
    }

    /// Two-finger tap: back to life size, facing you (Custom Cast's reset).
    @objc private func resetTapped() {
        resetPose()
    }

    func resetPose() {
        guard anchor != nil else { return }
        root.position = .zero
        root.scale = .one
        faceCamera()
    }

    // MARK: - Per frame

    private func tick(_ dt: Double) {
        // Custom Cast scales 0.01–3×; below 5 % she is lost on the floor.
        let s = root.scale.x
        if s < 0.05 || s > 3 { root.scale = SIMD3(repeating: min(max(s, 0.05), 3)) }
        var level: Float = 0
        if let player, player.isPlaying {
            player.updateMeters()
            let db = player.averagePower(forChannel: 0)
            level = min(max((db + 42) / 30, 0), 1)
        }
        mouth += (level - mouth) * Float(min(1, dt / 0.06))
        blinkClock += dt
        var blink: Float = 0
        if blinkClock >= nextBlink {
            let t = blinkClock - nextBlink
            blink = Float(t < 0.08 ? t / 0.08 : max(0, 1 - (t - 0.08) / 0.1))
            if t > 0.2 {
                blinkClock = 0
                nextBlink = Double.random(in: 2.2...5.5)
            }
        }
        if #available(iOS 18.0, *) { applyShapes(mouth: mouth, blink: blink) }
    }

    @available(iOS 18.0, *)
    private func applyShapes(mouth: Float, blink: Float) {
        guard let shapeEntity, var component = shapeEntity.components[BlendShapeWeightsComponent.self], !component.weightSet.isEmpty else { return }
        var data = component.weightSet[0]
        if let i = shapeIndex["mouth_a"] { data.weights[i] = mouth }
        if let i = shapeIndex["blink"] { data.weights[i] = blink }
        component.weightSet[0] = data
        shapeEntity.components.set(component)
    }

    // MARK: - Voice

    private func speak(_ audio: Data) {
        try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .spokenAudio, options: [.duckOthers])
        try? AVAudioSession.sharedInstance().setActive(true)
        player?.stop()
        player = try? AVAudioPlayer(data: audio)
        player?.isMeteringEnabled = true
        player?.play()
    }
}

extension LucinaARCoordinator: ARSessionDelegate {
    nonisolated func session(_ session: ARSession, didAdd anchors: [ARAnchor]) {
        guard anchors.contains(where: { $0 is ARPlaneAnchor }) else { return }
        MainActor.assumeIsolated {
            if self.model.phase == .finding { self.model.phase = .placing }
        }
    }

    nonisolated func session(_ session: ARSession, didFailWithError error: any Error) {
        MainActor.assumeIsolated {
            self.model.phase = .failed(String(localized: "AR stopped: \(error.localizedDescription)"))
        }
    }
}

private struct LucinaARViewContainer: UIViewRepresentable {
    let coordinator: LucinaARCoordinator

    func makeUIView(context: Context) -> ARView { coordinator.arView }
    func updateUIView(_ view: ARView, context: Context) {}
}

/// The full-screen AR view.
struct LucinaARScreen: View {
    @Bindable var controller: LucinaStageController
    /// Created once the screen is up (an ARView is not cheap; the struct may be re-created).
    @State private var coordinator: LucinaARCoordinator?
    @State private var placeholder = LucinaARModel()
    @Environment(\.dismiss) private var dismiss

    private var model: LucinaARModel { coordinator?.model ?? placeholder }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            if let coordinator {
                LucinaARViewContainer(coordinator: coordinator)
                    .ignoresSafeArea()
                    .accessibilityLabel("Lucina in your room")
            }
            VStack(spacing: 12) {
                topBar
                Spacer()
                if let hint {
                    Text(hint)
                        .font(.subheadline)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(.thinMaterial, in: Capsule())
                        .padding(.horizontal, 24)
                }
                if controller.listening {
                    Text(controller.transcript.isEmpty ? String(localized: "Listening…") : controller.transcript)
                        .font(.subheadline).lineLimit(2)
                        .padding(8).background(.thinMaterial, in: RoundedRectangle(cornerRadius: 12))
                } else if controller.thinking {
                    Text("Lucina is thinking…").font(.subheadline).padding(8).background(.thinMaterial, in: Capsule())
                } else if let message = controller.message {
                    Text(message).font(.footnote).padding(8).background(.thinMaterial, in: RoundedRectangle(cornerRadius: 12))
                }
                bottomBar
            }
            .padding(.bottom, 20)
        }
        .task {
            let current = coordinator ?? LucinaARCoordinator(controller: controller)
            coordinator = current
            await current.start()
        }
        .onDisappear {
            coordinator?.stop()
            controller.stopEverything()
        }
        .sheet(isPresented: Binding(get: { model.quickLook }, set: { model.quickLook = $0 })) {
            if let url = model.usdzURL { LucinaQuickLook(url: url).ignoresSafeArea() }
        }
    }

    private var hint: String? {
        switch model.phase {
        case .loading: return String(localized: "Loading Lucina…")
        case .finding: return String(localized: "Move your phone slowly to find the floor.")
        case .placing: return String(localized: "Tap the floor to place Lucina.")
        case .placed: return model.orientationOnly
            ? String(localized: "Tap her to say hi. Pinch to resize, twist to turn.")
            : String(localized: "Tap her to say hi. Pinch, twist or drag her; tap the floor to move her; two fingers reset.")
        case let .failed(reason): return reason
        }
    }

    private var topBar: some View {
        HStack {
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark").font(.headline).frame(width: 44, height: 44).background(.thinMaterial, in: Circle())
            }
            .accessibilityLabel("Close AR")
            .accessibilityIdentifier("lucinaARClose")
            Spacer()
            Menu {
                if ARWorldTrackingConfiguration.isSupported {
                    Button {
                        coordinator?.setOrientationOnly(!model.orientationOnly)
                    } label: {
                        Label(model.orientationOnly ? "On the floor (room tracking)" : "In front of me (3-DoF)",
                              systemImage: model.orientationOnly ? "cube.transparent" : "rotate.3d")
                    }
                }
                Button {
                    coordinator?.resetPose()
                } label: {
                    Label("Reset size and turn", systemImage: "arrow.counterclockwise")
                }
                Button {
                    model.quickLook = true
                } label: {
                    Label("Open in AR Quick Look", systemImage: "arkit")
                }
                .disabled(model.usdzURL == nil)
            } label: {
                Image(systemName: "ellipsis").font(.headline).frame(width: 44, height: 44).background(.thinMaterial, in: Circle())
            }
            .accessibilityLabel("AR options")
            .accessibilityIdentifier("lucinaAROptions")
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
    }

    private var bottomBar: some View {
        HStack(spacing: 18) {
            Button {
                controller.muted.toggle()
            } label: {
                Image(systemName: controller.muted ? "speaker.slash.fill" : "speaker.wave.2.fill")
                    .font(.title3).frame(width: 52, height: 52).background(.thinMaterial, in: Circle())
            }
            .accessibilityLabel("Lucina's voice")
            .accessibilityValue(controller.muted ? "Muted" : "Sound on")
            Button {
                Task { await controller.toggleListening() }
            } label: {
                Image(systemName: controller.listening ? "stop.fill" : "mic.fill")
                    .font(.title2).foregroundStyle(.white)
                    .frame(width: 68, height: 68)
                    .background(controller.listening ? Color.red.gradient : Theme.accentFill.gradient, in: Circle())
            }
            .disabled(controller.thinking)
            .accessibilityLabel(controller.listening ? String(localized: "Stop listening") : String(localized: "Talk to Lucina"))
        }
    }
}

/// AR Quick Look of the same private USDZ (Apple's viewer: lit, its own gestures; the fallback).
struct LucinaQuickLook: UIViewControllerRepresentable {
    let url: URL

    func makeCoordinator() -> Source { Source(url: url) }

    func makeUIViewController(context: Context) -> QLPreviewController {
        let preview = QLPreviewController()
        preview.dataSource = context.coordinator
        return preview
    }

    func updateUIViewController(_ controller: QLPreviewController, context: Context) {}

    final class Source: NSObject, QLPreviewControllerDataSource {
        let url: URL
        init(url: URL) { self.url = url }
        func numberOfPreviewItems(in controller: QLPreviewController) -> Int { 1 }
        func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> any QLPreviewItem {
            let item = ARQuickLookPreviewItem(fileAt: url)
            item.allowsContentScaling = true
            return item
        }
    }
}

/// Hosts the AR screen over the whole app (attach once, at the root).
struct LucinaARHost: ViewModifier {
    let controller: LucinaStageController
    @State private var presenter = LucinaARPresenter.shared

    func body(content: Content) -> some View {
        content.fullScreenCover(isPresented: $presenter.presented) {
            LucinaARScreen(controller: controller)
        }
    }
}
