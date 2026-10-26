import ARKit
import AVFoundation
import Observation
import SwiftUI

// Face tracking (Custom Cast F1): the TrueDepth camera drives Lucina's head, eyes, blinks, mouth and
// brows. ARKit's face anchor gives 52 blend-shape coefficients and the face transform; Swift sends
// only those numbers to the stage (`lucina.faceFrame`), where the shared solver of
// packages/lucina-stage (facedrive/) calibrates, mirrors and smooths them. The camera frames never
// leave ARKit: nothing is recorded, stored or uploaded. It starts only from a tap (the camera
// permission is asked then), shows a clear indicator while on, and stops when her view closes, the
// app goes to the background, or AR opens.

@Observable
@MainActor
final class LucinaFaceTracker: NSObject {
    static let shared = LucinaFaceTracker()

    enum State: Equatable { case off, starting, on }

    private(set) var state: State = .off
    /// A short problem report for the controls (permission refused, no TrueDepth camera).
    var message: String?

    /// TrueDepth camera present (Face ID iPhones and iPads).
    static var isSupported: Bool { ARFaceTrackingConfiguration.isSupported }

    @ObservationIgnored private var session: ARSession?
    @ObservationIgnored private weak var controller: LucinaStageController?
    @ObservationIgnored private var lastSent: TimeInterval = 0
    @ObservationIgnored private var backgroundObserver: NSObjectProtocol?

    /// Frames per second sent to the page (ARKit delivers 60).
    private static let rate: TimeInterval = 1.0 / 30.0

    func toggle(_ controller: LucinaStageController) async {
        if state != .off {
            stop()
            return
        }
        await start(controller)
    }

    func start(_ controller: LucinaStageController) async {
        guard state == .off else { return }
        guard Self.isSupported else {
            message = String(localized: "Face tracking needs the front TrueDepth camera.")
            return
        }
        message = nil
        state = .starting
        let granted: Bool
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: granted = true
        case .notDetermined: granted = await AVCaptureDevice.requestAccess(for: .video)
        default: granted = false
        }
        // Stopped while the permission sheet was up.
        guard state == .starting else { return }
        guard granted else {
            state = .off
            message = String(localized: "Allow the camera for Secretary in Settings to let Lucina mirror your face.")
            return
        }
        let session = ARSession()
        session.delegate = self
        let configuration = ARFaceTrackingConfiguration()
        configuration.maximumNumberOfTrackedFaces = 1
        configuration.isLightEstimationEnabled = false
        configuration.providesAudioData = false
        configuration.isWorldTrackingEnabled = false
        session.run(configuration, options: [.resetTracking, .removeExistingAnchors])
        self.session = session
        self.controller = controller
        lastSent = 0
        state = .on
        controller.run("lucina.faceStart()")
        backgroundObserver = NotificationCenter.default.addObserver(
            forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.stop() }
        }
    }

    /// Stops the camera; she eases back to her own motion.
    func stop() {
        if let backgroundObserver { NotificationCenter.default.removeObserver(backgroundObserver) }
        backgroundObserver = nil
        session?.delegate = nil
        session?.pause()
        session = nil
        if state != .off { controller?.run("lucina.faceStop()") }
        controller = nil
        state = .off
    }

    /// Takes the current head pose as straight ahead.
    func recenter() {
        controller?.run("lucina.faceRecenter()")
    }

    fileprivate func send(_ anchor: ARFaceAnchor, camera: simd_float4x4, at time: TimeInterval) {
        guard state == .on, let controller, anchor.isTracked, time - lastSent >= Self.rate * 0.9 else { return }
        lastSent = time
        // The face relative to the camera, column-major: the solver only uses its rotation relative to
        // the pose at start, so the camera's axis convention cancels out.
        let m = simd_mul(simd_inverse(camera), anchor.transform)
        let columns = [m.columns.0, m.columns.1, m.columns.2, m.columns.3]
        let matrix = columns.flatMap { [$0.x, $0.y, $0.z, $0.w] }.map { String(format: "%.5f", $0) }.joined(separator: ",")
        let shapes = anchor.blendShapes.map { key, value in
            "\"\(Self.blendshapeName(key.rawValue))\":\(String(format: "%.3f", value.floatValue))"
        }.joined(separator: ",")
        controller.run("lucina.faceFrame({\(shapes)},[\(matrix)])")
    }

    /// ARKit's `eyeBlink_L` → MediaPipe / web name `eyeBlinkLeft` (the solver's names).
    nonisolated static func blendshapeName(_ raw: String) -> String {
        if raw.hasSuffix("_L") { return String(raw.dropLast(2)) + "Left" }
        if raw.hasSuffix("_R") { return String(raw.dropLast(2)) + "Right" }
        return raw
    }
}

extension LucinaFaceTracker: ARSessionDelegate {
    nonisolated func session(_ session: ARSession, didUpdate frame: ARFrame) {
        guard let face = frame.anchors.lazy.compactMap({ $0 as? ARFaceAnchor }).first else { return }
        let camera = frame.camera.transform
        let time = frame.timestamp
        // ARSession calls its delegate on the main queue (no delegateQueue set).
        MainActor.assumeIsolated { self.send(face, camera: camera, at: time) }
    }

    nonisolated func session(_ session: ARSession, didFailWithError error: any Error) {
        MainActor.assumeIsolated {
            self.message = String(localized: "Face tracking stopped: \(error.localizedDescription)")
            self.stop()
        }
    }

    nonisolated func sessionWasInterrupted(_ session: ARSession) {
        MainActor.assumeIsolated { self.stop() }
    }
}

/// The camera indicator while face tracking is on: a red dot, what is happening, Recenter and Stop.
struct LucinaFaceBadge: View {
    let tracker: LucinaFaceTracker

    var body: some View {
        if tracker.state != .off {
            HStack(spacing: 10) {
                Circle().fill(.red).frame(width: 10, height: 10)
                    .shadow(color: .red.opacity(0.7), radius: 4)
                    .accessibilityHidden(true)
                Text(tracker.state == .starting ? "Starting the camera…" : "Camera on: she mirrors your face. Nothing leaves this phone.")
                    .font(.footnote)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                Button("Recenter") { tracker.recenter() }
                    .font(.footnote.weight(.semibold))
                    .disabled(tracker.state != .on)
                    .accessibilityIdentifier("lucinaFaceRecenter")
                Button("Stop") { tracker.stop() }
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("lucinaFaceStop")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).strokeBorder(.red.opacity(0.5)))
            .padding(.horizontal, 16)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Face tracking is on")
        }
    }
}
