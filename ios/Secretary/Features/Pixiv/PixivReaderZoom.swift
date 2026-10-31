import SwiftUI
import UIKit

/// Native scroll-view zoom keeps pinch, double-tap and panning consistent with Photos.
struct PixivReaderZoom: View {
    let page: PixivPage
    let client: APIClient?
    @Environment(\.dismiss) private var dismiss
    @State private var image: UIImage?
    @State private var error = false
    @State private var originalAsked = false
    @State private var fitWidth = false
    @State private var reset = 0

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button("Close", systemImage: "xmark") { dismiss() }.labelStyle(.iconOnly)
                    .accessibilityIdentifier("pixiv-zoom-close")
                Spacer()
                Button("Fit") { fitWidth = false; reset += 1 }
                    .accessibilityIdentifier("pixiv-zoom-fit")
                Button("Fit width") { fitWidth = true; reset += 1 }
                    .accessibilityIdentifier("pixiv-zoom-width")
            }
            .padding().background(.regularMaterial)
            if let image {
                PixivReaderZoomSurface(image: image, fitWidth: fitWidth, reset: reset) { pixelScale in
                    if pixelScale > 1.1 { Task { await loadOriginal() } }
                }
                .accessibilityIdentifier("pixiv-zoom-image")
            } else if error {
                VStack {
                    ContentUnavailableView("Couldn't load the image.", systemImage: "photo")
                    Button("Try again") { Task { await load() } }
                }
            } else { Spacer(); ProgressView(); Spacer() }
        }
        .background(Theme.page)
        .task { await load() }
    }

    private func load() async {
        error = false
        let path = page.url.isEmpty ? (page.thumbUrl.isEmpty ? page.originalUrl : page.thumbUrl) : page.url
        guard let url = client?.absolute(path) else { error = true; return }
        let loaded = await SignedImageLoader.shared.image(url)
        guard !Task.isCancelled else { return }
        image = loaded; error = loaded == nil
    }

    private func loadOriginal() async {
        guard !originalAsked, !page.originalUrl.isEmpty, page.originalUrl != page.url,
              let url = client?.absolute(page.originalUrl) else { return }
        originalAsked = true
        if let original = await SignedImageLoader.shared.image(url), !Task.isCancelled { image = original }
    }
}

private struct PixivReaderZoomSurface: UIViewRepresentable {
    let image: UIImage
    let fitWidth: Bool
    let reset: Int
    let onScale: (CGFloat) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(onScale: onScale) }

    func makeUIView(context: Context) -> PixivReaderScrollView {
        let scroll = PixivReaderScrollView()
        scroll.delegate = context.coordinator
        scroll.addSubview(scroll.picture)
        scroll.showsVerticalScrollIndicator = false
        scroll.showsHorizontalScrollIndicator = false
        scroll.contentInsetAdjustmentBehavior = .never
        scroll.accessibilityLabel = String(localized: "Zoomable page")
        let tap = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.doubleTap(_:)))
        tap.numberOfTapsRequired = 2
        scroll.addGestureRecognizer(tap)
        return scroll
    }

    func updateUIView(_ scroll: PixivReaderScrollView, context: Context) {
        scroll.widthMode = fitWidth
        let shouldReset = context.coordinator.reset != reset
        context.coordinator.reset = reset
        scroll.setImage(image, reset: shouldReset)
    }

    @MainActor final class Coordinator: NSObject, UIScrollViewDelegate {
        let onScale: (CGFloat) -> Void
        var reset = -1
        init(onScale: @escaping (CGFloat) -> Void) { self.onScale = onScale }

        func viewForZooming(in scrollView: UIScrollView) -> UIView? { (scrollView as? PixivReaderScrollView)?.picture }
        func scrollViewDidZoom(_ scrollView: UIScrollView) {
            guard let scroll = scrollView as? PixivReaderScrollView else { return }
            scroll.centerPicture()
            onScale(scroll.zoomScale * (scroll.window?.screen.scale ?? UIScreen.main.scale))
        }

        @objc func doubleTap(_ recognizer: UITapGestureRecognizer) {
            guard let scroll = recognizer.view as? PixivReaderScrollView else { return }
            if scroll.zoomScale > scroll.minimumZoomScale * 1.01 {
                scroll.setZoomScale(scroll.minimumZoomScale, animated: true)
            } else {
                let scale = min(scroll.maximumZoomScale, max(1, scroll.minimumZoomScale * 2))
                let point = recognizer.location(in: scroll.picture)
                let size = CGSize(width: scroll.bounds.width / scale, height: scroll.bounds.height / scale)
                scroll.zoom(to: CGRect(x: point.x - size.width / 2, y: point.y - size.height / 2,
                                       width: size.width, height: size.height), animated: true)
            }
        }
    }
}

/// Image coordinates are pixels, so a high-resolution swap can preserve its on-screen size.
private final class PixivReaderScrollView: UIScrollView {
    let picture = UIImageView()
    var widthMode = false
    private var viewport = CGSize.zero
    private var needsFit = true

    func setImage(_ image: UIImage, reset: Bool) {
        if picture.image !== image {
            let previousWidth = picture.bounds.width
            let displayedWidth = previousWidth * zoomScale
            let previousHeight = picture.bounds.height
            let center = CGPoint(x: (contentOffset.x + bounds.width / 2 - picture.frame.minX) / max(1, displayedWidth),
                                 y: (contentOffset.y + bounds.height / 2 - picture.frame.minY) / max(1, previousHeight * zoomScale))
            setZoomScale(1, animated: false)
            picture.image = image
            let size = CGSize(width: CGFloat(image.cgImage?.width ?? Int(image.size.width * image.scale)),
                              height: CGFloat(image.cgImage?.height ?? Int(image.size.height * image.scale)))
            picture.frame = CGRect(origin: .zero, size: size)
            contentSize = size
            updateLimits()
            if previousWidth > 0 && !reset {
                setZoomScale(max(minimumZoomScale, min(maximumZoomScale, displayedWidth / max(1, size.width))), animated: false)
                centerPicture()
                contentOffset = CGPoint(x: max(0, min(max(0, contentSize.width - bounds.width), center.x * contentSize.width - bounds.width / 2)),
                                        y: max(0, min(max(0, contentSize.height - bounds.height), center.y * contentSize.height - bounds.height / 2)))
            } else { needsFit = true }
        }
        if reset { needsFit = true }
        setNeedsLayout()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard bounds.width > 0, bounds.height > 0, picture.bounds.width > 0 else { return }
        if viewport != bounds.size { viewport = bounds.size; needsFit = true }
        if needsFit {
            needsFit = false
            updateLimits()
            let target = widthMode ? min(1, bounds.width / picture.bounds.width) : minimumZoomScale
            setZoomScale(target, animated: false)
            contentOffset = .zero
        }
        centerPicture()
    }

    private func updateLimits() {
        guard picture.bounds.width > 0, picture.bounds.height > 0, bounds.width > 0, bounds.height > 0 else { return }
        minimumZoomScale = min(1, min(bounds.width / picture.bounds.width, bounds.height / picture.bounds.height))
        maximumZoomScale = 4
    }

    func centerPicture() {
        picture.center = CGPoint(x: max(bounds.width, contentSize.width) / 2, y: max(bounds.height, contentSize.height) / 2)
    }
}
