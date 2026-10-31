import SwiftUI
import UIKit

/// NClient never enters the shared picture loader's disk cache.
actor NClientImageLoader {
    private let cache = NSCache<NSURL, UIImage>()
    private var pending: [URL: Task<UIImage?, Never>] = [:]
    private let session: URLSession

    init() {
        let config = URLSessionConfiguration.ephemeral
        config.urlCache = nil
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.httpShouldSetCookies = false
        config.httpCookieAcceptPolicy = .never
        session = URLSession(configuration: config, delegate: NClientImageSessionDelegate(), delegateQueue: nil)
        cache.totalCostLimit = 100 * 1024 * 1024
    }

    func image(_ url: URL) async -> UIImage? {
        if let image = cache.object(forKey: url as NSURL) { return image }
        if let task = pending[url] { return await task.value }
        let session = session
        let task = Task<UIImage?, Never> {
            guard let (data, response) = try? await session.data(from: url),
                  let response = response as? HTTPURLResponse, (200..<300).contains(response.statusCode) else { return nil }
            return UIImage(data: data)
        }
        pending[url] = task
        let image = await task.value
        pending[url] = nil
        if let image {
            cache.setObject(image, forKey: url as NSURL, cost: Int(image.size.width * image.size.height * image.scale * image.scale * 4))
        }
        return image
    }
}

struct NClientImage: View {
    let url: URL?
    let loader: NClientImageLoader
    var mode: ContentMode = .fill
    @State private var image: UIImage?
    @State private var failed = false
    var body: some View {
        Group {
            if let image { Image(uiImage: image).resizable().aspectRatio(contentMode: mode) }
            else if failed { Image(systemName: "photo").frame(maxWidth: .infinity, maxHeight: .infinity).background(Theme.field) }
            else { ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity).background(Theme.field) }
        }
        .task(id: url) {
            image = nil; failed = false
            guard let url else { failed = true; return }
            image = await loader.image(url)
            failed = image == nil
        }
    }
}

/// UIKit's scroll view provides bounded pinch zoom, pan, and double-tap zoom on iOS 17.
struct NClientZoomPage: UIViewRepresentable {
    let image: UIImage
    let tap: () -> Void

    func makeCoordinator() -> Coordinator { Coordinator(tap: tap) }
    func makeUIView(context: Context) -> UIScrollView {
        let scroll = UIScrollView()
        scroll.minimumZoomScale = 1; scroll.maximumZoomScale = 4
        scroll.showsVerticalScrollIndicator = false; scroll.showsHorizontalScrollIndicator = false
        scroll.delegate = context.coordinator
        let imageView = context.coordinator.imageView
        imageView.contentMode = .scaleAspectFit
        imageView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        imageView.frame = scroll.bounds
        scroll.addSubview(imageView)
        let single = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.singleTap))
        let double = UITapGestureRecognizer(target: context.coordinator, action: #selector(Coordinator.doubleTap(_:)))
        double.numberOfTapsRequired = 2
        single.require(toFail: double)
        scroll.addGestureRecognizer(single); scroll.addGestureRecognizer(double)
        return scroll
    }
    func updateUIView(_ scroll: UIScrollView, context: Context) {
        context.coordinator.imageView.image = image
        if scroll.zoomScale == 1 { context.coordinator.imageView.frame = scroll.bounds }
    }
    final class Coordinator: NSObject, UIScrollViewDelegate {
        let imageView = UIImageView()
        let tap: () -> Void
        init(tap: @escaping () -> Void) { self.tap = tap }
        func viewForZooming(in scrollView: UIScrollView) -> UIView? { imageView }
        @objc func singleTap() { tap() }
        @objc func doubleTap(_ gesture: UITapGestureRecognizer) {
            guard let scroll = gesture.view as? UIScrollView else { return }
            if scroll.zoomScale > 1 { scroll.setZoomScale(1, animated: true) }
            else {
                let point = gesture.location(in: imageView)
                let size = CGSize(width: scroll.bounds.width / 2.5, height: scroll.bounds.height / 2.5)
                scroll.zoom(to: CGRect(x: point.x - size.width / 2, y: point.y - size.height / 2, width: size.width, height: size.height), animated: true)
            }
        }
    }
}

/// Applied separately to pushed screens and covers so no presented surface escapes the privacy mask.
struct NClientPrivacy: ViewModifier {
    @Environment(\.scenePhase) private var phase
    func body(content: Content) -> some View {
        content
            .opacity(phase == .active ? 1 : 0)
            .overlay { if phase != .active { Color.black.ignoresSafeArea().accessibilityHidden(true) } }
    }
}

/// A signed image must never redirect to an upstream or other unvalidated URL.
private final class NClientImageSessionDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}
