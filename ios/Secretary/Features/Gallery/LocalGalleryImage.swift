import CoreGraphics
import Foundation
import Observation
import os
@preconcurrency import Photos
import UIKit

private let localImageLog = Logger(subsystem: "tech.getsecretary.app", category: "GalleryImages")

/// PhotoKit work and request cancellation share a serial queue, away from scrolling and gestures.
private final class LocalPhotoRequest: @unchecked Sendable {
    private let queue = DispatchQueue(label: "tech.getsecretary.app.gallery.photos", qos: .userInitiated)
    private let manager = PHImageManager.default()
    private var requests: [PHImageRequestID] = []
    private var cancelled = false

    typealias Receive = @Sendable (CGImage?, CGSize, Int, Bool) -> Void

    func start(_ identifier: String, size: CGSize, scale: CGFloat, tile: Bool, receive: @escaping Receive) {
        queue.async {
            guard !self.cancelled else { return }
            let access = PHPhotoLibrary.authorizationStatus(for: .readWrite)
            guard access == .authorized || access == .limited,
                  let asset = PHAsset.fetchAssets(withLocalIdentifiers: [identifier], options: nil).firstObject,
                  asset.mediaType == .image else {
                receive(nil, .zero, 0, true)
                return
            }
            if tile {
                self.request(asset, size: size, mode: .opportunistic, tile: true, stage: 0, receive: receive)
            } else {
                // The small image can appear immediately while PhotoKit renders the screen-sized one.
                self.request(asset, size: CGSize(width: size.width / scale, height: size.height / scale),
                             mode: .fastFormat, tile: false, stage: 0, receive: receive)
                self.request(asset, size: size, mode: .highQualityFormat, tile: false, stage: 1, receive: receive)
            }
        }
    }

    func original(_ identifier: String, receive: @escaping Receive) {
        queue.async {
            let access = PHPhotoLibrary.authorizationStatus(for: .readWrite)
            guard !self.cancelled, access == .authorized || access == .limited,
                  let asset = PHAsset.fetchAssets(withLocalIdentifiers: [identifier], options: nil).firstObject else { return }
            // Enough for the deepest zoom without decoding a 24-48 MP original (100-200 MB of pixels).
            self.request(asset, size: CGSize(width: 4096, height: 4096), mode: .highQualityFormat,
                         tile: false, stage: 2, receive: receive)
        }
    }

    private func request(_ asset: PHAsset, size: CGSize, mode: PHImageRequestOptionsDeliveryMode,
                         tile: Bool, stage: Int, receive: @escaping Receive) {
        let options = PHImageRequestOptions()
        options.deliveryMode = mode
        options.resizeMode = mode == .highQualityFormat ? .exact : .fast
        options.isSynchronous = false
        options.isNetworkAccessAllowed = false
        let pixels = CGSize(width: asset.pixelWidth, height: asset.pixelHeight)
        let id = manager.requestImage(for: asset, targetSize: size, contentMode: tile ? .aspectFill : .aspectFit,
                                      options: options) { image, info in
            let degraded = info?[PHImageResultIsDegradedKey] as? Bool == true
            let finished = mode == .fastFormat || !degraded
            // PhotoKit can call back on any queue; orientation rendering also stays off the main thread.
            self.queue.async {
                guard !self.cancelled else { return }
                var cgImage = image?.cgImage
                if let image, image.imageOrientation != .up {
                    let format = UIGraphicsImageRendererFormat()
                    format.scale = 1
                    let size = CGSize(width: image.size.width * image.scale, height: image.size.height * image.scale)
                    let renderer = UIGraphicsImageRenderer(size: size, format: format)
                    cgImage = renderer.image { _ in image.draw(in: CGRect(origin: .zero, size: size)) }.cgImage
                }
                receive(cgImage, pixels, stage, finished)
            }
        }
        requests.append(id)
    }

    func cancel() {
        queue.async {
            self.cancelled = true
            for id in self.requests { self.manager.cancelImageRequest(id) }
            self.requests.removeAll()
        }
    }
}

/// One view's local image, with generation checks so cancelled PhotoKit callbacks cannot replace it.
@Observable
@MainActor
final class LocalGalleryImage {
    private(set) var image: CGImage?
    private(set) var pixelSize: CGSize = .zero
    private(set) var unavailable = false
    private(set) var previewPixels: CGFloat = 1440
    @ObservationIgnored private var worker: LocalPhotoRequest?
    @ObservationIgnored private var generation = UUID()
    @ObservationIgnored private var stage = -1
    @ObservationIgnored private var finished: Set<Int> = []
    @ObservationIgnored private var originalRequested = false
    @ObservationIgnored private var identifier: String?
    @ObservationIgnored private var tile = false
    @ObservationIgnored private var targetSize = CGSize.zero
    @ObservationIgnored private var logged = false

    func load(_ identifier: String?, size: CGSize, tile: Bool) {
        guard worker == nil else { return }
        guard let identifier else { unavailable = true; return }
        self.identifier = identifier
        self.tile = tile
        targetSize = size
        unavailable = false
        finished = []
        originalRequested = false
        let generation = UUID()
        self.generation = generation
        let worker = LocalPhotoRequest()
        self.worker = worker
        worker.start(identifier, size: size, scale: UIScreen.main.scale, tile: tile, receive: receiver(generation))
    }

    private func receiver(_ generation: UUID) -> LocalPhotoRequest.Receive {
        { [weak self] image, size, stage, finished in
            Task { @MainActor in
                guard let self, self.generation == generation else { return }
                if let image, stage >= self.stage {
                    self.image = image
                    self.pixelSize = size
                    if size.width > 0, size.height > 0 {
                        let fit = min(1, min(self.targetSize.width / size.width, self.targetSize.height / size.height))
                        self.previewPixels = max(size.width, size.height) * fit
                    }
                    self.stage = stage
                    if !self.tile, !self.logged {
                        self.logged = true
                        localImageLog.notice("GalleryImages source=local asset=\(self.identifier ?? "", privacy: .public)")
                    }
                }
                if finished { self.finished.insert(stage) }
                if self.image == nil, self.finished.contains(0), self.tile || self.finished.contains(1) || size == .zero {
                    self.unavailable = true
                }
                // Only a small local copy (iCloud "Optimise iPhone Storage"): the server preview is sharper.
                if !self.tile, stage == 1, finished, image == nil { self.unavailable = true }
            }
        }
    }

    func requestOriginal() {
        guard image != nil, !originalRequested, let identifier, let worker else { return }
        originalRequested = true
        worker.original(identifier, receive: receiver(generation))
    }

    func cancel() {
        generation = UUID()
        worker?.cancel()
        worker = nil
    }
}
