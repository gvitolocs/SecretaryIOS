import AVFoundation
import CoreMotion
import SwiftUI
import UIKit

// Lucina's studio on iOS (the web's Studio sheet, as native menus over the shared stage):
// camera views, poses, expressions, backgrounds, light, photo (share sheet), dance (music only
// from that tap and only when her voice is on), and tilt parallax fed by CoreMotion.

@MainActor
final class LucinaStudioState {
    static let shared = LucinaStudioState()
    static let backgroundKey = "secretary.studio.background"
    static let lightingKey = "secretary.studio.lighting"

    let motion = CMMotionManager()
    var dancing = false
    var tilting = false
}

enum LucinaStudioOptions {
    static let backgrounds: [(id: String, name: LocalizedStringResource)] = [
        ("default", "Stage"),
        ("color:#101426", "Night"),
        ("color:#f4f1ec", "Paper"),
        ("color:#ffd6e5", "Pink"),
        ("color:#cfe8ff", "Sky"),
        ("room:cafe", "Café"),
        ("room:bedroom", "Bedroom"),
        ("room:rooftop", "Rooftop"),
    ]
    static let lights: [(id: String, name: LocalizedStringResource)] = [
        ("default", "Default"),
        ("studio", "Studio"),
        ("warm", "Warm"),
        ("cool", "Cool"),
        ("sunset", "Sunset"),
        ("night", "Night"),
    ]
    static let expressions: [(id: String?, name: LocalizedStringResource)] = [
        (nil, "Neutral"),
        ("smile", "Smile"),
        ("sad", "Sad"),
        ("surprised", "Surprised"),
        ("angry", "Angry"),
    ]
    /// Procedural poses (the iOS stage has no clip file): all of them look right.
    static let poses: [(id: String, name: LocalizedStringResource)] = [
        ("greet", "Wave"),
        ("happy", "Happy"),
        ("tilt", "Head tilt"),
        ("nod", "Nod"),
        ("think", "Think"),
        ("surprised", "Surprised"),
        ("giggle", "Giggle"),
    ]
}

extension LucinaStageController {
    func cameraPreset(_ preset: String) { run("lucina.cameraPreset('\(preset)')") }
    func resetView() { run("lucina.resetView()") }

    func setBackground(_ id: String) {
        UserDefaults.standard.set(id, forKey: LucinaStudioState.backgroundKey)
        run("lucina.setBackground('\(id)')")
    }

    func setLighting(_ id: String) {
        UserDefaults.standard.set(id, forKey: LucinaStudioState.lightingKey)
        run("lucina.setLighting('\(id)')")
    }

    /// Re-applies the remembered background and light (after the stage is ready).
    func restoreStudio() {
        let d = UserDefaults.standard
        if let bg = d.string(forKey: LucinaStudioState.backgroundKey), bg != "default" { run("lucina.setBackground('\(bg)')") }
        if let light = d.string(forKey: LucinaStudioState.lightingKey), light != "default" { run("lucina.setLighting('\(light)')") }
    }

    func setExpression(_ id: String?) {
        run(id.map { "lucina.setExpressionHold('\($0)')" } ?? "lucina.setExpressionHold(null)")
    }

    func playGesture(_ id: String) { run("lucina.gesture('\(id)')") }

    var dancing: Bool { LucinaStudioState.shared.dancing }
    var tilting: Bool { LucinaStudioState.shared.tilting }

    /// From a tap: dance, with the built-in beat only when her voice is on.
    func toggleDance() {
        let state = LucinaStudioState.shared
        state.dancing.toggle()
        let music = state.dancing && !muted
        if music {
            try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .default, options: [.mixWithOthers])
            try? AVAudioSession.sharedInstance().setActive(true)
        }
        run("lucina.setDance(\(state.dancing), \(music))")
    }

    /// Tilt parallax: CoreMotion attitude → deviceorientation events in the page (30 Hz).
    func toggleTilt() {
        let state = LucinaStudioState.shared
        if state.tilting {
            state.motion.stopDeviceMotionUpdates()
            state.tilting = false
            run("lucina.setGyro(false)")
            return
        }
        guard state.motion.isDeviceMotionAvailable, !UIAccessibility.isReduceMotionEnabled else { return }
        state.tilting = true
        run("lucina.setGyro(true)")
        state.motion.deviceMotionUpdateInterval = 1.0 / 30.0
        state.motion.startDeviceMotionUpdates(to: .main) { [weak self] motion, _ in
            guard let self, let a = motion?.attitude else { return }
            let beta = a.pitch * 180 / .pi
            let gamma = a.roll * 180 / .pi
            self.run(String(format: "lucina.tilt(%.2f, %.2f)", beta, gamma))
        }
    }

    /// The tab went away or the stage was released: no motion updates or dance beat for a stage nobody sees.
    func stopStudio() {
        let state = LucinaStudioState.shared
        if state.tilting {
            state.motion.stopDeviceMotionUpdates()
            state.tilting = false
            run("lucina.setGyro(false)")
        }
        if state.dancing {
            state.dancing = false
            run("lucina.setDance(false, false)")
        }
    }

    func takePhoto() { run("lucina.snapshot()") }

    /// The stage's PNG into the share sheet (save to Photos, send, …).
    func sharePhoto(dataURL: String) {
        guard let comma = dataURL.firstIndex(of: ","),
              let data = Data(base64Encoded: String(dataURL[dataURL.index(after: comma)...])),
              let image = UIImage(data: data) else { return }
        let sheet = UIActivityViewController(activityItems: [image], applicationActivities: nil)
        guard let root = UIApplication.shared.connectedScenes
            .compactMap({ ($0 as? UIWindowScene)?.keyWindow?.rootViewController })
            .first else { return }
        var top = root
        while let next = top.presentedViewController { top = next }
        sheet.popoverPresentationController?.sourceView = top.view
        sheet.popoverPresentationController?.sourceRect = CGRect(x: top.view.bounds.midX, y: top.view.bounds.maxY - 80, width: 1, height: 1)
        top.present(sheet, animated: true)
    }
}

/// Face, bust, full body and reset, over her (Custom Cast's camera presets).
struct LucinaViewBar: View {
    let controller: LucinaStageController

    var body: some View {
        HStack(spacing: 8) {
            chip("Face", id: "lucinaViewFace") { controller.cameraPreset("face") }
            chip("Bust", id: "lucinaViewBust") { controller.cameraPreset("bust") }
            chip("Full body", id: "lucinaViewFull") { controller.cameraPreset("full") }
            Button {
                controller.resetView()
            } label: {
                Image(systemName: "arrow.counterclockwise")
                    .font(.subheadline)
                    .frame(width: 34, height: 34)
                    .background(.thinMaterial, in: Circle())
            }
            .accessibilityLabel("Reset view")
            .accessibilityIdentifier("lucinaViewReset")
        }
    }

    private func chip(_ title: LocalizedStringKey, id: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title).font(.subheadline).padding(.horizontal, 12).frame(height: 34).background(.thinMaterial, in: Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(id)
    }
}

/// The studio menu: poses, face, background, light, photo, dance, tilt, face tracking, AR.
struct LucinaStudioMenu: View {
    let controller: LucinaStageController
    @State private var refresh = false

    var body: some View {
        Menu {
            Menu("Poses") {
                ForEach(LucinaStudioOptions.poses, id: \.id) { pose in
                    Button(String(localized: pose.name)) { controller.playGesture(pose.id) }
                }
            }
            Menu("Expression") {
                ForEach(LucinaStudioOptions.expressions, id: \.id) { e in
                    Button(String(localized: e.name)) { controller.setExpression(e.id) }
                }
            }
            Menu("Background") {
                ForEach(LucinaStudioOptions.backgrounds, id: \.id) { bg in
                    Button(String(localized: bg.name)) { controller.setBackground(bg.id) }
                }
            }
            Menu("Light") {
                ForEach(LucinaStudioOptions.lights, id: \.id) { light in
                    Button(String(localized: light.name)) { controller.setLighting(light.id) }
                }
            }
            Divider()
            Button {
                controller.takePhoto()
            } label: {
                Label("Photo", systemImage: "camera")
            }
            Button {
                controller.toggleDance()
                refresh.toggle()
            } label: {
                Label(controller.dancing ? "Stop dancing" : "Dance", systemImage: "music.note")
            }
            Button {
                controller.toggleTilt()
                refresh.toggle()
            } label: {
                Label(controller.tilting ? "Tilt off" : "Tilt", systemImage: "gyroscope")
            }
            if LucinaFaceTracker.isSupported {
                Button {
                    Task { await LucinaFaceTracker.shared.toggle(controller) }
                } label: {
                    Label(LucinaFaceTracker.shared.state == .off ? "Mirror me" : "Stop mirroring", systemImage: "face.smiling")
                }
                .accessibilityIdentifier("lucinaFaceTracking")
            }
            if LucinaARPresenter.isSupported {
                Button {
                    LucinaARPresenter.shared.presented = true
                } label: {
                    Label("Place in your room (AR)", systemImage: "arkit")
                }
                .accessibilityIdentifier("lucinaAR")
            }
        } label: {
            Image(systemName: "sparkles")
                .font(.title3)
                .frame(width: 52, height: 52)
                .background(.thinMaterial, in: Circle())
                .id(refresh)
        }
        .accessibilityLabel("Lucina's studio")
        .accessibilityIdentifier("lucinaStudio")
    }
}
