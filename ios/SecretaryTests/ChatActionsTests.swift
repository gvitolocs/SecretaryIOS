import XCTest
import CoreGraphics
import UniformTypeIdentifiers
@testable import Secretary

final class BubbleSwipeTests: XCTestCase {
    private func run(_ translations: [CGSize]) -> BubbleSwipe.Track {
        translations.reduce(BubbleSwipe.Track.undecided) { BubbleSwipe.track($0, translation: $1) }
    }

    func testStaysUndecidedUnderTheActivationDistance() {
        XCTAssertEqual(run([CGSize(width: -8, height: 3), CGSize(width: -11, height: 0)]), .undecided)
        XCTAssertEqual(BubbleSwipe.action(.undecided, canReply: true, canReadAloud: true), .none)
    }

    func testMostlySidewaysLocksHorizontal() {
        XCTAssertEqual(run([CGSize(width: -14, height: 5)]), .horizontal(-14))
        // Once horizontal, later vertical movement doesn't change the axis.
        XCTAssertEqual(run([CGSize(width: -14, height: 5), CGSize(width: -40, height: 60)]), .horizontal(-40))
    }

    func testMostlyVerticalStaysWithTheScrollView() {
        let track = run([CGSize(width: 6, height: 20), CGSize(width: -90, height: 20)])
        XCTAssertEqual(track, .vertical)
        XCTAssertEqual(BubbleSwipe.action(track, canReply: true, canReadAloud: true), .none)
        XCTAssertEqual(BubbleSwipe.offset(track, canReply: true, canReadAloud: true), 0)
        // A diagonal tie is not horizontal.
        XCTAssertEqual(run([CGSize(width: 20, height: 20)]), .vertical)
    }

    func testLeftPastTheThresholdReplies() {
        XCTAssertEqual(BubbleSwipe.action(.horizontal(-59), canReply: true, canReadAloud: true), .none)
        XCTAssertEqual(BubbleSwipe.action(.horizontal(-60), canReply: true, canReadAloud: true), .reply)
        XCTAssertEqual(BubbleSwipe.action(.horizontal(-200), canReply: false, canReadAloud: true), .none)
    }

    func testRightPastTheThresholdReadsAloud() {
        XCTAssertEqual(BubbleSwipe.action(.horizontal(61), canReply: true, canReadAloud: true), .readAloud)
        XCTAssertEqual(BubbleSwipe.action(.horizontal(61), canReply: true, canReadAloud: false), .none)
    }

    func testOffsetFollowsTheFingerUpToSeventyPoints() {
        XCTAssertEqual(BubbleSwipe.offset(.horizontal(-30), canReply: true, canReadAloud: true), -30)
        XCTAssertEqual(BubbleSwipe.offset(.horizontal(-300), canReply: true, canReadAloud: true), -70)
        XCTAssertEqual(BubbleSwipe.offset(.horizontal(300), canReply: true, canReadAloud: true), 70)
        XCTAssertEqual(BubbleSwipe.offset(.horizontal(-30), canReply: false, canReadAloud: true), 0, "a direction that does nothing stays put")
        XCTAssertEqual(BubbleSwipe.progress(-30), 0.5)
        XCTAssertEqual(BubbleSwipe.progress(70), 1)
    }
}

final class ChatQuoteTests: XCTestCase {
    private func item(_ role: TimelineItem.Role = .lucina, text: String = "", attachments: [TimelineAttachment] = [],
                      id: String = "tl_1") -> TimelineItem {
        TimelineItem(id: id, ts: Date(timeIntervalSince1970: 1_000), channel: .ios, role: role, text: text, attachments: attachments)
    }

    func testSenders() {
        XCTAssertEqual(ChatQuote.sender(item(.lucina)), "Lucina")
        XCTAssertEqual(ChatQuote.sender(item(.user)), "You")
        var mcp = item(.user, text: "hi")
        mcp.meta = TimelineMeta()
        mcp.meta?.via = "mcp"
        mcp.meta?.caller = "claude-code"
        XCTAssertEqual(ChatQuote.sender(mcp), "Claude Code")
    }

    func testTextIsOneLineWithoutMarkdown() {
        XCTAssertEqual(ChatQuote.summary(item(text: "**Ciao**\n\nsecond   line")), "Ciao second line")
    }

    func testPhotosVoiceNotesAndFiles() {
        let photo = TimelineAttachment(kind: "image", name: "IMG_1.jpg")
        XCTAssertEqual(ChatQuote.summary(item(.user, attachments: [photo])), "📷 Photo")
        XCTAssertEqual(ChatQuote.summary(item(.user, attachments: [photo, photo, photo])), "📷 3 photos")
        XCTAssertEqual(ChatQuote.summary(item(.user, text: "Look", attachments: [photo])), "📷 Look")
        let voice = TimelineAttachment(kind: "voice", name: "voice.aac", durationMs: 7_400)
        XCTAssertEqual(ChatQuote.summary(item(.user, attachments: [voice])), "🎤 Voice note 0:07")
        let pdf = TimelineAttachment(kind: "pdf", name: "Fattura.pdf")
        XCTAssertEqual(ChatQuote.summary(item(.user, attachments: [pdf])), "📄 Fattura.pdf")
        XCTAssertEqual(ChatQuote.summary(item(.user)), "Message")
    }

    func testRichRepliesQuoteTheirBubbles() throws {
        let rich = try XCTUnwrap(RichMessage.parse(.object(["format": .string("v1"), "bubbles": .array([.string("Fatto **subito**")])])))
        var reply = item(text: "fallback")
        reply.rich = rich
        XCTAssertEqual(ChatQuote.summary(reply), "Fatto subito")
    }
}

final class ChatPlainTextTests: XCTestCase {
    func testStripsBlockAndInlineMarkdownKeepingLinks() {
        let markdown = "# Title\n> quoted\n- one\n- [x] done\n- [ ] todo\n---\n**bold** and [site](https://example.com)\n```\ncode\n```"
        XCTAssertEqual(ChatPlainText.from(markdown),
                       "Title\nquoted\n• one\n☑ done\n☐ todo\n\nbold and site (https://example.com)\ncode")
    }

    func testSpeechReadsWordsAndTranscripts() {
        let photo = TimelineItem(id: "a", ts: Date(), channel: .ios, role: .user, text: "",
                                 attachments: [TimelineAttachment(kind: "image")])
        XCTAssertNil(ChatSpeech.text(for: photo, transcript: nil), "a photo without a caption has nothing to read")
        let captioned = TimelineItem(id: "b", ts: Date(), channel: .ios, role: .user, text: "Il *mio* gatto",
                                     attachments: [TimelineAttachment(kind: "image")])
        XCTAssertEqual(ChatSpeech.text(for: captioned, transcript: nil), "Il mio gatto")
        let voice = TimelineItem(id: "c", ts: Date(), channel: .ios, role: .user, text: "",
                                 attachments: [TimelineAttachment(kind: "voice")])
        XCTAssertEqual(ChatSpeech.text(for: voice, transcript: "compra il latte"), "compra il latte")
        let long = TimelineItem(id: "d", ts: Date(), channel: .ios, role: .lucina, text: String(repeating: "a ", count: 1000))
        XCTAssertEqual(ChatSpeech.text(for: long, transcript: nil)?.count, 1200, "the TTS limit")
    }
}

final class ChatShareBuilderTests: XCTestCase {
    private func item(_ text: String = "", _ attachments: [TimelineAttachment] = [], role: TimelineItem.Role = .user,
                      id: String = "tl_1", ts: TimeInterval = 0) -> TimelineItem {
        var item = TimelineItem(id: id, ts: Date(timeIntervalSince1970: 1_761_472_920 + ts), channel: .ios, role: role, text: text,
                                attachments: attachments)
        item.meta = TimelineMeta(kind: "share")
        item.meta?.shareId = "S1"
        return item
    }

    private func kinds(_ parts: [ChatSharePart]) -> [String] {
        parts.map { part in
            switch part {
            case .text: return "text"
            case .link: return "link"
            case let .media(media): return media.kind.rawValue
            }
        }
    }

    func testPictureAlone() {
        let parts = ChatShareBuilder.parts(for: item("", [TimelineAttachment(kind: "image", mediaId: "m1")]), transcript: nil)
        XCTAssertEqual(kinds(parts), ["image"])
        guard case let .media(media) = parts[0] else { return XCTFail("not media") }
        XCTAssertEqual(media.attachment.mediaId, "m1")
        XCTAssertEqual(media.fileIndex, 0)
        XCTAssertEqual(media.shareId, "S1")
    }

    func testPictureWithCaptionGivesBoth() {
        let parts = ChatShareBuilder.parts(for: item("**Guarda**", [TimelineAttachment(kind: "photo")]), transcript: nil)
        XCTAssertEqual(kinds(parts), ["image", "text"])
        XCTAssertEqual(parts.last, .text("Guarda"))
    }

    func testTextIsPlain() {
        XCTAssertEqual(ChatShareBuilder.parts(for: item("- uno\n- due", role: .lucina), transcript: nil), [.text("• uno\n• due")])
    }

    func testVoiceNoteGivesAudioAndTranscript() {
        let parts = ChatShareBuilder.parts(for: item("", [TimelineAttachment(kind: "voice", mediaId: "v1")]), transcript: "ciao Lucina")
        XCTAssertEqual(kinds(parts), ["voice", "text"])
        XCTAssertEqual(parts.last, .text("ciao Lucina"))
    }

    func testFilesLinksAndLocations() {
        let attachments = [
            TimelineAttachment(kind: "pdf", name: "a.pdf", mediaId: "f1"),
            TimelineAttachment(kind: "link", url: "https://example.com/x"),
            TimelineAttachment(kind: "link", url: "javascript:alert(1)"),
            TimelineAttachment(kind: "location", name: "Casa", lat: 55.6, lon: 12.5),
        ]
        let parts = ChatShareBuilder.parts(for: item("", attachments), transcript: nil)
        XCTAssertEqual(kinds(parts), ["file", "text", "link", "link"])
        XCTAssertEqual(parts[1], .text("📍 Casa"))
        XCTAssertEqual(parts[2], .link(URL(string: "https://example.com/x")!))
        guard case let .link(maps) = parts[3] else { return XCTFail("no maps link") }
        XCTAssertEqual(maps.host, "maps.apple.com")
    }

    func testCardsGiveSummaryAndLinks() throws {
        let json: JSONValue = .object([
            "format": .string("v1"),
            "bubbles": .array([.string("Ecco le offerte")]),
            "blocks": .array([
                .object(["type": .string("card"), "title": .string("Latte"), "subtitle": .string("Netto"), "body": .string("**9,95** kr")]),
                .object(["type": .string("link_preview"), "url": .string("https://netto.dk/latte"), "title": .string("Netto")]),
            ]),
        ])
        var reply = item(role: .lucina)
        reply.rich = try XCTUnwrap(RichMessage.parse(json))
        let parts = ChatShareBuilder.parts(for: reply, transcript: nil)
        XCTAssertEqual(parts, [.text("Ecco le offerte\n\nLatte\nNetto\n9,95 kr\n\nNetto"), .link(URL(string: "https://netto.dk/latte")!)])
    }

    func testConversationTranscript() {
        let utc = TimeZone(identifier: "UTC")!
        let items = [
            item("Ciao", id: "1"),
            item("Ciao Giuseppe!", role: .lucina, id: "2", ts: 30),
            item("", [TimelineAttachment(kind: "voice", durationMs: 4_000), TimelineAttachment(kind: "image", name: "a.jpg", mediaId: "m")], id: "3", ts: 60),
        ]
        let text = ChatShareBuilder.transcript(items, userName: "Giuseppe", timeZone: utc) { $0.id == "3" ? "compra il latte" : nil }
        XCTAssertEqual(text, "Giuseppe — 10:02\nCiao\n\nLucina — 10:02\nCiao Giuseppe!\n\nGiuseppe — 10:03\n🎤 compra il latte\n📷 a.jpg")
        let parts = ChatShareBuilder.conversationParts(items, userName: "Giuseppe", timeZone: utc) { _ in nil }
        XCTAssertEqual(kinds(parts), ["text", "voice", "image"], "one transcript, then the files")
    }

    func testTranscriptAcrossDaysCarriesTheDate() {
        let utc = TimeZone(identifier: "UTC")!
        let text = ChatShareBuilder.transcript([item("a", id: "1"), item("b", role: .lucina, id: "2", ts: 86_400)], userName: "Me",
                                               timeZone: utc) { _ in nil }
        XCTAssertEqual(text, "Me — 2025-10-26 10:02\na\n\nLucina — 2025-10-27 10:02\nb")
    }

    func testOwnerNameFromTheDeviceName() {
        XCTAssertEqual(ChatShareBuilder.ownerName(deviceName: "Giuseppe's iPhone"), "Giuseppe")
        XCTAssertEqual(ChatShareBuilder.ownerName(deviceName: "Giuseppe’s iPad"), "Giuseppe")
        XCTAssertEqual(ChatShareBuilder.ownerName(deviceName: "iPhone di Giuseppe"), "Giuseppe")
        XCTAssertEqual(ChatShareBuilder.ownerName(deviceName: "Giuseppes iPhone"), "Giuseppe")
        XCTAssertEqual(ChatShareBuilder.ownerName(deviceName: "iPhone"), "Me")
        XCTAssertEqual(ChatShareBuilder.ownerName(deviceName: nil), "Me")
    }

    func testFileNamesGetAnExtension() {
        XCTAssertEqual(ChatShareNaming.fileName(for: TimelineAttachment(kind: "image", name: "IMG_1.HEIC")), "IMG_1.HEIC")
        XCTAssertEqual(ChatShareNaming.fileName(for: TimelineAttachment(kind: "image", mime: "image/png")), "photo.png")
        XCTAssertEqual(ChatShareNaming.fileName(for: TimelineAttachment(kind: "voice")), "voice-note.aac")
        XCTAssertEqual(ChatShareNaming.fileName(for: TimelineAttachment(kind: "pdf", name: "a/b:c", mime: "application/pdf")), "a_b_c.pdf")
        XCTAssertEqual(ChatShareNaming.type(of: URL(fileURLWithPath: "/x/voice.aac")), UTType(filenameExtension: "aac"))
    }

    func testMenuActionsPerMessage() {
        let local = TimelineItem(id: TimelineItem.localPrefix + "x", ts: Date(), channel: .ios, role: .user, text: "hi")
        let localActions = MessageAction.available(for: local, speech: "hi")
        XCTAssertFalse(localActions.contains(.reply), "an optimistic bubble can't be replied to yet")
        XCTAssertFalse(localActions.contains(.saveToLibrary))
        let photo = item("", [TimelineAttachment(kind: "image", mediaId: "m")])
        XCTAssertEqual(MessageAction.available(for: photo, speech: nil), [.reply, .share, .saveToPhotos, .saveToLibrary, .saveAsNote, .select, .hide])
        XCTAssertEqual(MessageAction.saveAsNote.intent, "note")
        XCTAssertEqual(MessageAction.saveToLibrary.intent, "library")
    }
}

final class TTSCacheTests: XCTestCase {
    private var directory: URL!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent("tts-tests-" + UUID().uuidString, isDirectory: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: directory)
    }

    func testFileNames() {
        XCTAssertEqual(TTSCache.fileName(for: "tl_01JABC"), "tl_01JABC.mp3")
        let unsafe = TTSCache.fileName(for: "../etc/passwd")
        XCTAssertFalse(unsafe.contains("/"))
        XCTAssertFalse(unsafe.hasPrefix("."))
        XCTAssertTrue(unsafe.hasSuffix(".mp3"))
        XCTAssertNotEqual(TTSCache.fileName(for: "a/b"), TTSCache.fileName(for: "a_b"), "cleaned ids never share a file")
        XCTAssertEqual(TTSCache.fileName(for: "a/b"), TTSCache.fileName(for: "a/b"), "stable")
        XCTAssertLessThan(TTSCache.fileName(for: String(repeating: "x", count: 500)).count, 130)
    }

    func testReadWriteAndLeastRecentlyUsedPruning() throws {
        let cache = TTSCache(directory: directory, limit: 250)
        let t0 = Date(timeIntervalSince1970: 1_000)
        try cache.write(Data(count: 100), for: "a", now: t0)
        try cache.write(Data(count: 100), for: "b", now: t0.addingTimeInterval(1))
        // Playing "a" again makes "b" the least recently used.
        XCTAssertEqual(cache.read("a", now: t0.addingTimeInterval(2))?.count, 100)
        try cache.write(Data(count: 100), for: "c", now: t0.addingTimeInterval(3))
        XCTAssertNotNil(cache.read("a"))
        XCTAssertNil(cache.read("b"), "the oldest goes first")
        XCTAssertNotNil(cache.read("c"))
        cache.remove("a")
        XCTAssertNil(cache.read("a"))
    }

    func testTheNewestFileIsKeptEvenWhenLargerThanTheLimit() throws {
        let cache = TTSCache(directory: directory, limit: 50)
        try cache.write(Data(count: 10), for: "old")
        try cache.write(Data(count: 80), for: "new")
        XCTAssertNotNil(cache.read("new"))
        XCTAssertNil(cache.read("old"))
    }
}

@MainActor
final class ChatReplyStateTests: XCTestCase {
    private func store() -> ChatStore {
        let defaults = UserDefaults(suiteName: "chat-actions-" + UUID().uuidString)!
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".json")
        return ChatStore(api: { SecretaryAPI(client: APIClient(baseURL: URL(string: "https://example.invalid")!, accessKey: nil)) },
                         fileURL: url, defaults: defaults)
    }

    func testReplyNeedsAServerIdAndIsTakenOnce() {
        let chat = store()
        let local = TimelineItem(id: TimelineItem.localPrefix + "1", ts: Date(), channel: .ios, role: .lucina, text: "x")
        XCTAssertFalse(chat.beginReply(local))
        XCTAssertNil(chat.replyingTo)
        let server = TimelineItem(id: "tl_9", ts: Date(), channel: .ios, role: .lucina, text: "x")
        XCTAssertTrue(chat.beginReply(server))
        XCTAssertEqual(chat.takeReply()?.id, "tl_9")
        XCTAssertNil(chat.replyingTo, "the bar closes when a send takes it")
    }

    func testHideAndUnhideAll() {
        let chat = store()
        chat.hide("tl_1")
        XCTAssertTrue(chat.isHidden("tl_1"))
        chat.unhideAll()
        XCTAssertFalse(chat.isHidden("tl_1"))
    }

    func testRequestsCarryReplyTo() throws {
        let request = ChatRequest(message: "m", sessionId: "s", clientMessageId: "c", replyTo: "tl_7")
        let json = try XCTUnwrap(String(data: APIClient.encoder.encode(request), encoding: .utf8))
        XCTAssertTrue(json.contains("\"replyTo\":\"tl_7\""))
        let plain = try XCTUnwrap(String(data: APIClient.encoder.encode(ChatRequest(message: "m", sessionId: "s", clientMessageId: "c")),
                                         encoding: .utf8))
        XCTAssertFalse(plain.contains("replyTo"))
    }

    func testOutgoingSharesKeepReplyToThroughTheServerCopy() {
        let share = OutgoingShare(shareId: "S2", caption: "x", files: [], urls: ["https://a.b"], createdAt: Date(), replyTo: "tl_3")
        XCTAssertEqual(share.localItem().replyTo, "tl_3")
        let accepted = ShareReconcile.accept([share.localItem()], shareId: "S2", accepted: ShareAccepted(shareId: "S2", userItemId: "tl_4"))
        XCTAssertEqual(accepted.first?.id, "tl_4")
        XCTAssertEqual(accepted.first?.replyTo, "tl_3")
    }
}
