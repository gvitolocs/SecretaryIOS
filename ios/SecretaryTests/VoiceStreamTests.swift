import AVFoundation
import XCTest
@testable import Secretary

final class VoiceStartTests: XCTestCase {
    private func object(_ json: String) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    }

    func testNoteStart() {
        let start = VoiceStart.note(sessionId: "sess_1", replyTo: "itm-9", lang: "it")
        XCTAssertEqual(start.json(), #"{"codec":"pcm16","lang":"it","mode":"ptt","purpose":"note","rate":16000,"replyTo":"itm-9","sessionId":"sess_1","source":"ios-chat","thread":"main","type":"start"}"#)
    }

    func testNoteStartWithoutOptionalKeys() {
        let start = VoiceStart.note(sessionId: nil, replyTo: nil)
        XCTAssertEqual(start.json(), #"{"codec":"pcm16","mode":"ptt","purpose":"note","rate":16000,"source":"ios-chat","thread":"main","type":"start"}"#)
    }

    func testCommandStart() throws {
        let json = try object(VoiceStart.command(sessionId: "abc", speak: false).json())
        XCTAssertEqual(json["mode"] as? String, "vad")
        XCTAssertEqual(json["purpose"] as? String, "command")
        XCTAssertEqual(json["thread"] as? String, "main")
        XCTAssertEqual(json["text"] as? Bool, true)
        XCTAssertEqual(json["speak"] as? Bool, false)
        XCTAssertEqual(json["lang"] as? String, "it")
        XCTAssertNil(json["replyTo"], "note-only keys stay out of a command")
        XCTAssertNil(json["source"])
    }

    func testUnsafeIdsAreLeftOutBecauseTheServerRefusesThem() throws {
        let json = try object(VoiceStart.note(sessionId: "has space", replyTo: String(repeating: "a", count: 65)).json())
        XCTAssertNil(json["sessionId"])
        XCTAssertNil(json["replyTo"])
        XCTAssertTrue(VoiceStart.isSafeId("itm_Ab-9"))
        XCTAssertFalse(VoiceStart.isSafeId(""))
        XCTAssertFalse(VoiceStart.isSafeId(nil))
        XCTAssertFalse(VoiceStart.isSafeId("a/b"))
    }

    func testEndAndCancelFrames() {
        XCTAssertEqual(VoiceStart.end, #"{"type":"end"}"#)
        XCTAssertEqual(VoiceStart.cancel, #"{"type":"cancel"}"#)
    }
}

final class VoiceEventParserTests: XCTestCase {
    func testBareListeningIsAnOldServer() {
        guard case let .listening(listening)? = VoiceEvent.parse(text: #"{"type":"listening"}"#) else { return XCTFail() }
        XCTAssertFalse(listening.understandsStreaming)
        XCTAssertNil(listening.turn)
    }

    func testListeningEchoesWhatTheServerUnderstood() {
        let text = #"{"type":"listening","purpose":"note","codec":"pcm16","rate":16000,"turn":"t-1"}"#
        guard case let .listening(listening)? = VoiceEvent.parse(text: text) else { return XCTFail() }
        XCTAssertTrue(listening.understandsStreaming)
        XCTAssertEqual(listening.turn, "t-1")
        XCTAssertEqual(listening.rate, 16000)
        XCTAssertEqual(listening.purpose, "note")
    }

    func testSection2Messages() {
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"endpoint"}"#), .endpoint)
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"transcript","text":"ciao"}"#), .transcript("ciao"))
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"audio","rate":24000}"#), .audioFormat(rate: 24000))
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"reply","text":"ok"}"#), .reply(text: "ok", shareId: nil, itemId: nil))
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"error","code":"no_speech","message":"m"}"#),
                       .error(code: "no_speech", message: "m", shareId: nil))
    }

    func testDoneKeepsTheTimings() {
        let text = #"{"type":"done","ms":{"stt":80,"lucina_first":400,"route":"jev","ack":true,"total":1500}}"#
        XCTAssertEqual(VoiceEvent.parse(text: text), .done(ms: ["stt": 80, "lucina_first": 400, "total": 1500], shareId: nil),
                       "non-numeric entries are dropped")
    }

    func testSection10Messages() {
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"delta","text":"Cia"}"#), .delta("Cia"))
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"answer","text":"Ciao!","replaces":true}"#), .answer("Ciao!", replaces: true))
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"answer","text":"Ciao!"}"#), .answer("Ciao!", replaces: false))
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"note","shareId":"s1","userItemId":"u1"}"#), .note(shareId: "s1", userItemId: "u1"))
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"reply","shareId":"s1","text":"fatto","itemId":"l1"}"#),
                       .reply(text: "fatto", shareId: "s1", itemId: "l1"))
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"error","code":"lucina_unavailable","shareId":"s1"}"#),
                       .error(code: "lucina_unavailable", message: "", shareId: "s1"))
    }

    func testNoteMessagesCarryTheirShareId() {
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"done","shareId":"s1","ms":{"total":9}}"#)?.shareId, "s1")
        XCTAssertNil(VoiceEvent.parse(text: #"{"type":"done","ms":{}}"#)?.shareId)
        XCTAssertNil(VoiceEvent.transcript("x").shareId)
    }

    func testUnknownAndMalformedMessages() {
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"telemetry_ack"}"#), .unknown("telemetry_ack"))
        XCTAssertEqual(VoiceEvent.parse(text: #"{"type":"note"}"#), .unknown("note"), "a note without its share id is ignored")
        XCTAssertNil(VoiceEvent.parse(text: "not json"))
        XCTAssertNil(VoiceEvent.parse(text: #"{"text":"no type"}"#))
        XCTAssertNil(VoiceEvent.parse(text: "[1,2]"))
    }

    func testBinaryFramesAreAudio() {
        XCTAssertEqual(VoiceEvent.parse(data: Data([1, 2])), .audio(Data([1, 2])))
    }
}

final class VoiceBackoffTests: XCTestCase {
    func testDelaysDoubleUpToThirtySeconds() {
        XCTAssertEqual((0..<9).map { VoiceBackoff.delay(attempt: $0) }, [1, 2, 4, 8, 16, 30, 30, 30, 30])
        XCTAssertEqual(VoiceBackoff.delay(attempt: -3), 1)
        XCTAssertEqual(VoiceBackoff.delay(attempt: 10_000), 30)
    }
}

final class VoiceSocketURLTests: XCTestCase {
    func testSchemeAndPath() {
        XCTAssertEqual(VoiceSocket.url(base: URL(string: "https://api.example.com")!)?.absoluteString, "wss://api.example.com/v1/voice/ws")
        XCTAssertEqual(VoiceSocket.url(base: URL(string: "http://192.168.1.5:8800/")!)?.absoluteString, "ws://192.168.1.5:8800/v1/voice/ws")
        XCTAssertEqual(VoiceSocket.url(base: URL(string: "https://example.com/secretary")!)?.absoluteString, "wss://example.com/secretary/v1/voice/ws")
        XCTAssertNil(VoiceSocket.url(base: URL(string: "ftp://example.com")!))
    }

    func testUpgradeHeaders() throws {
        let request = try XCTUnwrap(VoiceSocket.request(base: URL(string: "https://api.example.com")!, accessKey: "k"))
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer k")
        XCTAssertEqual(request.value(forHTTPHeaderField: "X-Secretary-Client"), "ios")
    }
}

final class VoiceTimingsTests: XCTestCase {
    func testMarksAreRelativeToEndAndCountOnce() {
        var timings = VoiceTimings()
        let start = ContinuousClock.now
        XCTAssertNil(timings.mark(.transcript, at: start), "nothing before end")
        timings.end(at: start)
        XCTAssertEqual(timings.mark(.transcript, at: start + .milliseconds(85)), 85)
        XCTAssertNil(timings.mark(.transcript, at: start + .milliseconds(90)), "the first one counts")
        XCTAssertEqual(timings.mark(.audio, at: start + .seconds(1)), 1000)
        timings.end(at: start + .seconds(5))
        XCTAssertEqual(timings.mark(.transcript, at: start + .seconds(5) + .milliseconds(10)), 10, "a new turn starts over")
    }
}

final class PCMPackerTests: XCTestCase {
    func testDefaultFrameIsSixtyMilliseconds() {
        XCTAssertEqual(PCMPacker.defaultFrameBytes, 1920)
        XCTAssertEqual(PCM16.seconds(bytes: PCMPacker.defaultFrameBytes), 0.06, accuracy: 1e-9)
    }

    func testFramesAreCutToSizeAcrossAppends() {
        var packer = PCMPacker(frameBytes: 8)
        XCTAssertEqual(packer.append(Data(0..<5)), [])
        let frames = packer.append(Data(5..<20))
        XCTAssertEqual(frames, [Data(0..<8), Data(8..<16)])
        XCTAssertEqual(packer.flush(), Data(16..<20))
        XCTAssertNil(packer.flush())
    }

    func testFlushKeepsWholeSamplesOnly() {
        var packer = PCMPacker(frameBytes: 8)
        _ = packer.append(Data([1, 2, 3]))
        XCTAssertEqual(packer.flush(), Data([1, 2]))
    }
}

final class PCM16Tests: XCTestCase {
    func testOutputCapacityCoversTheResample() {
        XCTAssertGreaterThanOrEqual(PCM16.outputCapacity(inFrames: 1024, inRate: 48_000), 342)
        XCTAssertGreaterThanOrEqual(PCM16.outputCapacity(inFrames: 1024, inRate: 44_100), 372)
        XCTAssertEqual(PCM16.outputCapacity(inFrames: 100, inRate: 0), 0)
    }

    func testSampleClipsAndRejectsNaN() {
        XCTAssertEqual(PCM16.sample(0), 0)
        XCTAssertEqual(PCM16.sample(1), 32767)
        XCTAssertEqual(PCM16.sample(-1), -32767)
        XCTAssertEqual(PCM16.sample(3), 32767)
        XCTAssertEqual(PCM16.sample(.nan), 0)
    }

    func testLevel() {
        XCTAssertEqual(PCM16.level(of: Data()), 0)
        XCTAssertEqual(PCM16.level(of: Data(count: 640)), 0, "silence")
        var loud = Data()
        for _ in 0..<320 { loud.append(contentsOf: [0xFF, 0x3F]) }
        XCTAssertGreaterThan(PCM16.level(of: loud), 0.8)
        XCTAssertLessThanOrEqual(PCM16.level(of: loud), 1)
    }

    func testStreamPlayerSamples() {
        XCTAssertEqual(PCMStreamPlayer.floats(fromS16le: Data([0x00, 0x00, 0x00, 0x40, 0x00, 0x80])), [0, 0.5, -1])
        XCTAssertEqual(PCMStreamPlayer.floats(fromS16le: Data([0x00, 0x40, 0x07])), [0.5], "half a sample is dropped")
    }
}

final class WAVWriterTests: XCTestCase {
    func testHeaderLayout() {
        let header = WAVWriter.header(dataBytes: 3200)
        XCTAssertEqual(header.count, 44)
        XCTAssertEqual(String(decoding: header[0..<4], as: UTF8.self), "RIFF")
        XCTAssertEqual(header.subdata(in: 4..<8), Data([0xA4, 0x0C, 0, 0]), "36 + 3200")
        XCTAssertEqual(String(decoding: header[8..<16], as: UTF8.self), "WAVEfmt ")
        XCTAssertEqual(header.subdata(in: 20..<22), Data([1, 0]), "PCM")
        XCTAssertEqual(header.subdata(in: 22..<24), Data([1, 0]), "mono")
        XCTAssertEqual(header.subdata(in: 24..<28), Data([0x80, 0x3E, 0, 0]), "16000 Hz")
        XCTAssertEqual(header.subdata(in: 28..<32), Data([0x00, 0x7D, 0, 0]), "32000 bytes a second")
        XCTAssertEqual(header.subdata(in: 34..<36), Data([16, 0]))
        XCTAssertEqual(String(decoding: header[36..<40], as: UTF8.self), "data")
        XCTAssertEqual(header.subdata(in: 40..<44), Data([0x80, 0x0C, 0, 0]))
    }

    func testFinishPatchesTheSizes() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("wav-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: url) }
        let writer = try XCTUnwrap(WAVWriter(url: url))
        writer.append(Data(count: 1000))
        writer.append(Data(count: 600))
        writer.finish()
        let file = try Data(contentsOf: url)
        XCTAssertEqual(file.count, 44 + 1600)
        XCTAssertEqual(file.prefix(44), WAVWriter.header(dataBytes: 1600))
    }
}

final class AudioStreamConverterTests: XCTestCase {
    func testFortyEightKilohertzFloatBecomesSixteenKilohertzS16() throws {
        let format = try XCTUnwrap(AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48_000, channels: 1, interleaved: false))
        let converter = try XCTUnwrap(AudioStreamConverter(input: format))
        var total = 0
        for _ in 0..<10 {
            let buffer = try XCTUnwrap(AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 1024))
            buffer.frameLength = 1024
            for i in 0..<1024 { buffer.floatChannelData![0][i] = 0.5 * sinf(Float(i) * 0.1) }
            total += converter.convert(buffer)?.count ?? 0
        }
        // 10 x 1024 frames at 48 kHz are 3413 frames at 16 kHz; the filter holds back a few.
        XCTAssertEqual(Double(total / 2), 3413, accuracy: 120)
    }
}

final class VoiceFallbackTests: XCTestCase {
    func testCommandProblems() {
        XCTAssertNotNil(StreamedCommand.problem("no_speech"))
        XCTAssertNotEqual(StreamedCommand.problem("no_speech"), StreamedCommand.problem("busy"))
        XCTAssertNotNil(StreamedCommand.problem("anything_else"))
    }

    func testOldServerNeedsNoCodecEcho() {
        let old = VoiceEvent.Listening(purpose: nil, codec: nil, rate: nil, turn: nil)
        XCTAssertFalse(old.understandsStreaming, "a client that asked for pcm16 notes cancels and uses the share upload")
        XCTAssertTrue(VoiceEvent.Listening(purpose: "note", codec: "pcm16", rate: 16000, turn: "t").understandsStreaming)
    }

    func testWavNotesUseTheWavMimeAndName() {
        var note = VoiceNote(url: URL(fileURLWithPath: "/tmp/x.wav"), duration: 2)
        XCTAssertEqual(note.mime, "audio/aac")
        XCTAssertEqual(note.fileExtension, "aac")
        note.mime = "audio/wav"
        XCTAssertEqual(note.fileExtension, "wav")
        XCTAssertTrue(VoiceNoteFiles.fileName(for: Date(timeIntervalSince1970: 0), ext: "wav").hasSuffix(".wav"))
    }
}
