import AVFoundation
import SwiftUI
import UIKit

/// Stories and highlights full screen (owner, 2026-11-01): the picture or video edge to edge on black, nothing under
/// it. A tap in the middle shows who, when, the caption, the progress and the buttons over it (gone again after 3 s);
/// a tap on the left or right third goes back or on, swiping pages too, a swipe down closes and holding pauses a video.
struct InstagramStoryViewer: View {
    @Environment(\.dismiss) private var dismiss
    let store: InstagramStore
    let request: IgViewerRequest
    @State private var selection: Int
    @State private var chrome = true
    @State private var chromeShown = 0
    @GestureState private var holding = false

    init(store: InstagramStore, request: IgViewerRequest) {
        self.store = store; self.request = request
        _selection = State(initialValue: IgSlide.startIndex(request.items, itemIndex: request.start))
    }
    private var items: [IgItem] {
        if let key = request.highlight?.id ?? request.tab?.rawValue { return store.pages[key]?.items ?? request.items }
        return request.items
    }
    private var slides: [IgSlide] { IgSlide.slides(items) }
    private var current: IgSlide? { slides.indices.contains(selection) ? slides[selection] : nil }

    var body: some View {
        GeometryReader { geo in
            ZStack {
                Color.black.ignoresSafeArea()
                TabView(selection: $selection) {
                    ForEach(Array(slides.enumerated()), id: \.element.id) { index, slide in
                        InstagramStorySlide(store: store, slide: slide, active: selection == index, held: holding)
                            .tag(index)
                    }
                }
                .tabViewStyle(.page(indexDisplayMode: .never))
                .ignoresSafeArea()
                .accessibilityIdentifier("instagram-story-pages")
                .simultaneousGesture(SpatialTapGesture().onEnded { tap(at: $0.location.x, width: geo.size.width) })
                .simultaneousGesture(
                    LongPressGesture(minimumDuration: 0.35, maximumDistance: 12)
                        .sequenced(before: DragGesture(minimumDistance: 0))
                        .updating($holding) { value, state, _ in
                            if case .second(true, _) = value { state = true }
                        }
                )
                if chrome, let slide = current { overlay(slide).transition(.opacity) }
            }
        }
        .statusBarHidden(true)
        .persistentSystemOverlays(.hidden)
        .simultaneousGesture(DragGesture().onEnded { value in
            if value.translation.height > 120 && abs(value.translation.width) < 80 { dismiss() }
        })
        .overlay(alignment: .bottom) { IgNotice(store: store) }
        .task(id: chromeShown) {
            guard chrome else { return }
            try? await Task.sleep(for: .seconds(3))
            if !Task.isCancelled { withAnimation(.easeInOut(duration: 0.2)) { chrome = false } }
        }
        .task(id: selection) {
            if selection >= slides.count - 3 {
                if let highlight = request.highlight {
                    await store.load(.highlights, username: request.username, more: true, highlight: highlight)
                } else if let tab = request.tab { await store.load(tab, username: request.username, more: true) }
            }
        }
    }

    private func tap(at x: CGFloat, width: CGFloat) {
        if x < width * 0.3 {
            if selection > 0 { withAnimation { selection -= 1 } }
        } else if x > width * 0.7 {
            if selection < slides.count - 1 { withAnimation { selection += 1 } }
        } else {
            withAnimation(.easeInOut(duration: 0.2)) { chrome.toggle() }
            if chrome { chromeShown += 1 }
        }
    }

    private func overlay(_ slide: IgSlide) -> some View {
        ZStack {
            VStack(spacing: 0) {
                LinearGradient(colors: [.black.opacity(0.6), .clear], startPoint: .top, endPoint: .bottom).frame(height: 150)
                Spacer(minLength: 0)
                LinearGradient(colors: [.clear, .black.opacity(0.7)], startPoint: .top, endPoint: .bottom).frame(height: 220)
            }
            .ignoresSafeArea()
            .allowsHitTesting(false)
            VStack(spacing: 0) {
                VStack(spacing: 10) {
                    HStack(spacing: 3) {
                        ForEach(slides.indices, id: \.self) { index in
                            Capsule().fill(Color.white.opacity(index == selection ? 1 : index < selection ? 0.7 : 0.28)).frame(height: 3)
                        }
                    }
                    .allowsHitTesting(false)
                    HStack(spacing: 10) {
                        Button { dismiss() } label: {
                            Image(systemName: "xmark").font(.headline).frame(width: 36, height: 36).background(.black.opacity(0.35), in: Circle())
                        }
                        .accessibilityLabel("Close").accessibilityIdentifier("instagram-viewer-close")
                        VStack(alignment: .leading, spacing: 1) {
                            Text(verbatim: "@" + request.username).font(.subheadline.weight(.semibold))
                            if let seconds = slide.item.takenAt {
                                Text(Date(timeIntervalSince1970: seconds), format: .relative(presentation: .named)).font(.caption).opacity(0.75)
                            }
                        }
                        .allowsHitTesting(false)
                        Spacer()
                        Text("\(selection + 1) of \(slides.count)").font(.caption).opacity(0.8).allowsHitTesting(false)
                    }
                }
                .padding(.horizontal, 12).padding(.top, 8)
                Spacer(minLength: 0)
                VStack(alignment: .leading, spacing: 12) {
                    if let caption = slide.item.caption, !caption.isEmpty {
                        Text(verbatim: caption).font(.subheadline).lineLimit(4).allowsHitTesting(false)
                    }
                    HStack(spacing: 18) {
                        if !request.archived {
                            Button { Task { await store.save(slide.item, username: request.username, child: slide.child) } } label: {
                                Label("Save to Lucina", systemImage: "square.and.arrow.down")
                            }
                            .buttonStyle(.borderedProminent).disabled(store.saving).accessibilityIdentifier("instagram-save")
                        }
                        if let url = store.absolute(slide.item.permalink ?? slide.child.url) {
                            ShareLink(item: url) { Image(systemName: "square.and.arrow.up") }.accessibilityLabel("Share")
                        }
                        if let link = slide.item.permalink.flatMap({ store.absolute($0) }) {
                            Link(destination: link) { Image(systemName: "arrow.up.right.square") }
                                .accessibilityLabel("Open on Instagram").accessibilityIdentifier("instagram-open-permalink")
                        }
                        Spacer()
                    }
                    .font(.title3)
                }
                .padding(.horizontal, 16).padding(.bottom, 16)
            }
        }
        .foregroundStyle(.white).tint(Theme.accent)
    }
}

/// One story: the picture fitted, or the video looping without controls (a tap belongs to the viewer).
private struct InstagramStorySlide: View {
    let store: InstagramStore
    let slide: IgSlide
    let active: Bool
    let held: Bool
    @State private var image: UIImage?
    @State private var player: AVQueuePlayer?
    @State private var looper: AVPlayerLooper?

    var body: some View {
        Group {
            if slide.child.kind == "video" {
                if let player { InstagramStoryVideo(player: player) } else { ProgressView().tint(.white) }
            } else if let image {
                Image(uiImage: image).resizable().scaledToFit()
            } else {
                ProgressView().tint(.white)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .contentShape(Rectangle())
        .task(id: slide.id) {
            if slide.child.kind == "video" {
                guard let url = store.absolute(slide.child.url) else { return }
                let queue = AVQueuePlayer()
                looper = AVPlayerLooper(player: queue, templateItem: AVPlayerItem(url: url))
                player = queue
                update()
            } else {
                image = await InstagramPictureLoader.image(store: store, path: slide.child.url)
            }
        }
        .onChange(of: active) { _, _ in update() }
        .onChange(of: held) { _, _ in update() }
        .onDisappear { player?.pause() }
    }

    private func update() {
        if active && !held { player?.play() } else { player?.pause() }
    }
}

/// The video's layer alone: no system controls to catch the taps.
private struct InstagramStoryVideo: UIViewRepresentable {
    let player: AVPlayer
    func makeUIView(context: Context) -> PlayerView {
        let view = PlayerView()
        view.isUserInteractionEnabled = false
        view.playerLayer.videoGravity = .resizeAspect
        view.playerLayer.player = player
        return view
    }
    func updateUIView(_ view: PlayerView, context: Context) {
        if view.playerLayer.player !== player { view.playerLayer.player = player }
    }
    final class PlayerView: UIView {
        override class var layerClass: AnyClass { AVPlayerLayer.self }
        var playerLayer: AVPlayerLayer { layer as! AVPlayerLayer }
    }
}
