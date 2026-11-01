import AVKit
import SwiftUI
import UIKit

struct IgViewerRequest: Identifiable {
    let id = UUID()
    let items: [IgItem]
    let start: Int
    let username: String
    var archived = false
    var tab: IgTab?
    var highlight: IgHighlight?
}

struct IgSlide: Identifiable, Equatable {
    let id: String
    let item: IgItem
    let child: IgChild
    let frame: Int
    let frames: Int
    static func slides(_ items: [IgItem]) -> [IgSlide] {
        items.enumerated().flatMap { itemIndex, item in
            let children = item.children.isEmpty ? [IgChild(id: item.id, kind: item.kind == "video" ? "video" : "image", url: item.url, thumbUrl: item.thumbUrl)] : item.children
            return children.enumerated().map { index, child in
                IgSlide(id: "\(itemIndex):\(item.id):\(index)", item: item, child: child, frame: index, frames: children.count)
            }
        }
    }
    static func startIndex(_ items: [IgItem], itemIndex: Int) -> Int {
        slides(Array(items.prefix(max(0, itemIndex)))).count
    }
}

/// Carousels become consecutive frames, so paging follows the grid's original order.
struct InstagramViewer: View {
    @Environment(\.dismiss) private var dismiss
    let store: InstagramStore
    let request: IgViewerRequest
    @State private var selection: Int
    init(store: InstagramStore, request: IgViewerRequest) {
        self.store = store; self.request = request
        _selection = State(initialValue: IgSlide.startIndex(request.items, itemIndex: request.start))
    }
    private var items: [IgItem] {
        if let key = request.highlight?.id ?? request.tab?.rawValue { return store.pages[key]?.items ?? request.items }
        return request.items
    }
    private var slides: [IgSlide] { IgSlide.slides(items) }
    var body: some View {
        // Stories and highlights open full screen, their info as an overlay on a tap (InstagramStoryViewer).
        if request.tab == .stories || request.highlight != nil {
            InstagramStoryViewer(store: store, request: request)
        } else {
            paged
        }
    }
    private var paged: some View {
        VStack(spacing: 0) {
            HStack {
                Button { dismiss() } label: { Image(systemName: "xmark") }
                    .accessibilityLabel("Close").accessibilityIdentifier("instagram-viewer-close")
                Spacer()
                Text(verbatim: "@" + request.username).font(.headline)
                Spacer()
                Text("\(min(selection + 1, slides.count)) of \(slides.count)").font(.caption)
            }.padding()
            TabView(selection: $selection) {
                ForEach(Array(slides.enumerated()), id: \.element.id) { index, slide in
                    InstagramSlideView(store: store, slide: slide, username: request.username,
                                       archived: request.archived, active: selection == index)
                        .tag(index)
                }
            }.tabViewStyle(.page(indexDisplayMode: .never))
                .accessibilityIdentifier("instagram-viewer-pages")
        }
        .background(Theme.page.ignoresSafeArea()).foregroundStyle(Theme.text).tint(Theme.accent)
        .simultaneousGesture(DragGesture().onEnded { value in
            if value.translation.height > 120 && abs(value.translation.width) < 80 { dismiss() }
        })
        .overlay(alignment: .bottom) { IgNotice(store: store) }
        .task(id: selection) {
            if selection >= slides.count - 3 {
                if let highlight = request.highlight {
                    await store.load(.highlights, username: request.username, more: true, highlight: highlight)
                } else if let tab = request.tab { await store.load(tab, username: request.username, more: true) }
            }
        }
    }
}

private struct InstagramSlideView: View {
    let store: InstagramStore
    let slide: IgSlide
    let username: String
    let archived: Bool
    let active: Bool
    var body: some View {
        VStack(spacing: 12) {
            if slide.child.kind == "video" {
                InstagramVideoView(url: store.absolute(slide.child.url), active: active)
                    .frame(maxHeight: .infinity)
            } else {
                InstagramZoomPicture(store: store, path: slide.child.url).frame(maxHeight: .infinity)
            }
            if slide.frames > 1 { Text("Frame \(slide.frame + 1) of \(slide.frames)").font(.caption).foregroundStyle(Theme.textSecondary) }
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    if let seconds = slide.item.takenAt {
                        Text("Taken at \(Date(timeIntervalSince1970: seconds).formatted(date: .abbreviated, time: .shortened))").font(.caption)
                    }
                    if let caption = slide.item.caption, !caption.isEmpty { Text(verbatim: caption).textSelection(.enabled) }
                    HStack {
                        if let likes = slide.item.likeCount { Label("\(likes) likes", systemImage: "heart") }
                        if let comments = slide.item.commentCount { Label("\(comments) comments", systemImage: "bubble") }
                    }.font(.caption).foregroundStyle(Theme.textSecondary)
                    actions
                }.frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal)
            }.frame(maxHeight: 170)
        }.padding(.bottom)
    }
    private var actions: some View {
        VStack(alignment: .leading, spacing: 8) {
            if !archived {
                HStack {
                    Button("Save to Lucina") { Task { await store.save(slide.item, username: username, child: slide.child) } }
                        .buttonStyle(.borderedProminent).disabled(store.saving).accessibilityIdentifier("instagram-save")
                    if slide.frames > 1 {
                        Button("Save all \(slide.frames)") { Task { await store.save(slide.item, username: username) } }.disabled(store.saving)
                    }
                    if store.saving { ProgressView() }
                }
            }
            HStack(spacing: 20) {
                if let url = store.absolute(slide.item.permalink ?? slide.child.url) {
                    ShareLink(item: url) { Label("Share", systemImage: "square.and.arrow.up") }
                }
                if let link = slide.item.permalink.flatMap({ store.absolute($0) }) {
                    Link("Open on Instagram", destination: link).accessibilityIdentifier("instagram-open-permalink")
                }
            }.font(.callout)
        }
    }
}

private struct InstagramVideoView: View {
    let url: URL?
    let active: Bool
    @State private var player: AVPlayer?
    var body: some View {
        Group {
            if let player { VideoPlayer(player: player) }
            else { ProgressView() }
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

struct InstagramProfilePicture: View {
    @Environment(\.dismiss) private var dismiss
    let store: InstagramStore
    let path: String?
    var body: some View {
        VStack {
            HStack {
                Spacer()
                Button("Close") { dismiss() }.padding().accessibilityIdentifier("instagram-picture-close")
            }
            InstagramZoomPicture(store: store, path: path)
        }.background(Theme.page.ignoresSafeArea()).tint(Theme.accent)
            .simultaneousGesture(DragGesture().onEnded { value in
                if value.translation.height > 140 && abs(value.translation.width) < 80 { dismiss() }
            })
    }
}

struct InstagramZoomPicture: View {
    let store: InstagramStore
    let path: String?
    @State private var image: UIImage?
    @State private var failed = false
    var body: some View {
        Group {
            if let image { InstagramZoomScroll(image: image) }
            else if failed {
                ContentUnavailableView {
                    Label("Couldn't load the image.", systemImage: "photo")
                } actions: {
                    Button("Try again") { Task { await load() } }
                }
            } else { ProgressView() }
        }.task(id: path) { await load() }
    }
    private func load() async {
        failed = false
        image = await InstagramPictureLoader.image(store: store, path: path)
        failed = image == nil
    }
}

/// UIScrollView gives photos native pinch, panning and double-tap zoom on iOS 17.
private struct InstagramZoomScroll: UIViewRepresentable {
    let image: UIImage
    func makeCoordinator() -> Coordinator { Coordinator() }
    func makeUIView(context: Context) -> UIScrollView {
        let scroll = InstagramPhotoScroll()
        scroll.minimumZoomScale = 1; scroll.maximumZoomScale = 5
        scroll.delegate = context.coordinator
        scroll.showsHorizontalScrollIndicator = false; scroll.showsVerticalScrollIndicator = false
        scroll.bouncesZoom = true
        let picture = context.coordinator.picture
        picture.contentMode = .scaleAspectFit
        picture.isUserInteractionEnabled = true
        scroll.picture = picture
        scroll.addSubview(picture)
        let doubleTap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.doubleTap(_:)))
        doubleTap.numberOfTapsRequired = 2
        scroll.addGestureRecognizer(doubleTap)
        return scroll
    }
    func updateUIView(_ scroll: UIScrollView, context: Context) {
        if context.coordinator.picture.image !== image {
            scroll.setZoomScale(1, animated: false)
            context.coordinator.picture.image = image
        }
        scroll.setNeedsLayout()
    }
    final class Coordinator: NSObject, UIScrollViewDelegate {
        let picture = UIImageView()
        func viewForZooming(in scrollView: UIScrollView) -> UIView? { picture }
        func scrollViewDidZoom(_ scrollView: UIScrollView) {
            let horizontal = max(0, (scrollView.bounds.width - picture.frame.width) / 2)
            let vertical = max(0, (scrollView.bounds.height - picture.frame.height) / 2)
            scrollView.contentInset = UIEdgeInsets(top: vertical, left: horizontal, bottom: vertical, right: horizontal)
        }
        @objc func doubleTap(_ gesture: UITapGestureRecognizer) {
            guard let scroll = gesture.view as? UIScrollView else { return }
            if scroll.zoomScale > 1 { scroll.setZoomScale(1, animated: true) }
            else {
                let point = gesture.location(in: picture)
                let size = CGSize(width: scroll.bounds.width / 3, height: scroll.bounds.height / 3)
                scroll.zoom(to: CGRect(x: point.x - size.width / 2, y: point.y - size.height / 2, width: size.width, height: size.height), animated: true)
            }
        }
    }
}

/// SwiftUI can assign the viewport after `updateUIView`; size the photo during UIKit layout.
private final class InstagramPhotoScroll: UIScrollView {
    weak var picture: UIImageView?
    override func layoutSubviews() {
        super.layoutSubviews()
        if zoomScale == 1, let picture, picture.frame.size != bounds.size {
            picture.frame = CGRect(origin: .zero, size: bounds.size)
            contentSize = bounds.size
        }
    }
}
