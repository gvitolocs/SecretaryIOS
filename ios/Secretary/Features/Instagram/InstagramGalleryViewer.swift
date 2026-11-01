import AVKit
import SwiftUI
import UIKit

/// Stories and highlights the Gallery's way (owner, 2026-11-01: "I like the gallery views more"): each item full screen
/// on black, pictures with pinch, double-tap zoom and pan, swiping between items, one tap on the picture hides or shows
/// the bars (and the status bar), a swipe down closes when the picture is not zoomed. Who, when, the caption and the
/// buttons sit on the bars' scrims; videos keep the system player's controls.
struct InstagramGalleryViewer: View {
    @Environment(\.dismiss) private var dismiss
    let store: InstagramStore
    let request: IgViewerRequest
    @State private var selection: Int
    @State private var chromeHidden = false
    @State private var zoomed = false

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
        ZStack {
            Color.black.ignoresSafeArea()
            TabView(selection: $selection) {
                ForEach(Array(slides.enumerated()), id: \.element.id) { index, slide in
                    page(slide, active: selection == index).tag(index)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
            .ignoresSafeArea()
            .accessibilityIdentifier("instagram-viewer-pages")
            if !chromeHidden, let slide = current { bars(slide).transition(.opacity) }
        }
        .statusBarHidden(chromeHidden)
        .animation(.easeInOut(duration: 0.2), value: chromeHidden)
        .simultaneousGesture(DragGesture().onEnded { value in
            if !zoomed && value.translation.height > 120 && abs(value.translation.width) < 80 { dismiss() }
        })
        .overlay(alignment: .bottom) { IgNotice(store: store) }
        .onChange(of: selection) { _, _ in zoomed = false }
        .task(id: selection) {
            if selection >= slides.count - 3 {
                if let highlight = request.highlight {
                    await store.load(.highlights, username: request.username, more: true, highlight: highlight)
                } else if let tab = request.tab { await store.load(tab, username: request.username, more: true) }
            }
        }
    }

    @ViewBuilder private func page(_ slide: IgSlide, active: Bool) -> some View {
        if slide.child.kind == "video" {
            InstagramGalleryVideo(url: store.absolute(slide.child.url), active: active)
        } else {
            InstagramZoomPicture(store: store, path: slide.child.url,
                                 onTap: { chromeHidden.toggle() }, onZoom: { zoomed = $0 })
        }
    }

    private func bars(_ slide: IgSlide) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                Button { dismiss() } label: { Image(systemName: "chevron.left").font(.headline).frame(width: 36, height: 36) }
                    .accessibilityLabel("Close").accessibilityIdentifier("instagram-viewer-close")
                VStack(alignment: .leading, spacing: 1) {
                    Text(verbatim: "@" + request.username).font(.subheadline.weight(.semibold))
                    if let seconds = slide.item.takenAt {
                        Text(Date(timeIntervalSince1970: seconds).formatted(date: .abbreviated, time: .shortened)).font(.caption).opacity(0.75)
                    }
                }
                Spacer()
                Text("\(selection + 1) of \(slides.count)").font(.caption).opacity(0.8)
            }
            .padding(.horizontal, 12).padding(.top, 8).padding(.bottom, 28)
            .background(LinearGradient(colors: [.black.opacity(0.65), .clear], startPoint: .top, endPoint: .bottom).ignoresSafeArea())
            Spacer(minLength: 0).allowsHitTesting(false)
            VStack(alignment: .leading, spacing: 12) {
                if let caption = slide.item.caption, !caption.isEmpty {
                    Text(verbatim: caption).font(.subheadline).lineLimit(4)
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
            .padding(.horizontal, 16).padding(.top, 36).padding(.bottom, 12)
            .background(LinearGradient(colors: [.clear, .black.opacity(0.7)], startPoint: .top, endPoint: .bottom).ignoresSafeArea())
        }
        .foregroundStyle(.white).tint(Theme.accent)
    }
}

/// A video item: the system player (its own controls), playing while it is the page on screen.
private struct InstagramGalleryVideo: View {
    let url: URL?
    let active: Bool
    @State private var player: AVPlayer?
    var body: some View {
        Group {
            if let player { VideoPlayer(player: player) } else { ProgressView().tint(.white) }
        }
        .task(id: url) {
            player?.pause()
            player = url.map { AVPlayer(url: $0) }
            if active { player?.play() }
        }
        .onChange(of: active) { _, playing in if playing { player?.play() } else { player?.pause() } }
        .onDisappear { player?.pause() }
    }
}
