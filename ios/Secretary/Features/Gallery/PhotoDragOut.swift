import os
import SecretaryCore
import SwiftUI
import UIKit

private let dragLog = Logger(subsystem: "tech.getsecretary.app", category: "GalleryImages")

/// Google Photos' swipe on a full-screen photo: dragged down, the photo follows the finger and
/// shrinks while the black fades to what is behind the viewer; let go far or fast enough and the
/// viewer closes, otherwise it springs back. Dragged up, it shows the details. Horizontal drags
/// stay with the pager, and a zoomed photo (`enabled` false) pans instead. The Rust view
/// (`secretary_image_view` with `drag`) decides; this only draws and animates.
///
/// The cover needs `.presentationBackground(.clear)` (and `clearNavigationBackground()` inside a
/// NavigationStack) for the grid to show through.
struct PhotoDragOut: ViewModifier {
    var enabled: Bool
    /// The presentation has the zoom transition (iOS 18): its own interactive dismiss shrinks the
    /// photo back into its grid tile, so a drag down is left to it and only the swipe up is ours.
    var systemDismiss = false
    /// True while a drag is out, so the viewer can hide its bars.
    @Binding var dragging: Bool
    var onDismiss: () -> Void
    var onInfo: () -> Void = {}
    @State private var axis = "undecided"
    @State private var out = Out.rest

    struct Out: Equatable {
        var x: CGFloat = 0, y: CGFloat = 0, scale: CGFloat = 1, backdrop: Double = 1
        static let rest = Out()
    }

    func body(content: Content) -> some View {
        GeometryReader { geo in
            dismissGesture(ZStack {
                Color.black.opacity(out.backdrop).ignoresSafeArea()
                content
                    .scaleEffect(out.scale)
                    .offset(x: out.x, y: out.y)
                    // The pager stands still while the photo is out (setting it cancels its pan).
                    .scrollDisabled(axis == "out")
            }, geo.size)
        }
    }

    /// iOS 18+: a UIKit pan, not a SwiftUI DragGesture: above the pager's scroll view a DragGesture
    /// never got the touch (the scroll view's pan won), so a swipe down did nothing on the iPhone.
    @ViewBuilder
    private func dismissGesture<V: View>(_ view: V, _ size: CGSize) -> some View {
        if #available(iOS 18.0, *) {
            view.gesture(VerticalPan(enabled: enabled, upOnly: systemDismiss,
                                     changed: { t, vy in changed(t, vy, size) },
                                     ended: { t, vy in ended(t, vy, size) }))
        } else {
            view.simultaneousGesture(DragGesture(minimumDistance: 10)
                .onChanged { g in changed(g.translation, Self.velocity(g), size) }
                .onEnded { g in ended(g.translation, Self.velocity(g), size) }, including: enabled ? .all : .subviews)
        }
    }

    /// The vertical velocity that `ask` turns back into SwiftUI's predicted end.
    private static func velocity(_ g: DragGesture.Value) -> CGFloat {
        (g.predictedEndTranslation.height - g.translation.height) / 0.25
    }

    private func changed(_ t: CGSize, _ vy: CGFloat, _ size: CGSize) {
        guard axis == "undecided" || axis == "out" else { return }
        guard let d = Self.ask(t, vy, size, ended: false) else {
            dragLog.error("GalleryDrag no drag answer from the core (an old SecretaryCore?)")
            return
        }
        if axis == "undecided" {
            axis = d.axis
            if axis != "undecided" {
                dragLog.notice("GalleryDrag axis=\(d.axis, privacy: .public) dx=\(Int(t.width)) dy=\(Int(t.height))")
            }
            if axis == "out" { dragging = true }
        }
        if axis == "out" { out = d.out }
    }

    private func ended(_ t: CGSize, _ vy: CGFloat, _ size: CGSize) {
        defer { axis = "undecided" }
        guard axis == "out" else { return }
        let end = Self.ask(t, vy, size, ended: true)?.end
        dragLog.notice("GalleryDrag end=\(end ?? "none", privacy: .public) dy=\(Int(t.height)) vy=\(Int(vy)) h=\(Int(size.height))")
        switch end {
        case "dismiss":
            // Carry on down and fade out, then close without the cover's slide.
            withAnimation(.easeOut(duration: 0.18)) {
                out.y += size.height * 0.25
                out.scale *= 0.9
                out.backdrop = 0
            } completion: {
                var t = Transaction()
                t.disablesAnimations = true
                withTransaction(t) { onDismiss() }
            }
        case "info":
            back()
            onInfo()
        default:
            back()
        }
    }

    private func back() {
        withAnimation(.spring(response: 0.3, dampingFraction: 0.85)) { out = .rest }
        dragging = false
    }

    /// `vy`: vertical velocity in points per second; a flick's predicted end is about a quarter
    /// second of it further on (SwiftUI's `predictedEndTranslation` for a decelerating drag).
    private static func ask(_ t: CGSize, _ vy: CGFloat, _ size: CGSize, ended: Bool) -> (axis: String, out: Out, end: String?)? {
        let request: [String: Any] = [
            "new": [1, 1, size.width, size.height],
            "drag": ["dx": t.width, "dy": t.height, "predicted_dy": t.height + vy * 0.25, "ended": ended],
        ]
        guard let body = try? JSONSerialization.data(withJSONObject: request),
              let json = String(data: body, encoding: .utf8),
              let raw = json.withCString({ secretary_image_view($0) }) else { return nil }
        defer { secretary_string_free(raw) }
        guard let data = String(cString: raw).data(using: .utf8),
              let answer = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let drag = answer["drag"] as? [String: Any],
              let axis = drag["axis"] as? String,
              let o = drag["out"] as? [String: Double] else { return nil }
        let out = Out(x: o["offset_x"] ?? 0, y: o["offset_y"] ?? 0, scale: o["scale"] ?? 1, backdrop: o["backdrop"] ?? 1)
        return (axis, out, drag["end"] as? String)
    }
}

/// One finger, mostly vertical at the start, recognised together with the pager's scroll pan (which
/// then stays put: `scrollDisabled` while the photo is out). Off while the photo is zoomed.
@available(iOS 18.0, *)
struct VerticalPan: UIGestureRecognizerRepresentable {
    var enabled: Bool
    var upOnly = false
    var changed: (CGSize, CGFloat) -> Void
    var ended: (CGSize, CGFloat) -> Void

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        var enabled = true
        var upOnly = false

        func gestureRecognizerShouldBegin(_ gesture: UIGestureRecognizer) -> Bool {
            guard enabled, let pan = gesture as? UIPanGestureRecognizer else { return false }
            let v = pan.velocity(in: pan.view)
            let begin = abs(v.y) > abs(v.x) && (!upOnly || v.y < 0)
            dragLog.notice("GalleryDrag pan should_begin=\(begin) vx=\(Int(v.x)) vy=\(Int(v.y))")
            return begin
        }

        func gestureRecognizer(_ gesture: UIGestureRecognizer,
                               shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool { true }
    }

    func makeCoordinator(converter: CoordinateSpaceConverter) -> Coordinator { Coordinator() }

    func makeUIGestureRecognizer(context: Context) -> UIPanGestureRecognizer {
        let pan = UIPanGestureRecognizer()
        pan.maximumNumberOfTouches = 1
        pan.delegate = context.coordinator
        context.coordinator.enabled = enabled
        context.coordinator.upOnly = upOnly
        return pan
    }

    func updateUIGestureRecognizer(_ recognizer: UIPanGestureRecognizer, context: Context) {
        context.coordinator.enabled = enabled
        context.coordinator.upOnly = upOnly
    }

    func handleUIGestureRecognizerAction(_ recognizer: UIPanGestureRecognizer, context: Context) {
        let p = recognizer.translation(in: recognizer.view)
        let t = CGSize(width: p.x, height: p.y)
        let vy = recognizer.velocity(in: recognizer.view).y
        switch recognizer.state {
        case .began, .changed: changed(t, vy)
        case .ended, .cancelled, .failed: ended(t, vy)
        default: break
        }
    }
}

extension View {
    func photoDragOut(enabled: Bool = true, systemDismiss: Bool = false, dragging: Binding<Bool>,
                      onDismiss: @escaping () -> Void, onInfo: @escaping () -> Void = {}) -> some View {
        var systemOwnsDown = false
        if #available(iOS 18.0, *) { systemOwnsDown = systemDismiss }
        return modifier(PhotoDragOut(enabled: enabled, systemDismiss: systemOwnsDown, dragging: dragging,
                                     onDismiss: onDismiss, onInfo: onInfo))
    }

    /// iOS 18: the viewer grows out of this view and shrinks back into it (the zoom transition).
    @ViewBuilder
    func zoomSource(_ id: String, in namespace: Namespace.ID?) -> some View {
        if #available(iOS 18.0, *), let namespace {
            matchedTransitionSource(id: id, in: namespace)
        } else {
            self
        }
    }

    /// On a presented viewer: open from, and close into, the source with this id.
    @ViewBuilder
    func zoomTransition(from id: String, in namespace: Namespace.ID) -> some View {
        if #available(iOS 18.0, *) {
            navigationTransition(.zoom(sourceID: id, in: namespace))
        } else {
            self
        }
    }

    /// On the root view inside a viewer's NavigationStack: no opaque container behind it, so a
    /// photo dragged out shows the grid (the cover itself takes `.presentationBackground(.clear)`).
    @ViewBuilder
    func clearNavigationBackground() -> some View {
        if #available(iOS 18.0, *) {
            containerBackground(.clear, for: .navigation)
        } else {
            self
        }
    }
}
