import XCTest
import CoreGraphics
@testable import Secretary

final class VoiceWaveformTests: XCTestCase {
    func testDownsamplesToSixtyFourBucketsKeepingEachSlicesPeak() {
        // 128 levels: every odd one loud, so each bucket of two has a 1.0 in it.
        let levels = (0..<128).map { $0 % 2 == 1 ? Float(1) : Float(0.1) }
        let bytes = VoiceWaveform.bytes(levels)
        XCTAssertEqual(bytes.count, 64)
        XCTAssertTrue(bytes.allSatisfy { $0 == 255 })
    }

    func testUnevenSlicesCoverEveryLevel() {
        var levels = [Float](repeating: 0, count: 100)
        levels[99] = 0.5
        let bytes = VoiceWaveform.bytes(levels)
        XCTAssertEqual(bytes.count, 64)
        XCTAssertEqual(bytes.last, 128, "the last level lands in the last bucket (0.5 × 255 rounded)")
        XCTAssertEqual(bytes.dropLast().allSatisfy { $0 == 0 }, true)
    }

    func testShortNotesKeepOneBytePerLevelAndClamp() {
        XCTAssertEqual(VoiceWaveform.bytes([0, 0.5, 1, 2, -1, .nan]), [0, 128, 255, 255, 0, 0])
        XCTAssertEqual(VoiceWaveform.bytes([]), [])
        XCTAssertNil(VoiceWaveform.base64([]))
    }

    func testBase64RoundTrip() throws {
        let base64 = try XCTUnwrap(VoiceWaveform.base64([0, 1]))
        XCTAssertEqual(base64, "AP8=")
        XCTAssertEqual(VoiceWaveform.decode(base64), [0, 1])
        let long = try XCTUnwrap(VoiceWaveform.base64((0..<1000).map { Float($0) / 999 }))
        XCTAssertEqual(Data(base64Encoded: long)?.count, 64, "never more than 64 bytes")
        XCTAssertNil(VoiceWaveform.decode("not base64!"))
        XCTAssertNil(VoiceWaveform.decode(nil))
    }

    func testBarsStretchFewSamplesAcrossTheWidth() {
        XCTAssertEqual(VoiceWaveformBars.stretch([0.1, 0.9], to: 4), [0.1, 0.1, 0.9, 0.9])
        XCTAssertEqual(VoiceWaveformBars.stretch([0.1, 0.9, 0.5], to: 2), [0.1, 0.9, 0.5], "more samples than bars: resampled later")
    }

    func testFileNameAndPurge() throws {
        var components = DateComponents()
        (components.year, components.month, components.day, components.hour, components.minute, components.second) = (2026, 10, 26, 14, 30, 12)
        let date = try XCTUnwrap(Calendar.current.date(from: components))
        XCTAssertEqual(VoiceNoteFiles.fileName(for: date), "voice-20261026-143012.aac")

        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let old = directory.appendingPathComponent("old.aac")
        let fresh = directory.appendingPathComponent("fresh.aac")
        try Data("a".utf8).write(to: old)
        try Data("b".utf8).write(to: fresh)
        try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(-2 * 24 * 3600)], ofItemAtPath: old.path)
        VoiceNoteFiles.purge(in: directory)
        XCTAssertFalse(FileManager.default.fileExists(atPath: old.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: fresh.path))
    }
}

final class VoiceGestureTests: XCTestCase {
    func testThresholds() {
        XCTAssertEqual(VoiceGesture.outcome(.zero), .keep)
        XCTAssertEqual(VoiceGesture.outcome(CGSize(width: -99, height: 0)), .keep)
        XCTAssertEqual(VoiceGesture.outcome(CGSize(width: -100, height: 0)), .cancel)
        XCTAssertEqual(VoiceGesture.outcome(CGSize(width: 0, height: -79)), .keep)
        XCTAssertEqual(VoiceGesture.outcome(CGSize(width: 0, height: -80)), .lock)
        XCTAssertEqual(VoiceGesture.outcome(CGSize(width: 150, height: 120)), .keep, "right and down do nothing")
    }

    func testTheFurtherThresholdWins() {
        XCTAssertEqual(VoiceGesture.outcome(CGSize(width: -120, height: -82)), .cancel, "1.2 of cancel vs 1.03 of lock")
        XCTAssertEqual(VoiceGesture.outcome(CGSize(width: -101, height: -120)), .lock, "1.01 of cancel vs 1.5 of lock")
    }

    func testRelease() {
        XCTAssertEqual(VoiceGesture.release(CGSize(width: -30, height: -20)), .send)
        XCTAssertEqual(VoiceGesture.release(CGSize(width: -130, height: 0)), .cancel)
        XCTAssertEqual(VoiceGesture.release(CGSize(width: 0, height: -90)), .lock)
    }

    func testCancelHintFadesAsTheFingerSlides() {
        XCTAssertEqual(VoiceGesture.cancelHintOpacity(.zero), 1)
        XCTAssertEqual(VoiceGesture.cancelHintOpacity(CGSize(width: -50, height: 0)), 0.5, accuracy: 0.0001)
        XCTAssertEqual(VoiceGesture.cancelHintOpacity(CGSize(width: -200, height: 0)), 0)
        XCTAssertEqual(VoiceGesture.cancelHintOpacity(CGSize(width: 40, height: 0)), 1)
        XCTAssertEqual(VoiceGesture.lockProgress(CGSize(width: 0, height: -40)), 0.5, accuracy: 0.0001)
    }

    func testTap() {
        XCTAssertTrue(VoiceGesture.isTap(CGSize(width: 5, height: -6)))
        XCTAssertFalse(VoiceGesture.isTap(CGSize(width: 0, height: -20)))
    }
}

final class VoiceNoteMetaTests: XCTestCase {
    func testVoiceMetaJSON() throws {
        let meta = ShareMeta(files: [ShareMeta.File(index: 0, transcript: "ciao", durationMs: 4200, voice: true, waveform: "AP8=")])
        XCTAssertEqual(try meta.json(), #"{"files":[{"durationMs":4200,"index":0,"transcript":"ciao","voice":true,"waveform":"AP8="}]}"#)
        let whisper = ShareMeta(files: [ShareMeta.File(index: 0, durationMs: 900, voice: true, waveform: "AP8=")])
        XCTAssertEqual(try whisper.json(), #"{"files":[{"durationMs":900,"index":0,"voice":true,"waveform":"AP8="}]}"#,
                       "no transcript: the server runs Whisper")
    }

    func testPendingVoiceAttachmentMeta() {
        let url = URL(fileURLWithPath: "/tmp/voice-20261026-143012.aac")
        let note = PendingAttachment(url: url, name: "voice-20261026-143012.aac", mime: "audio/aac", kind: .audio, size: 10,
                                     transcript: "compra il latte", durationMs: 2500, voice: true, waveform: "AP8=")
        let file = note.metaFile(index: 0)
        XCTAssertEqual(file.voice, true)
        XCTAssertEqual(file.waveform, "AP8=")
        XCTAssertEqual(file.durationMs, 2500)
        XCTAssertEqual(file.transcript, "compra il latte")
        let plain = PendingAttachment(url: url, name: "song.aac", mime: "audio/aac", waveform: "AP8=")
        XCTAssertNil(plain.metaFile(index: 0).voice)
        XCTAssertNil(plain.metaFile(index: 0).waveform, "only voice notes carry a waveform")
    }

    func testOutgoingVoiceShare() throws {
        let share = OutgoingShare(shareId: "V1", caption: "",
                                  files: [OutgoingShare.File(fileName: "0-voice.aac", name: "voice-20261026-143012.aac", mime: "audio/aac",
                                                             kind: "audio", size: 99, durationMs: 4200, transcript: "ciao",
                                                             voice: true, waveform: "AP8=")],
                                  createdAt: Date(timeIntervalSince1970: 100))
        XCTAssertEqual(try share.meta.json(), #"{"files":[{"durationMs":4200,"index":0,"transcript":"ciao","voice":true,"waveform":"AP8="}]}"#)
        let bubble = share.localItem(ts: Date(timeIntervalSince1970: 100))
        XCTAssertEqual(bubble.attachments.map(\.kind), ["voice"])
        XCTAssertEqual(bubble.attachments[0].waveform, "AP8=")
        XCTAssertEqual(bubble.attachments[0].transcript, "ciao")
        XCTAssertTrue(bubble.attachments[0].isVoice)
        XCTAssertTrue(bubble.attachments[0].isFilePart)
        // Records written before voice notes existed still load.
        let old = #"{"fileName":"0-a.m4a","name":"a.m4a","mime":"audio/mp4","kind":"audio","size":3}"#
        let decoded = try JSONDecoder().decode(OutgoingShare.File.self, from: Data(old.utf8))
        XCTAssertFalse(decoded.isVoice)
        // And a voice record round-trips.
        let again = try JSONDecoder().decode(OutgoingShare.self, from: JSONEncoder().encode(share))
        XCTAssertEqual(again, share)
    }
}

final class VoiceBubbleDecodingTests: XCTestCase {
    private func decode(_ json: String) throws -> TimelineItem {
        try JSONDecoder().decode(TimelineItem.self, from: Data(json.utf8))
    }

    func testVoiceAttachment() throws {
        let item = try decode(#"""
        {"id":"tl_1","ts":1761480000000,"channel":"ios","role":"user","text":"",
         "attachments":[{"kind":"voice","mime":"audio/aac","mediaId":"m1","durationMs":4200,"waveform":"AP8=","transcript":"ciao Lucina"}],
         "meta":{"kind":"share","shareId":"S1"}}
        """#)
        let attachment = try XCTUnwrap(item.attachments.first)
        XCTAssertTrue(attachment.isVoice)
        XCTAssertTrue(attachment.isAudio)
        XCTAssertEqual(attachment.waveform, "AP8=")
        XCTAssertEqual(attachment.transcript, "ciao Lucina")
        XCTAssertEqual(VoiceWaveform.decode(attachment.waveform), [0, 1])
        XCTAssertEqual(BubbleEntry.entries(item.attachments).first?.fileIndex, 0)
    }

    func testLenientVoiceFields() throws {
        let item = try decode(#"""
        {"id":"tl_2","ts":1761480000000,"channel":"telegram","role":"user","text":"",
         "attachments":[{"kind":"VOICE","waveform":5,"transcript":["x"]}]}
        """#)
        let attachment = try XCTUnwrap(item.attachments.first)
        XCTAssertTrue(attachment.isVoice)
        XCTAssertNil(attachment.waveform)
        XCTAssertNil(attachment.transcript)
    }

    func testReplyMeta() throws {
        let reply = try decode(#"""
        {"id":"tl_3","ts":1761480001000,"channel":"ios","role":"lucina","text":"Va bene, domani alle 8.","replyTo":"tl_1",
         "meta":{"transcript":"ricordami di comprare il latte","voiceReply":true}}
        """#)
        XCTAssertEqual(reply.meta?.transcript, "ricordami di comprare il latte")
        XCTAssertEqual(reply.meta?.voiceReply, true)
        let odd = try decode(#"{"id":"tl_4","ts":1,"role":"lucina","text":"x","meta":{"voiceReply":"yes","transcript":7}}"#)
        XCTAssertNil(odd.meta?.voiceReply)
        XCTAssertNil(odd.meta?.transcript)
        // The cache keeps them.
        let cached = try JSONDecoder().decode(TimelineItem.self, from: JSONEncoder().encode(reply))
        XCTAssertEqual(cached.meta?.voiceReply, true)
        XCTAssertEqual(cached.meta?.transcript, "ricordami di comprare il latte")
    }

    func testStatusAttachmentTranscript() throws {
        let status = try JSONDecoder().decode(ShareStatus.self, from: Data(#"""
        {"stage":"thinking","attachments":[{"index":0,"kind":"voice","state":"read","transcript":"compra il latte"}]}
        """#.utf8))
        XCTAssertEqual(status.attachments.first?.transcript, "compra il latte")
    }
}

@MainActor
final class VoiceReplyTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_761_480_100)

    private func note(_ id: String = "tl_1") -> TimelineItem {
        TimelineItem(id: id, ts: now.addingTimeInterval(-10), channel: .ios, role: .user, text: "",
                     attachments: [TimelineAttachment(kind: "voice", mime: "audio/aac", durationMs: 3000)])
    }

    private func reply(_ id: String = "tl_2", to: String = "tl_1", voiceReply: Bool? = true, age: TimeInterval = 5) -> TimelineItem {
        var meta = TimelineMeta()
        meta.voiceReply = voiceReply
        return TimelineItem(id: id, ts: now.addingTimeInterval(-age), channel: .ios, role: .lucina, text: "**Fatto**, a domani.",
                            replyTo: to, meta: meta)
    }

    private func autoplay(_ item: TimelineItem, items: [TimelineItem]? = nil, enabled: Bool = true, foreground: Bool = true,
                          played: Set<String> = []) -> Bool {
        LucinaVoiceReplies.shouldAutoplay(item, in: items ?? [note(), item], enabled: enabled, foreground: foreground, played: played, now: now)
    }

    func testPlaysAFreshReplyToAVoiceNote() {
        XCTAssertTrue(autoplay(reply()))
        XCTAssertEqual(LucinaVoiceReplies.speech(for: reply()), "Fatto, a domani.", "no markdown in her voice")
    }

    func testNeverTwiceAndOnlyWhenWanted() {
        XCTAssertFalse(autoplay(reply(), played: ["tl_2"]))
        XCTAssertFalse(autoplay(reply(), enabled: false))
        XCTAssertFalse(autoplay(reply(), foreground: false))
        XCTAssertFalse(autoplay(reply(voiceReply: nil)))
        XCTAssertFalse(autoplay(reply(voiceReply: false)))
        XCTAssertFalse(autoplay(reply(age: 600)), "an old reply loaded later stays quiet")
    }

    func testOnlyRepliesToVoiceNotes() {
        let text = TimelineItem(id: "tl_1", ts: now, channel: .ios, role: .user, text: "ciao")
        XCTAssertFalse(autoplay(reply(), items: [text, reply()]))
        XCTAssertFalse(autoplay(reply(to: "missing")))
    }

    func testDefaultsToOn() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "voice-reply-tests-\(UUID().uuidString)"))
        XCTAssertTrue(LucinaVoiceReplies.isEnabled(defaults))
        defaults.set(false, forKey: LucinaVoiceReplies.settingKey)
        XCTAssertFalse(LucinaVoiceReplies.isEnabled(defaults))
    }
}
