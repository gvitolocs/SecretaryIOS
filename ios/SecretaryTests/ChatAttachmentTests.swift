import XCTest
import UIKit
import UniformTypeIdentifiers
@testable import Secretary

final class ThumbHashTests: XCTestCase {
    /// Solid red 20×10 (landscape), made with the reference encoder (evanw/thumbhash `rgbaToThumbHash`).
    private let red: [UInt8] = [213, 251, 3, 4, 128, 119, 104, 120, 135, 120, 120, 119, 136, 143, 7, 124, 116, 192, 71]
    /// A 10×16 (portrait) horizontal ramp from black to white, same encoder.
    private let ramp: [UInt8] = [31, 8, 30, 4, 0, 128, 119, 135, 120, 136, 120, 120, 136, 119, 0, 0, 0, 0, 0]

    func testDecodesASolidColourAtTheRightSize() throws {
        let decoded = try XCTUnwrap(ThumbHash.decode(red))
        XCTAssertEqual(decoded.width, 32)
        XCTAssertEqual(decoded.height, 18, "7:4 luminance grid → 32 × round(32 / 1.75)")
        XCTAssertEqual(decoded.pixels.count, 32 * 18 * 4)
        for offset in stride(from: 0, to: decoded.pixels.count, by: 4 * 37) {
            XCTAssertEqual(Int(decoded.pixels[offset]), 254, accuracy: 2)
            XCTAssertEqual(Int(decoded.pixels[offset + 1]), 0, accuracy: 2)
            XCTAssertEqual(Int(decoded.pixels[offset + 2]), 1, accuracy: 2)
            XCTAssertEqual(decoded.pixels[offset + 3], 255)
        }
        XCTAssertEqual(ThumbHash.approximateAspectRatio(red) ?? 0, 7.0 / 4.0, accuracy: 0.0001)
    }

    func testDecodesAPortraitRamp() throws {
        let decoded = try XCTUnwrap(ThumbHash.decode(ramp))
        XCTAssertEqual(decoded.width, 18)
        XCTAssertEqual(decoded.height, 32)
        let left = Int(decoded.pixels[0])
        let right = Int(decoded.pixels[(decoded.width - 1) * 4])
        XCTAssertLessThan(left, 60, "dark on the left")
        XCTAssertGreaterThan(right, 200, "light on the right")
        XCTAssertEqual(ThumbHash.approximateAspectRatio(ramp) ?? 0, 4.0 / 7.0, accuracy: 0.0001)
    }

    func testBase64AndImage() throws {
        let base64 = Data(red).base64EncodedString()
        XCTAssertEqual(base64, "1fsDBIB3aHiHeHh3iI8HfHTARw==")
        XCTAssertEqual(ThumbHash.decode(base64: base64), ThumbHash.decode(red))
        // Unpadded and URL-safe forms decode the same.
        XCTAssertEqual(ThumbHash.decode(base64: "1fsDBIB3aHiHeHh3iI8HfHTARw"), ThumbHash.decode(red))
        let image = try XCTUnwrap(ThumbHash.image(base64: base64))
        XCTAssertEqual(image.size, CGSize(width: 32, height: 18))
    }

    func testRejectsMalformedHashes() {
        XCTAssertNil(ThumbHash.decode([1, 2, 3]))
        XCTAssertNil(ThumbHash.decode(Array(red.prefix(8))), "too few AC bytes")
        XCTAssertNil(ThumbHash.decode(base64: "not base64!"))
    }
}

final class AlbumLayoutTests: XCTestCase {
    func testFramesForOneToTenPictures() {
        for count in 1...10 {
            let layout = AlbumLayout.layout(count: count, aspect: 0.75)
            XCTAssertEqual(layout.frames.count, count, "\(count) pictures")
            let bounds = CGRect(origin: .zero, size: layout.size)
            XCTAssertLessThanOrEqual(layout.size.width, AlbumLayout.width + 0.001)
            for (index, frame) in layout.frames.enumerated() {
                XCTAssertTrue(bounds.insetBy(dx: -0.001, dy: -0.001).contains(frame), "\(count): frame \(index) inside the bubble")
                XCTAssertGreaterThan(frame.width, 40)
                XCTAssertGreaterThan(frame.height, 40)
                for other in layout.frames[(index + 1)...] {
                    XCTAssertFalse(frame.insetBy(dx: 0.5, dy: 0.5).intersects(other), "\(count): no overlaps")
                }
            }
            // The tiles reach the bubble's right and bottom edges (no ragged mosaic).
            XCTAssertEqual(layout.frames.map(\.maxX).max() ?? 0, layout.size.width, accuracy: 0.5)
            XCTAssertEqual(layout.frames.map(\.maxY).max() ?? 0, layout.size.height, accuracy: 0.5)
        }
    }

    func testShapes() {
        let two = AlbumLayout.layout(count: 2)
        XCTAssertEqual(two.frames[0].minY, two.frames[1].minY, "side by side")
        let three = AlbumLayout.layout(count: 3)
        XCTAssertEqual(three.frames[0].height, three.size.height, "one big")
        XCTAssertEqual(three.frames[1].minX, three.frames[2].minX, "two stacked")
        let four = AlbumLayout.layout(count: 4)
        XCTAssertEqual(Set(four.frames.map(\.minY)).count, 2, "2 × 2")
        let seven = AlbumLayout.layout(count: 7)
        XCTAssertEqual(Set(seven.frames.map(\.minY)).count, 3, "rows of 3")
        XCTAssertEqual(seven.frames.last?.width ?? 0, AlbumLayout.width, accuracy: 0.001, "a lone last picture spans the row")
    }

    func testSinglePictureKeepsItsShapeWithinTheLimits() {
        let tall = AlbumLayout.layout(count: 1, aspect: 9.0 / 19.5)
        XCTAssertEqual(tall.size.height, AlbumLayout.maxHeight)
        XCTAssertEqual(tall.size.width, max(AlbumLayout.minSide, AlbumLayout.maxHeight * 9 / 19.5), accuracy: 0.001)
        let wide = AlbumLayout.layout(count: 1, aspect: 16.0 / 9.0)
        XCTAssertEqual(wide.size.width, AlbumLayout.width)
        XCTAssertEqual(wide.size.height, AlbumLayout.width * 9 / 16, accuracy: 0.001)
        XCTAssertEqual(AlbumLayout.layout(count: 0).frames.count, 0)
    }
}

final class ShareMetaTests: XCTestCase {
    private func directory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private let credentials = BridgeCredentials(token: "t", endpoints: ["https://example.com"], sources: ["share"])

    func testMetaJSONIsCompactSortedAndSkipsFilesWithoutFacts() throws {
        let meta = ShareMeta(files: [ShareMeta.File(index: 0, transcript: "ciao", durationMs: 4200),
                                     ShareMeta.File(index: 1),
                                     ShareMeta.File(index: 2, width: 1170, height: 2532, takenAt: "2026-10-25T09:00:00Z")],
                             location: ShareMeta.Location(lat: 55.5, lon: 12.25, name: "Home"))
        XCTAssertEqual(try meta.json(),
                       #"{"files":[{"durationMs":4200,"index":0,"transcript":"ciao"},{"height":2532,"index":2,"takenAt":"2026-10-25T09:00:00Z","width":1170}],"location":{"lat":55.5,"lon":12.25,"name":"Home"}}"#)
        XCTAssertTrue(ShareMeta(files: [ShareMeta.File(index: 0)]).isEmpty)
    }

    func testLongTranscriptsAreShortenedToFit() throws {
        let long = String(repeating: "parola ", count: 20_000)
        let json = try ShareMeta(files: [ShareMeta.File(index: 0, transcript: long)]).json()
        XCTAssertLessThanOrEqual(json.utf8.count, ShareMeta.maxBytes)
        XCTAssertTrue(json.contains("parola"))
    }

    func testChatMultipartCarriesSessionAndMeta() throws {
        let directory = try directory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("memo.m4a")
        try Data("abc".utf8).write(to: file)
        let meta = ShareMeta(files: [ShareMeta.File(index: 0, transcript: "compra il latte", durationMs: 1500)])
        let record = try ShareRequestBuilder.build(payload: SharePayload(files: [ShareFile(url: file, name: "memo.m4a", mime: "audio/mp4")]),
                                                   intent: "ask", note: "Cosa dice?", source: "ios-chat", credentials: credentials,
                                                   directory: directory, shareId: "s1", boundary: "b", language: "it",
                                                   sessionId: "ios-abc", meta: meta, origin: "chat")
        XCTAssertEqual(record.origin, "chat")
        let body = String(decoding: try Data(contentsOf: directory.appendingPathComponent("s1.body")), as: UTF8.self)
        func field(_ name: String, _ value: String) -> String {
            "--b\r\nContent-Disposition: form-data; name=\"\(name)\"\r\n\r\n\(value)\r\n"
        }
        XCTAssertTrue(body.contains(field("source", "ios-chat")))
        XCTAssertTrue(body.contains(field("note", "Cosa dice?")))
        XCTAssertTrue(body.contains(field("channel", "ios")))
        XCTAssertTrue(body.contains(field("sessionId", "ios-abc")))
        XCTAssertTrue(body.contains(field("meta", #"{"files":[{"durationMs":1500,"index":0,"transcript":"compra il latte"}]}"#)))
        XCTAssertTrue(body.contains("filename=\"memo.m4a\"\r\nContent-Type: audio/mp4\r\n\r\nabc\r\n"))
        let saved = try JSONDecoder().decode(ShareRecord.self, from: Data(contentsOf: directory.appendingPathComponent("s1.json")))
        XCTAssertEqual(saved.origin, "chat")
    }

    func testLocationOnlyShareHasAGeoPart() throws {
        let directory = try directory()
        defer { try? FileManager.default.removeItem(at: directory) }
        _ = try ShareRequestBuilder.build(payload: SharePayload(), intent: "ask", note: "", source: "ios-chat", credentials: credentials,
                                          directory: directory, shareId: "loc", boundary: "b",
                                          meta: ShareMeta(location: ShareMeta.Location(lat: 55.5, lon: 12.25, name: nil)))
        let body = String(decoding: try Data(contentsOf: directory.appendingPathComponent("loc.body")), as: UTF8.self)
        XCTAssertTrue(body.contains("name=\"text\"\r\n\r\ngeo:55.5,12.25\r\n"))
        XCTAssertTrue(body.contains(#"{"location":{"lat":55.5,"lon":12.25}}"#))
    }

    func testPendingAttachmentMeta() {
        let url = URL(fileURLWithPath: "/tmp/x.jpg")
        let attachments = [PendingAttachment(url: url, name: "x.jpg", mime: "image/jpeg", width: 4032, height: 3024),
                           PendingAttachment(url: url, name: "notes.pdf", mime: "application/pdf"),
                           PendingAttachment(url: url, name: "memo.m4a", mime: "audio/mp4", transcript: "ciao", durationMs: 3000)]
        XCTAssertEqual(attachments.map(\.kind), [.image, .pdf, .audio])
        let meta = PendingAttachment.meta(for: attachments)
        XCTAssertEqual(meta.files.map(\.index), [0, 1, 2])
        XCTAssertEqual(meta.files[0].width, 4032)
        XCTAssertFalse(meta.files[1].hasFacts)
        XCTAssertEqual(meta.files[2].transcript, "ciao")
    }

    func testKindClassification() {
        let cases: [(String, String, AttachmentKind)] = [
            ("IMG_1.HEIC", "image/heic", .image), ("clip.mov", "video/quicktime", .video), ("song.mp3", "audio/mpeg", .audio),
            ("bill.pdf", "application/pdf", .pdf), ("cv.docx", "application/octet-stream", .document),
            ("sheet.xlsx", "application/octet-stream", .spreadsheet), ("deck.pptx", "application/octet-stream", .presentation),
            ("main.rs", "application/octet-stream", .code), ("backup.zip", "application/zip", .archive),
            ("anna.vcf", "text/vcard", .contact), ("party.ics", "text/calendar", .calendar), ("readme.md", "text/markdown", .text),
            ("blob.zz9q", "application/octet-stream", .file)
        ]
        for (name, mime, kind) in cases { XCTAssertEqual(AttachmentKind.classify(mime: mime, filename: name), kind, name) }
    }
}

final class PasteClassificationTests: XCTestCase {
    private func png() -> Data {
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        return UIGraphicsImageRenderer(size: CGSize(width: 2, height: 2), format: format).pngData { context in
            UIColor.red.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 2, height: 2))
        }
    }

    func testImageFileTextAndLink() {
        let image = NSItemProvider(item: png() as NSData, typeIdentifier: UTType.png.identifier)
        let pdf = NSItemProvider(item: Data("%PDF-1.4".utf8) as NSData, typeIdentifier: UTType.pdf.identifier)
        let text = NSItemProvider(item: "hello Lucina" as NSString, typeIdentifier: UTType.plainText.identifier)
        let link = NSItemProvider(item: URL(string: "https://example.com/a")! as NSURL, typeIdentifier: UTType.url.identifier)
        let styled = NSItemProvider(item: Data("{\\rtf1 hi}".utf8) as NSData, typeIdentifier: UTType.rtf.identifier)
        XCTAssertEqual(AttachmentLoader.classify(image), .image(.png))
        XCTAssertEqual(AttachmentLoader.classify(pdf), .file(.pdf))
        XCTAssertEqual(AttachmentLoader.classify(text), .text)
        XCTAssertEqual(AttachmentLoader.classify(link), .url)
        XCTAssertEqual(AttachmentLoader.classify(styled), .text, "styled text pastes as text, not as an RTF file")

        XCTAssertTrue(AttachmentLoader.shouldAttach([image]))
        XCTAssertTrue(AttachmentLoader.shouldAttach([text, pdf]), "a file among the items goes to the tray")
        XCTAssertFalse(AttachmentLoader.shouldAttach([text]))
        XCTAssertFalse(AttachmentLoader.shouldAttach([link, text]))
    }

    func testPastedScreenshotLoadsIntoTheTray() async throws {
        let image = NSItemProvider(item: png() as NSData, typeIdentifier: UTType.png.identifier)
        let text = NSItemProvider(item: "caption" as NSString, typeIdentifier: UTType.plainText.identifier)
        let loaded = try await AttachmentLoader.load(providers: [image, text])
        defer { loaded.attachments.forEach { try? FileManager.default.removeItem(at: $0.url) } }
        XCTAssertEqual(loaded.attachments.count, 1)
        XCTAssertEqual(loaded.attachments.first?.kind, .image)
        XCTAssertEqual(loaded.attachments.first?.width, 2)
        XCTAssertEqual(loaded.texts, ["caption"])
    }
}

final class ChatShareReconcileTests: XCTestCase {
    private func share(_ id: String = "S1") -> OutgoingShare {
        OutgoingShare(shareId: id, caption: "Look",
                      files: [OutgoingShare.File(fileName: "0-a.jpg", name: "a.jpg", mime: "image/jpeg", kind: "image", size: 10, width: 3, height: 4),
                              OutgoingShare.File(fileName: "1-b.pdf", name: "b.pdf", mime: "application/pdf", kind: "pdf", size: 20)],
                      urls: ["https://example.com"], location: nil, createdAt: Date(timeIntervalSince1970: 100))
    }

    func testLocalBubble() {
        let item = share().localItem(ts: Date(timeIntervalSince1970: 100))
        XCTAssertEqual(item.id, "local-S1")
        XCTAssertEqual(item.localStatus, .sending)
        XCTAssertEqual(item.meta?.kind, "share")
        XCTAssertEqual(item.meta?.shareId, "S1")
        XCTAssertEqual(item.attachments.map(\.kind), ["image", "pdf", "link"])
        XCTAssertEqual(item.attachments.first?.width, 3)
        XCTAssertEqual(share().meta.files.map(\.index), [0, 1])
    }

    func testAcceptedTurnsTheBubbleIntoTheServerItemKeepingAttachments() {
        let items = [share().localItem(ts: Date(timeIntervalSince1970: 100))]
        let accepted = ShareAccepted(shareId: "S1", userItemId: "tl_42",
                                     attachments: [ShareStatus.Attachment(index: 1, kind: "pdf", mediaId: "m2"),
                                                   ShareStatus.Attachment(index: 0, kind: "image", mediaId: "m1", thumbhash: "abc")])
        let out = ShareReconcile.accept(items, shareId: "S1", accepted: accepted)
        XCTAssertEqual(out.map(\.id), ["tl_42"])
        XCTAssertNil(out[0].localStatus)
        XCTAssertEqual(out[0].meta?.shareId, "S1", "local previews stay keyed by the share")
        XCTAssertEqual(out[0].attachments.map(\.mediaId), ["m1", "m2", nil])
        XCTAssertEqual(out[0].attachments[0].thumbhash, "abc")
        XCTAssertEqual(out[0].ts, Date(timeIntervalSince1970: 100), "no jump in the transcript")
    }

    func testAcceptedWithoutAnItemIdMarksSent() {
        let out = ShareReconcile.accept([share().localItem()], shareId: "S1", accepted: nil)
        XCTAssertEqual(out.first?.id, "local-S1")
        XCTAssertEqual(out.first?.localStatus, .sent)
    }

    func testTheTimelineCopyWinsWhenItArrivedFirst() {
        var server = TimelineItem(id: "tl_42", ts: Date(timeIntervalSince1970: 101), channel: .ios, role: .user, text: "Look")
        server.meta = TimelineMeta(kind: "share")
        server.meta?.shareId = "S1"
        let out = ShareReconcile.accept([share().localItem(), server], shareId: "S1",
                                        accepted: ShareAccepted(userItemId: "tl_42"))
        XCTAssertEqual(out.map(\.id), ["tl_42"])
        // And the merge itself drops the optimistic bubble when the stream delivers the share item.
        let merged = TimelineMerge.merge([share().localItem()], [server])
        XCTAssertEqual(merged.items.map(\.id), ["tl_42"])
        XCTAssertEqual(merged.replaced, ["local-S1": "tl_42"])
    }

    func testFailureAndReply() {
        let failed = ShareReconcile.fail([share().localItem()], shareId: "S1", message: "Not sent")
        XCTAssertEqual(failed.first?.localStatus, .error)
        XCTAssertEqual(failed.first?.localError, "Not sent")
        let reply = TimelineItem(id: "tl_43", ts: Date(), channel: .ios, role: .lucina, text: "Bella", replyTo: "tl_42")
        XCTAssertTrue(ShareReconcile.replied([reply], userItemId: "tl_42", status: nil))
        XCTAssertTrue(ShareReconcile.replied([reply], userItemId: nil, status: ShareStatus(stage: "done", lucinaItemId: "tl_43")))
        XCTAssertFalse(ShareReconcile.replied([], userItemId: "tl_42", status: ShareStatus(stage: "reading")))
    }

    func testStatusAndAcceptedDecoding() throws {
        let status = try JSONDecoder().decode(ShareStatus.self, from: Data(#"""
        {"stage":"reading","attachments":[{"index":0,"kind":"pdf","mediaId":"m1","state":"read","summary":"Bolletta"},{"bad":true,"index":"x"}],"lucinaItemId":null}
        """#.utf8))
        XCTAssertEqual(status.stage, "reading")
        XCTAssertTrue(status.isWorking)
        XCTAssertEqual(status.attachments.first?.summary, "Bolletta")
        XCTAssertEqual(status.attachments.count, 2, "odd entries decode leniently")
        let accepted = try JSONDecoder().decode(ShareAccepted.self, from: Data(#"{"shareId":"S1","userItemId":42,"attachments":[]}"#.utf8))
        XCTAssertEqual(accepted.userItemId, "42")
    }

    func testRetryKeepsTheShareId() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        for file in share().files { try Data("x".utf8).write(to: directory.appendingPathComponent(file.fileName)) }
        let credentials = BridgeCredentials(token: "t", endpoints: ["https://example.com"], sources: ["share"])
        func build() throws -> ShareRecord {
            try ShareRequestBuilder.build(payload: share().payload(in: directory), intent: "ask", note: "Look", source: "ios-chat",
                                          credentials: credentials, directory: directory, shareId: share().shareId,
                                          meta: share().meta, origin: "chat")
        }
        let first = try build()
        let retried = try build()
        XCTAssertEqual(first.shareId, "S1")
        XCTAssertEqual(retried.shareId, first.shareId, "the server dedupes by share id")
        XCTAssertGreaterThanOrEqual(retried.createdAt, first.createdAt, "a retry is a fresh record, not an overdue one")
        // A retry uploads on its own background session; it still maps back to the share.
        XCTAssertEqual(ShareRequestBuilder.shareId(fromSession: ShareRequestBuilder.prefix + "S1"), "S1")
        XCTAssertEqual(ShareRequestBuilder.shareId(fromSession: ShareRequestBuilder.prefix + "S1~ab12cd34"), "S1")
        XCTAssertNil(ShareRequestBuilder.shareId(fromSession: "tech.getsecretary.other.S1"))
    }

    func testEntriesIndexFileParts() {
        let entries = BubbleEntry.entries([TimelineAttachment(kind: "photo"), TimelineAttachment(kind: "link", url: "https://a.b"),
                                           TimelineAttachment(kind: "pdf"), TimelineAttachment(kind: "location", lat: 1, lon: 2)])
        XCTAssertEqual(entries.map(\.fileIndex), [0, nil, 1, nil])
        XCTAssertTrue(entries[0].attachment.isImage)
        XCTAssertEqual(TimelineAttachment(kind: "location", url: "geo:55.5,12.25;u=10").coordinate?.lat, 55.5)
    }
}

final class VoiceCaptionTests: XCTestCase {
    private func voice(_ name: String) -> TimelineAttachment { TimelineAttachment(kind: "voice", name: name) }

    func testLegacyAutoCaptionIsHidden() {
        XCTAssertTrue(TimelineItem.hidesVoiceCaption("\u{1F4CE} voice-note.wav", attachments: [voice("voice-note.wav")]))
        XCTAssertTrue(TimelineItem.hidesVoiceCaption("  \u{1F4CE} note.m4a ", attachments: [voice("note.m4a")]))
        XCTAssertTrue(TimelineItem.hidesVoiceCaption("\u{1F4CE} a.wav, b.wav", attachments: [voice("a.wav"), voice("b.wav")]))
    }

    func testRealCaptionStays() {
        XCTAssertFalse(TimelineItem.hidesVoiceCaption("Listen to this", attachments: [voice("voice-note.wav")]))
        XCTAssertFalse(TimelineItem.hidesVoiceCaption("\u{1F4CE} voice-note.wav and more", attachments: [voice("voice-note.wav")]))
    }

    func testNonVoiceNamesStay() {
        let pdf = TimelineAttachment(kind: "pdf", name: "report.pdf")
        XCTAssertFalse(TimelineItem.hidesVoiceCaption("\u{1F4CE} report.pdf", attachments: [pdf]))
        XCTAssertFalse(TimelineItem.hidesVoiceCaption("\u{1F4CE} report.pdf, note.m4a", attachments: [pdf, voice("note.m4a")]))
    }
}
