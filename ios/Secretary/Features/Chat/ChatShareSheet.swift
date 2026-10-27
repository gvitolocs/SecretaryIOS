import Foundation
import LinkPresentation
import UIKit
import UniformTypeIdentifiers

/// One resolved share item: the real words, link or file (never a link to the server), with the type the
/// share sheet announces, so ChatGPT and other apps receive the image, the audio, the PDF or the text itself.
struct ChatShareItem: Equatable {
    enum Payload: Equatable {
        case text(String)
        case url(URL)
        case file(URL)
    }

    let payload: Payload
    let type: UTType
    var name: String?
    var bytes: Int

    static func text(_ text: String) -> ChatShareItem {
        ChatShareItem(payload: .text(text), type: .plainText, name: nil, bytes: text.utf8.count)
    }

    static func link(_ url: URL) -> ChatShareItem {
        ChatShareItem(payload: .url(url), type: .url, name: url.host, bytes: url.absoluteString.utf8.count)
    }

    static func file(_ url: URL) -> ChatShareItem {
        let size = (try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0
        return ChatShareItem(payload: .file(url), type: ChatShareNaming.type(of: url), name: url.lastPathComponent, bytes: size)
    }

    var isImage: Bool { type.conforms(to: .image) }
    var isText: Bool { if case .text = payload { return true } else { return false } }
    var fileURL: URL? { if case let .file(url) = payload { return url } else { return nil } }
    var mime: String { type.preferredMIMEType ?? "application/octet-stream" }

    /// What the share sheet hands to the target app.
    var activityValue: Any {
        switch payload {
        case let .text(text): return text
        case let .url(url): return url
        case let .file(url): return url
        }
    }
}

/// Turns share parts into real items: the composer's own copy of a file while it is kept, else the original
/// from lucina-media (`/v1/media/items/{id}/original` with the device token, cached in Caches/ChatMedia),
/// else, for a picture, the preview rendition or the local preview.
@MainActor
enum ChatShareResolver {
    struct Failure: LocalizedError {
        let errorDescription: String?
    }

    /// `progress` goes 0...1 as the parts resolve. Cancelling the task stops the downloads.
    static func resolve(_ parts: [ChatSharePart], store: ChatStore, client: APIClient,
                        progress: @escaping @MainActor (Double) -> Void = { _ in }) async throws -> [ChatShareItem] {
        var out: [ChatShareItem] = []
        let total = Double(max(1, parts.count))
        for (index, part) in parts.enumerated() {
            try Task.checkCancellation()
            switch part {
            case let .text(text):
                out.append(.text(text))
            case let .link(url):
                out.append(.link(url))
            case let .media(media):
                if let file = try await file(for: media, store: store, client: client) { out.append(.file(file)) }
            }
            progress(Double(index + 1) / total)
        }
        try Task.checkCancellation()
        return out
    }

    static func file(for media: ChatShareMedia, store: ChatStore, client: APIClient) async throws -> URL? {
        let name = ChatShareNaming.fileName(for: media.attachment)
        if let local = store.localFile(shareId: media.shareId, fileIndex: media.fileIndex ?? -1) {
            return try named(local, name)
        }
        if let id = media.attachment.mediaId, !id.isEmpty {
            do {
                return try await ChatMedia.original(id, name: name, info: nil, client: client)
            } catch let error as URLError where error.code == .cancelled {
                throw CancellationError()
            } catch {
                try Task.checkCancellation()
                guard media.kind == .image else { throw Failure(errorDescription: String(localized: "Couldn't download \(name).")) }
                if let image = await ChatMedia.image(id, rendition: .preview, info: nil, client: client) {
                    return try jpeg(image, name: name)
                }
            }
        }
        if media.kind == .image, let preview = ChatPreviews.image(shareId: media.shareId, index: media.fileIndex ?? 0) {
            return try jpeg(preview, name: name)
        }
        return nil
    }

    /// Where share copies go: Caches/ChatShare/<uuid>/.
    private static func scratch() throws -> URL {
        guard let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first else {
            throw Failure(errorDescription: String(localized: "Couldn't prepare the share."))
        }
        let folder = caches.appendingPathComponent("ChatShare", isDirectory: true).appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }

    /// The composer's copy is called `<index>-<name>` on disk: share it under the owner's file name.
    private static func named(_ url: URL, _ name: String) throws -> URL {
        guard url.lastPathComponent != name else { return url }
        let target = try scratch().appendingPathComponent(name)
        try FileManager.default.copyItem(at: url, to: target)
        return target
    }

    private static func jpeg(_ image: UIImage, name: String) throws -> URL {
        guard let data = image.jpegData(compressionQuality: 0.9) else {
            throw Failure(errorDescription: String(localized: "Couldn't prepare the share."))
        }
        let base = (name as NSString).deletingPathExtension
        let target = try scratch().appendingPathComponent((base.isEmpty ? "photo" : base) + ".jpg")
        try data.write(to: target, options: .atomic)
        return target
    }

    /// Share copies from earlier sessions.
    static func purgeScratch() {
        guard let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first else { return }
        try? FileManager.default.removeItem(at: caches.appendingPathComponent("ChatShare", isDirectory: true))
    }
}

/// One item for UIActivityViewController: the placeholder tells the sheet the type at once (so text targets
/// and image targets both show up), the item is the real value, and the preview row shows a title and, for a
/// picture, the picture.
final class ChatActivityItemSource: NSObject, UIActivityItemSource {
    let item: ChatShareItem

    init(_ item: ChatShareItem) {
        self.item = item
    }

    func activityViewControllerPlaceholderItem(_ activityViewController: UIActivityViewController) -> Any {
        item.activityValue
    }

    func activityViewController(_ activityViewController: UIActivityViewController, itemForActivityType activityType: UIActivity.ActivityType?) -> Any? {
        item.activityValue
    }

    func activityViewController(_ activityViewController: UIActivityViewController,
                                dataTypeIdentifierForActivityType activityType: UIActivity.ActivityType?) -> String {
        item.type.identifier
    }

    func activityViewController(_ activityViewController: UIActivityViewController, subjectForActivityType activityType: UIActivity.ActivityType?) -> String {
        "Lucina"
    }

    func activityViewControllerLinkMetadata(_ activityViewController: UIActivityViewController) -> LPLinkMetadata? {
        let metadata = LPLinkMetadata()
        switch item.payload {
        case let .text(text):
            metadata.title = String(ChatPlainText.oneLine(text).prefix(120))
        case let .url(url):
            metadata.originalURL = url
            metadata.url = url
            metadata.title = url.host ?? url.absoluteString
        case let .file(url):
            metadata.title = url.lastPathComponent
            if item.isImage { metadata.imageProvider = NSItemProvider(contentsOf: url) }
            metadata.iconProvider = NSItemProvider(contentsOf: url)
        }
        return metadata
    }
}

/// Presents the system share sheet from UIKit (a SwiftUI sheet around UIActivityViewController shows up
/// half empty), over whatever is on screen.
@MainActor
enum ChatSharePresenter {
    @discardableResult
    static func present(_ items: [ChatShareItem], completion: (() -> Void)? = nil) -> Bool {
        guard !items.isEmpty, let top = topViewController() else { return false }
        let controller = UIActivityViewController(activityItems: items.map(ChatActivityItemSource.init), applicationActivities: nil)
        controller.completionWithItemsHandler = { _, _, _, _ in completion?() }
        if let popover = controller.popoverPresentationController {
            popover.sourceView = top.view
            popover.sourceRect = CGRect(x: top.view.bounds.midX, y: top.view.bounds.midY, width: 1, height: 1)
            popover.permittedArrowDirections = []
        }
        top.present(controller, animated: true)
        return true
    }

    static func topViewController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let scene = scenes.first { $0.activationState == .foregroundActive } ?? scenes.first
        let window = scene?.windows.first(where: \.isKeyWindow) ?? scene?.windows.first
        var top = window?.rootViewController
        while let presented = top?.presentedViewController, !presented.isBeingDismissed { top = presented }
        return top
    }
}
