import Foundation
import UIKit
import UniformTypeIdentifiers
import SwiftUI
import PhotosUI

/// What one pasted or dropped item is.
enum PasteKind: Equatable {
    case image(UTType)
    case file(UTType)
    case url
    case text
}

/// Everything a paste, drop or picker produced: files for the tray, plus text and links.
struct LoadedAttachments {
    var attachments: [PendingAttachment] = []
    var texts: [String] = []
    var urls: [String] = []
}

/// The loaders behind the tray: item providers (paste, drag and drop, the share sheet), PhotosPicker items,
/// document picker URLs, the pasteboard and the camera. Every file is copied into `AttachmentStore`.
enum AttachmentLoader {
    private static let genericTypes: Set<UTType> = [.data, .item, .content, .text, .compositeContent, .url, .fileURL]

    /// Images win, then real files (Files copies, PDFs, documents), then links, then text. Styled text
    /// (RTF/HTML from Notes or Safari) stays text.
    static func classify(_ provider: NSItemProvider) -> PasteKind? {
        let types = provider.registeredTypeIdentifiers.compactMap { UTType($0) }
        if let image = types.first(where: { $0.conforms(to: .image) && $0 != .image }) ?? types.first(where: { $0 == .image }) {
            return .image(image)
        }
        let concrete = types.first { type in
            type.conforms(to: .data) && !type.conforms(to: .text) && !type.conforms(to: .url) && !genericTypes.contains(type)
        }
        if types.contains(.fileURL) { return .file(concrete ?? types.first { !genericTypes.contains($0) } ?? .data) }
        if let concrete { return .file(concrete) }
        if types.contains(where: { $0.conforms(to: .url) }) { return .url }
        if types.contains(where: { $0.conforms(to: .text) }) { return .text }
        if let first = types.first(where: { $0.conforms(to: .data) }) { return .file(first) }
        return nil
    }

    /// A paste with at least one image or file goes to the tray; plain text and links paste as text.
    static func shouldAttach(_ providers: [NSItemProvider]) -> Bool {
        providers.contains { provider in
            switch classify(provider) {
            case .image, .file: return true
            case .url, .text, nil: return false
            }
        }
    }

    // MARK: Item providers (paste, drop)

    static func load(providers: [NSItemProvider]) async throws -> LoadedAttachments {
        let directory = try AttachmentStore.pendingDirectory()
        var loaded = LoadedAttachments()
        for provider in providers {
            switch classify(provider) {
            case .image(let type), .file(let type):
                let file: ShareFile
                if type == .data, provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier),
                   let url = try await fileURL(provider) {
                    file = try ShareItemReader.copiedFile(url, directory: directory)
                } else {
                    file = try await copy(provider, type: type, directory: directory)
                }
                loaded.attachments.append(await PendingAttachment.make(from: file))
            case .url:
                let value = try await provider.loadItem(forTypeIdentifier: UTType.url.identifier)
                let url = (value as? URL) ?? (value as? NSURL).map { $0 as URL } ?? (value as? Data).flatMap { URL(dataRepresentation: $0, relativeTo: nil) }
                if let url, url.isFileURL {
                    let file = try ShareItemReader.copiedFile(url, directory: directory)
                    loaded.attachments.append(await PendingAttachment.make(from: file))
                } else if let url {
                    loaded.urls.append(url.absoluteString)
                } else if let text = value as? String {
                    if let web = ShareItemReader.webURL(text) { loaded.urls.append(web.absoluteString) } else { loaded.texts.append(text) }
                }
            case .text:
                let value = try await provider.loadItem(forTypeIdentifier: UTType.plainText.identifier)
                let text = (value as? String) ?? (value as? NSAttributedString)?.string ?? (value as? Data).map { String(decoding: $0, as: UTF8.self) }
                if let text, !text.isEmpty {
                    if let web = ShareItemReader.webURL(text) { loaded.urls.append(web.absoluteString) } else { loaded.texts.append(text) }
                }
            case nil:
                continue
            }
        }
        return loaded
    }

    private static func fileURL(_ provider: NSItemProvider) async throws -> URL? {
        let value = try await provider.loadItem(forTypeIdentifier: UTType.fileURL.identifier)
        if let url = value as? URL { return url }
        if let data = value as? Data { return URL(dataRepresentation: data, relativeTo: nil) }
        return nil
    }

    /// The provider's file, copied inside the callback (its temporary URL expires as it returns). In-memory
    /// items (a screenshot on the pasteboard) fall back to their data.
    static func copy(_ provider: NSItemProvider, type: UTType, directory: URL) async throws -> ShareFile {
        let suggested = provider.suggestedName
        do {
            return try await withCheckedThrowingContinuation { continuation in
                provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, error in
                    guard let url else { continuation.resume(throwing: error ?? ShareFailure.preparation); return }
                    do {
                        continuation.resume(returning: try ShareItemReader.copiedFile(url, name: name(suggested, url: url, type: type),
                                                                                     type: type, directory: directory))
                    } catch { continuation.resume(throwing: error) }
                }
            }
        } catch ShareFailure.tooLarge {
            throw ShareFailure.tooLarge
        } catch {
            let data: Data = try await withCheckedThrowingContinuation { continuation in
                provider.loadDataRepresentation(forTypeIdentifier: type.identifier) { data, error in
                    if let data { continuation.resume(returning: data) } else { continuation.resume(throwing: error ?? ShareFailure.preparation) }
                }
            }
            return try write(data, name: name(suggested, url: nil, type: type), type: type, directory: directory)
        }
    }

    private static func name(_ suggested: String?, url: URL?, type: UTType) -> String {
        let ext = type.preferredFilenameExtension ?? url?.pathExtension ?? ""
        if let suggested, !suggested.isEmpty {
            return (suggested as NSString).pathExtension.isEmpty && !ext.isEmpty ? suggested + "." + ext : suggested
        }
        if let url, !url.lastPathComponent.isEmpty, url.lastPathComponent != "/" { return url.lastPathComponent }
        let base = type.conforms(to: .image) ? "Image" : "File"
        return ext.isEmpty ? base : base + "." + ext
    }

    static func write(_ data: Data, name: String, type: UTType?, directory: URL) throws -> ShareFile {
        guard Int64(data.count) <= ShareRequestBuilder.maxBytes else { throw ShareFailure.tooLarge }
        let ext = (name as NSString).pathExtension
        let target = directory.appendingPathComponent(UUID().uuidString).appendingPathExtension(ext)
        try data.write(to: target, options: .completeFileProtectionUntilFirstUserAuthentication)
        let mime = type?.preferredMIMEType ?? UTType(filenameExtension: ext)?.preferredMIMEType ?? "application/octet-stream"
        return try ShareItemReader.normalize(ShareFile(url: target, name: name, mime: mime))
    }

    // MARK: Pasteboard

    static func load(pasteboard: UIPasteboard = .general) async throws -> LoadedAttachments {
        try await load(providers: pasteboard.itemProviders)
    }

    // MARK: Document picker

    static func load(fileURLs: [URL]) async throws -> [PendingAttachment] {
        let directory = try AttachmentStore.pendingDirectory()
        var out: [PendingAttachment] = []
        for url in fileURLs {
            let file = try ShareItemReader.copiedFile(url, directory: directory)
            out.append(await PendingAttachment.make(from: file))
        }
        return out
    }

    // MARK: PhotosPicker

    static func load(pickerItems: [PhotosPickerItem]) async throws -> [PendingAttachment] {
        var out: [PendingAttachment] = []
        for item in pickerItems {
            let isMovie = item.supportedContentTypes.contains { $0.conforms(to: .movie) }
            let picked: PickedFile?
            if isMovie { picked = try await item.loadTransferable(type: PickedMovie.self).map { PickedFile(url: $0.url) } }
            else { picked = try await item.loadTransferable(type: PickedFile.self) }
            guard let picked else { continue }
            let type = UTType(filenameExtension: picked.url.pathExtension)
            let file = try ShareItemReader.normalize(ShareFile(url: picked.url, name: picked.url.lastPathComponent.components(separatedBy: "__").last ?? picked.url.lastPathComponent,
                                                              mime: type?.preferredMIMEType ?? "application/octet-stream"))
            out.append(await PendingAttachment.make(from: file))
        }
        return out
    }

    /// Copies a transferred file into the pending folder as `<uuid>__<original name>`.
    fileprivate static func adopt(_ received: URL) throws -> URL {
        let directory = try AttachmentStore.pendingDirectory()
        let size = (try? received.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
        guard Int64(size) <= ShareRequestBuilder.maxBytes else { throw ShareFailure.tooLarge }
        let target = directory.appendingPathComponent(UUID().uuidString + "__" + received.lastPathComponent)
        try FileManager.default.copyItem(at: received, to: target)
        return target
    }

    // MARK: Camera

    static func load(cameraImage image: UIImage) async throws -> PendingAttachment {
        guard let data = image.jpegData(compressionQuality: 0.9) else { throw ShareFailure.preparation }
        let name = "Photo " + Self.stamp() + ".jpg"
        let file = try write(data, name: name, type: .jpeg, directory: try AttachmentStore.pendingDirectory())
        var attachment = await PendingAttachment.make(from: file)
        attachment.takenAt = Date()
        return attachment
    }

    static func load(cameraMovie url: URL) async throws -> PendingAttachment {
        let file = try ShareItemReader.copiedFile(url, name: "Video " + Self.stamp() + "." + (url.pathExtension.isEmpty ? "mov" : url.pathExtension),
                                                 directory: try AttachmentStore.pendingDirectory())
        var attachment = await PendingAttachment.make(from: file)
        attachment.takenAt = Date()
        return attachment
    }

    private static func stamp() -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH.mm.ss"
        return formatter.string(from: Date())
    }
}

/// A photo from PhotosPicker, as its original file (HEIC is converted later by `normalize`).
struct PickedFile: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .image) { received in
            PickedFile(url: try AttachmentLoader.adopt(received.file))
        }
    }
}

/// A video from PhotosPicker.
struct PickedMovie: Transferable {
    let url: URL

    static var transferRepresentation: some TransferRepresentation {
        FileRepresentation(importedContentType: .movie) { received in
            PickedMovie(url: try AttachmentLoader.adopt(received.file))
        }
    }
}
