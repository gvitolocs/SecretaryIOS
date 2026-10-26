import Foundation
import UniformTypeIdentifiers
import UIKit
import ImageIO

/// Copies inside the callback: NSItemProvider's temporary URLs expire as it returns.
enum ShareItemReader {
    static func webURL(_ text: String) -> URL? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.rangeOfCharacter(from: .whitespacesAndNewlines) == nil, let url = URL(string: trimmed),
              ["https", "http"].contains(url.scheme?.lowercased() ?? ""), url.host != nil else { return nil }
        return url
    }

    static func addText(_ text: String, to payload: inout SharePayload) {
        if let url = webURL(text) { payload.urls.append(url.absoluteString) }
        else if !text.isEmpty { payload.texts.append(text) }
    }

    static func copiedFile(_ url: URL, name: String? = nil, type: UTType? = nil, directory: URL) throws -> ShareFile {
        let originalName = name ?? url.lastPathComponent
        let resolved = type ?? UTType(filenameExtension: url.pathExtension)
        let target = directory.appendingPathComponent(UUID().uuidString).appendingPathExtension(url.pathExtension)
        let access = url.startAccessingSecurityScopedResource()
        defer { if access { url.stopAccessingSecurityScopedResource() } }
        let size = (try url.resourceValues(forKeys: [.fileSizeKey])).fileSize ?? 0
        guard Int64(size) <= ShareRequestBuilder.maxBytes else { throw ShareFailure.tooLarge }
        try FileManager.default.copyItem(at: url, to: target)
        do { return try normalize(ShareFile(url: target, name: originalName, mime: resolved?.preferredMIMEType ?? UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream")) }
        catch { try? FileManager.default.removeItem(at: target); throw error }
    }

    static func normalize(_ file: ShareFile) throws -> ShareFile {
        guard ["heic", "heif"].contains(file.url.pathExtension.lowercased()) || ["image/heic", "image/heif"].contains(file.mime) else { return file }
        guard let source = CGImageSourceCreateWithURL(file.url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary) else { throw ShareFailure.preparation }
        let output = file.url.deletingPathExtension().appendingPathExtension("jpg")
        guard let destination = CGImageDestinationCreateWithURL(output as CFURL, UTType.jpeg.identifier as CFString, 1, nil) else { throw ShareFailure.preparation }
        let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] ?? [:]
        CGImageDestinationAddImageFromSource(destination, source, 0, [kCGImageDestinationLossyCompressionQuality: 0.9,
                                                       kCGImagePropertyOrientation: properties[kCGImagePropertyOrientation] ?? 1] as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { throw ShareFailure.preparation }
        try? FileManager.default.removeItem(at: file.url)
        return ShareFile(url: output, name: (file.name as NSString).deletingPathExtension + ".jpg", mime: "image/jpeg")
    }

    static func read(_ items: [NSExtensionItem], directory: URL) async throws -> SharePayload {
        var payload = SharePayload()
        for item in items {
            if payload.title.isEmpty { payload.title = item.attributedTitle?.string ?? item.attributedContentText?.string ?? "" }
            for provider in item.attachments ?? [] {
                // Real file types win over their generic URL or text representations (including vCards).
                let types = provider.registeredTypeIdentifiers.compactMap { UTType($0) }
                let concrete = types.first { type in
                    type.conforms(to: .data) && !type.conforms(to: .plainText) && !type.conforms(to: .url)
                        && ![UTType.data, .item, .content, .text].contains(type)
                }
                if let type = concrete {
                    payload.files.append(try await file(provider, type: type, directory: directory))
                    if type.conforms(to: .vCard), provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
                        let value = try await provider.loadItem(forTypeIdentifier: UTType.url.identifier)
                        if let url = value as? URL, !url.isFileURL { addText(url.absoluteString, to: &payload) }
                        else if let text = value as? String, webURL(text) != nil { addText(text, to: &payload) }
                    }
                } else if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
                    let value = try await provider.loadItem(forTypeIdentifier: UTType.url.identifier)
                    let url = (value as? URL) ?? (value as? NSURL).map { $0 as URL }
                    if let url, url.isFileURL { payload.files.append(try copiedFile(url, directory: directory)) }
                    else if let url { addText(url.absoluteString, to: &payload) }
                    else if let text = value as? String { addText(text, to: &payload) }
                } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
                    let value = try await provider.loadItem(forTypeIdentifier: UTType.plainText.identifier)
                    if let text = value as? String { addText(text, to: &payload) }
                    else if let text = value as? NSAttributedString { addText(text.string, to: &payload) }
                } else if let type = types.first {
                    payload.files.append(try await file(provider, type: type, directory: directory))
                }
            }
            if (item.attachments ?? []).isEmpty, let text = item.attributedContentText?.string { addText(text, to: &payload) }
        }
        return payload
    }

    private static func file(_ provider: NSItemProvider, type: UTType, directory: URL) async throws -> ShareFile {
        try await withCheckedThrowingContinuation { continuation in
            provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, error in
                guard let url else { continuation.resume(throwing: error ?? ShareFailure.preparation); return }
                do { continuation.resume(returning: try copiedFile(url, type: type, directory: directory)) }
                catch { continuation.resume(throwing: error) }
            }
        }
    }
}
