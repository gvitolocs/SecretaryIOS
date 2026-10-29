import XCTest
@testable import Secretary

/// "Who called?": the Call Directory list (parse, validate, sort, dedup, file), the screen request and
/// response coding (the contract's JSON), the number field's parser and the store's sync and lookup.
@MainActor
final class CallersTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    // MARK: Contract JSON

    nonisolated static let directoryJSON = """
    {"generatedAt": "2026-10-29T12:00:00Z",
     "entries": [{"number": 4570405205, "label": "Telemarketing 86%"}, {"number": 35794095733, "label": "Likely scam (0.93)"}]}
    """

    nonisolated static let screenJSON = """
    {
      "lang": "en",
      "results": [{
        "number": "+4570405205",
        "display": "+45 70 40 52 05",
        "country": "DK", "countryName": "Denmark",
        "type": "mobile",
        "spam": 0.86,
        "kind": "telemarketing",
        "kindP": 0.81,
        "pickUp": 0.12,
        "label": "Likely telemarketing (0.86)",
        "short": "Telemarketing 86%",
        "source": "jev",
        "known": {"contact": null, "previousCalls": 2, "lastCall": "2026-10-28T10:00:00Z", "messages": 0},
        "reasons": ["Danish 70 number (company line)", "called 2 times before"]
      }, {
        "number": "abc", "display": "abc", "type": "invalid", "source": "rules", "label": "Not a phone number"
      }],
      "table": "| Number | Spam | Kind | Pick up |\\n|---|---|---|---|\\n| +45 70 40 52 05 | 0.86 | telemarketing 0.81 | 0.12 |"
    }
    """

    func testDirectoryResponseDecodesTheContractExample() throws {
        let response = try Fixtures.decode(CallerDirectoryResponse.self, Self.directoryJSON)
        XCTAssertEqual(response.generatedAt, "2026-10-29T12:00:00Z")
        XCTAssertEqual(response.generatedDate, Date(timeIntervalSince1970: 1_793_275_200))
        XCTAssertEqual(response.entries, [CallerDirectoryEntry(number: 4_570_405_205, label: "Telemarketing 86%"),
                                          CallerDirectoryEntry(number: 35_794_095_733, label: "Likely scam (0.93)")])
    }

    func testDirectoryResponseSkipsBadEntriesAndAcceptsNumericStrings() throws {
        let json = """
        {"entries": [{"number": 4570405205, "label": "A"}, {"number": "4570405206", "label": "B"}, {"label": "no number"},
                     {"number": 1.5, "label": "fraction"}, {"number": 4570405207}, "junk", null, {"number": 4570405208, "label": "C"}]}
        """
        let response = try Fixtures.decode(CallerDirectoryResponse.self, json)
        XCTAssertNil(response.generatedAt)
        XCTAssertEqual(response.entries.map(\.number), [4_570_405_205, 4_570_405_206, 4_570_405_208])
        XCTAssertEqual(response.entries.map(\.label), ["A", "B", "C"])
    }

    func testEmptyDirectoryDecodes() throws {
        let response = try Fixtures.decode(CallerDirectoryResponse.self, #"{"generatedAt":"2026-10-29T12:00:00.250Z","entries":[]}"#)
        XCTAssertTrue(response.entries.isEmpty)
        XCTAssertNotNil(response.generatedDate)
    }

    func testScreenResponseDecodesTheContractExample() throws {
        let response = try Fixtures.decode(CallerScreenResponse.self, Self.screenJSON)
        XCTAssertEqual(response.lang, "en")
        XCTAssertEqual(response.results.count, 2)
        let spam = response.results[0]
        XCTAssertEqual(spam.number, "+4570405205")
        XCTAssertEqual(spam.display, "+45 70 40 52 05")
        XCTAssertEqual(spam.countryName, "Denmark")
        XCTAssertEqual(spam.type, "mobile")
        XCTAssertEqual(spam.spam, 0.86)
        XCTAssertEqual(spam.kind, "telemarketing")
        XCTAssertEqual(spam.kindP, 0.81)
        XCTAssertEqual(spam.pickUp, 0.12)
        XCTAssertEqual(spam.label, "Likely telemarketing (0.86)")
        XCTAssertEqual(spam.short, "Telemarketing 86%")
        XCTAssertEqual(spam.source, "jev")
        XCTAssertNil(spam.known?.contact)
        XCTAssertEqual(spam.known?.previousCalls, 2)
        XCTAssertEqual(spam.known?.lastCall, "2026-10-28T10:00:00Z")
        XCTAssertEqual(spam.reasons, ["Danish 70 number (company line)", "called 2 times before"])
        XCTAssertFalse(spam.isInvalid)
        XCTAssertTrue(response.table?.hasPrefix("| Number | Spam") == true)
        let invalid = response.results[1]
        XCTAssertTrue(invalid.isInvalid)
        XCTAssertEqual(invalid.label, "Not a phone number")
        XCTAssertNil(invalid.spam)
        XCTAssertEqual(invalid.reasons, [])
    }

    func testScreenRequestEncodesTheContractShape() throws {
        let request = CallerScreenRequest(numbers: ["+45 70 40 52 05", "35794095733"], lang: "da")
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: APIClient.encoder.encode(request)) as? [String: Any])
        XCTAssertEqual(object["numbers"] as? [String], ["+45 70 40 52 05", "35794095733"])
        XCTAssertEqual(object["lang"] as? String, "da")
        XCTAssertNil(object["context"], "a lookup carries no context")
        XCTAssertNil(object["call"], "a lookup is not recorded as an incoming call")

        let full = CallerScreenRequest(numbers: ["+4570405205"], lang: nil,
                                       context: .init(contact: "Name"),
                                       call: .init(source: "android", at: "2026-10-29T12:00:00Z"))
        let fullObject = try XCTUnwrap(JSONSerialization.jsonObject(with: APIClient.encoder.encode(full)) as? [String: Any])
        XCTAssertNil((fullObject["context"] as? [String: Any])?["courier"], "no guesses about who calls are sent")
        XCTAssertEqual((fullObject["context"] as? [String: Any])?["contact"] as? String, "Name")
        XCTAssertEqual((fullObject["call"] as? [String: Any])?["source"] as? String, "android")
    }

    // MARK: Validate, sort, dedup

    func testNormalizeSortsAndRemovesDuplicates() {
        let list = CallerDirectoryList.normalize([
            .init(number: 4_570_405_206, label: "second"),
            .init(number: 4_570_405_205, label: "first"),
            .init(number: 4_570_405_206, label: "duplicate"),
            .init(number: 35_794_095_733, label: "cyprus"),
            .init(number: 4_570_405_205, label: "another duplicate"),
        ])
        XCTAssertEqual(list.map(\.number), [4_570_405_205, 4_570_405_206, 35_794_095_733])
        XCTAssertEqual(list.map(\.label), ["first", "second", "cyprus"], "the first entry of a number wins")
        XCTAssertEqual(zip(list, list.dropFirst()).allSatisfy { $0.number < $1.number }, true)
    }

    func testNormalizeDropsInvalidNumbersAndEmptyLabels() {
        let list = CallerDirectoryList.normalize([
            .init(number: 0, label: "zero"),
            .init(number: -4_570_405_205, label: "negative"),
            .init(number: 123_456, label: "too short"),
            .init(number: 1_000_000_000_000_000, label: "too long"),
            .init(number: 4_570_405_205, label: "   \n\t "),
            .init(number: 4_570_405_206, label: ""),
            .init(number: 1_000_000, label: "shortest"),
            .init(number: 999_999_999_999_999, label: "longest"),
        ])
        XCTAssertEqual(list.map(\.label), ["shortest", "longest"])
    }

    func testCleanLabel() {
        XCTAssertEqual(CallerDirectoryList.cleanLabel("  Telemarketing\n86%\t "), "Telemarketing 86%")
        XCTAssertEqual(CallerDirectoryList.cleanLabel("a\u{0007}b\u{200B}c"), "a b c")
        XCTAssertEqual(CallerDirectoryList.cleanLabel("Spam: Lucina's Pokémon ✓"), "Spam: Lucina's Pokémon ✓")
        let long = String(repeating: "x", count: 200)
        XCTAssertEqual(CallerDirectoryList.cleanLabel(long).count, CallerDirectoryList.maxLabelLength)
        XCTAssertEqual(CallerDirectoryList.cleanLabel(" \n "), "")
    }

    func testNormalizeCapsTheListAtTheLowestNumbers() {
        let entries = (0 ..< CallerDirectoryList.maxEntries + 50).reversed().map {
            CallerDirectoryEntry(number: 4_500_000_000 + Int64($0), label: "n\($0)")
        }
        let list = CallerDirectoryList.normalize(entries)
        XCTAssertEqual(list.count, CallerDirectoryList.maxEntries)
        XCTAssertEqual(list.first?.number, 4_500_000_000)
        XCTAssertEqual(list.last?.number, 4_500_000_000 + Int64(CallerDirectoryList.maxEntries) - 1)
    }

    // MARK: File

    func testFileRoundTrip() throws {
        let snapshot = CallerDirectorySnapshot(generatedAt: "2026-10-29T12:00:00Z", entries: [
            .init(number: 35_794_095_733, label: "Likely scam (0.93)"), .init(number: 4_570_405_205, label: "Telemarketing 86%"),
        ])
        let data = CallerDirectoryList.encode(snapshot)
        let text = String(decoding: data, as: UTF8.self)
        XCTAssertEqual(text, "SCD1\n2026-10-29T12:00:00Z\n2\n4570405205\tTelemarketing 86%\n35794095733\tLikely scam (0.93)\n")
        let back = try XCTUnwrap(CallerDirectoryList.decode(data))
        XCTAssertEqual(back.generatedAt, "2026-10-29T12:00:00Z")
        XCTAssertEqual(back.entries.map(\.number), [4_570_405_205, 35_794_095_733])
        XCTAssertEqual(back.entries.map(\.label), ["Telemarketing 86%", "Likely scam (0.93)"])
    }

    func testEmptyFileRoundTrip() throws {
        let data = CallerDirectoryList.encode(CallerDirectorySnapshot(generatedAt: nil, entries: []))
        XCTAssertEqual(String(decoding: data, as: UTF8.self), "SCD1\n-\n0\n")
        let back = try XCTUnwrap(CallerDirectoryList.decode(data))
        XCTAssertNil(back.generatedAt)
        XCTAssertTrue(back.entries.isEmpty)
    }

    func testDecodeRejectsDamagedFiles() {
        let good = "SCD1\n-\n2\n4570405205\tA\n4570405206\tB\n"
        XCTAssertNotNil(CallerDirectoryList.decode(Data(good.utf8)))
        XCTAssertNil(CallerDirectoryList.decode(Data("SCD2\n-\n0\n".utf8)), "wrong version")
        XCTAssertNil(CallerDirectoryList.decode(Data("SCD1\n-\n3\n4570405205\tA\n4570405206\tB\n".utf8)), "truncated: fewer lines than the count")
        XCTAssertNil(CallerDirectoryList.decode(Data("SCD1\n-\nx\n".utf8)), "bad count")
        XCTAssertNil(CallerDirectoryList.decode(Data("SCD1\n-\n1\nnot-a-number\tA\n".utf8)), "bad number")
        XCTAssertNil(CallerDirectoryList.decode(Data("SCD1\n-\n1\n4570405205 A\n".utf8)), "no tab")
        XCTAssertNil(CallerDirectoryList.decode(Data([0xFF, 0xFE, 0x00])), "not text")
        XCTAssertNil(CallerDirectoryList.decode(Data()))
    }

    func testDecodeSortsWhatTheFileGotWrong() throws {
        // CallKit rejects an unsorted list, so the extension never trusts the file's order.
        let messy = "SCD1\n-\n4\n4570405206\tB\n4570405205\tA\n4570405206\tB again\n0\tzero\n"
        let back = try XCTUnwrap(CallerDirectoryList.decode(Data(messy.utf8)))
        XCTAssertEqual(back.entries, [.init(number: 4_570_405_205, label: "A"), .init(number: 4_570_405_206, label: "B")])
    }

    func testLargeListStaysSmall() {
        let entries = (0 ..< 5000).map { CallerDirectoryEntry(number: 4_570_000_000 + Int64($0) * 7, label: "Telemarketing 86%") }
        let data = CallerDirectoryList.encode(CallerDirectorySnapshot(generatedAt: "2026-10-29T12:00:00Z", entries: entries))
        XCTAssertLessThan(data.count, 300_000)
        XCTAssertEqual(CallerDirectoryList.decode(data)?.entries.count, 5000)
    }

    // MARK: The number field

    func testParseNumbers() {
        XCTAssertEqual(CallerNumbers.parse("+45 70 40 52 05"), ["+45 70 40 52 05"])
        XCTAssertEqual(CallerNumbers.parse("  70405205  "), ["70405205"])
        XCTAssertEqual(CallerNumbers.parse("+45 70 40 52 05\n+357 94 095733\n"), ["+45 70 40 52 05", "+357 94 095733"])
        XCTAssertEqual(CallerNumbers.parse("+4570405205, 35794095733; 12345678"), ["+4570405205", "35794095733", "12345678"])
        XCTAssertEqual(CallerNumbers.parse("+45 70 40 52 05 +357 94 095733"), ["+45 70 40 52 05", "+357 94 095733"])
        XCTAssertEqual(CallerNumbers.parse("tel:+4570405205\r\ntel:+35794095733"), ["+4570405205", "+35794095733"])
        XCTAssertEqual(CallerNumbers.parse("+45 70 40 52 05\n+4570405205\n+45-70-40-52-05"), ["+45 70 40 52 05"],
                       "the same digits three times are one number")
        XCTAssertEqual(CallerNumbers.parse("\n \n,,;"), [])
        XCTAssertEqual(CallerNumbers.parse("no number here"), [])
        XCTAssertEqual(CallerNumbers.parse(""), [])
    }

    func testChunksOfTwenty() {
        let numbers = (0 ..< 45).map { "+4570400\(String(format: "%03d", $0))" }
        let chunks = CallerNumbers.chunks(numbers)
        XCTAssertEqual(chunks.map(\.count), [20, 20, 5])
        XCTAssertEqual(chunks.flatMap { $0 }, numbers)
        XCTAssertEqual(CallerNumbers.chunks([]), [])
    }

    // MARK: Text

    func testPercent() {
        XCTAssertEqual(CallerText.percent(0.86), "86%")
        XCTAssertEqual(CallerText.percent(0.125), "13%")
        XCTAssertEqual(CallerText.percent(0), "0%")
        XCTAssertEqual(CallerText.percent(1.4), "100%")
        XCTAssertEqual(CallerText.percent(nil), "–")
        XCTAssertEqual(CallerText.percent(.nan), "–")
    }

    func testRequestLanguage() {
        XCTAssertEqual(CallersStore.requestLanguage(["da"]), "da")
        XCTAssertEqual(CallersStore.requestLanguage(["it-IT", "en"]), "it")
        XCTAssertEqual(CallersStore.requestLanguage(["fr", "da-DK"]), "da")
        XCTAssertEqual(CallersStore.requestLanguage(["fr"]), "en")
        XCTAssertEqual(CallersStore.requestLanguage([]), "en")
    }

    // MARK: Sync schedule

    func testSyncIsDueEveryFifteenMinutes() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertTrue(CallersStore.isDue(lastSync: nil, lastAttempt: nil, now: now))
        XCTAssertFalse(CallersStore.isDue(lastSync: now.addingTimeInterval(-14 * 60), lastAttempt: nil, now: now))
        XCTAssertTrue(CallersStore.isDue(lastSync: now.addingTimeInterval(-15 * 60), lastAttempt: nil, now: now))
        // A failed attempt waits a minute before the next, even though nothing ever synced.
        XCTAssertFalse(CallersStore.isDue(lastSync: nil, lastAttempt: now.addingTimeInterval(-30), now: now))
        XCTAssertTrue(CallersStore.isDue(lastSync: nil, lastAttempt: now.addingTimeInterval(-61), now: now))
    }

    // MARK: Store

    /// A CallKit stand-in: the extension's status and the reloads asked of it.
    final class FakeDirectory: CallDirectoryManaging, @unchecked Sendable {
        var status: CallerExtensionStatus = .enabled
        var reloads = 0
        var reloadError: Error?
        func enabledStatus() async -> CallerExtensionStatus { status }
        func reload() async throws {
            reloads += 1
            if let reloadError { throw reloadError }
        }
        @MainActor func openSettings() async {}
    }

    private struct Harness {
        let store: CallersStore
        let directory: FakeDirectory
        let defaults: UserDefaults
        let saved: Saved
    }

    final class Saved: @unchecked Sendable {
        var snapshot: CallerDirectorySnapshot?
        var writes = 0
        var removed = false
    }

    private func makeStore(status: CallerExtensionStatus = .enabled, signedIn: Bool = true) -> Harness {
        let name = "callers-tests-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        addTeardownBlock { defaults.removePersistentDomain(forName: name) }
        let directory = FakeDirectory()
        directory.status = status
        let saved = Saved()
        let client = StubURLProtocol.makeClient()
        let store = CallersStore(api: { signedIn ? CallersAPI(client: client) : nil }, manager: directory, defaults: defaults,
                                 readSaved: { saved.snapshot }, writeSaved: { saved.snapshot = $0; saved.writes += 1 },
                                 removeSaved: { saved.snapshot = nil; saved.removed = true })
        return Harness(store: store, directory: directory, defaults: defaults, saved: saved)
    }

    private func serveDirectory(_ json: String = CallersTests.directoryJSON) {
        StubURLProtocol.handler = { request in
            XCTAssertEqual(request.url?.path, "/v1/callers/directory")
            XCTAssertEqual(request.httpMethod, "GET")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer sec_test")
            return .init(body: json)
        }
    }

    func testSyncSavesTheListAndReloadsTheExtension() async throws {
        let h = makeStore()
        serveDirectory()
        await h.store.sync()
        XCTAssertNil(h.store.syncError)
        XCTAssertEqual(h.saved.snapshot?.entries.map(\.number), [4_570_405_205, 35_794_095_733])
        XCTAssertEqual(h.saved.snapshot?.generatedAt, "2026-10-29T12:00:00Z")
        XCTAssertEqual(h.directory.reloads, 1)
        XCTAssertEqual(h.store.entryCount, 2)
        XCTAssertNotNil(h.store.lastSync)
        XCTAssertEqual(h.store.extensionStatus, .enabled)
        XCTAssertEqual(h.defaults.integer(forKey: CallersStore.countKey), 2)
    }

    func testSyncSortsAndDedupsWhatTheServerSent() async {
        let h = makeStore()
        serveDirectory("""
        {"generatedAt": "2026-10-29T12:00:00Z", "entries": [
          {"number": 35794095733, "label": "b"}, {"number": 4570405205, "label": "a"}, {"number": 4570405205, "label": "again"}, {"number": 5, "label": "bad"}]}
        """)
        await h.store.sync()
        XCTAssertEqual(h.saved.snapshot?.entries, [.init(number: 4_570_405_205, label: "a"), .init(number: 35_794_095_733, label: "b")])
    }

    func testUnchangedListIsNotReloadedAgain() async {
        let h = makeStore()
        serveDirectory()
        await h.store.sync()
        await h.store.sync()
        XCTAssertEqual(h.directory.reloads, 1, "the same labels need no reload")
        serveDirectory("""
        {"generatedAt": "2026-10-29T13:00:00Z", "entries": [{"number": 4570405205, "label": "Telemarketing 90%"}]}
        """)
        await h.store.sync()
        XCTAssertEqual(h.directory.reloads, 2)
        XCTAssertEqual(h.store.entryCount, 1)
    }

    func testDisabledExtensionGetsTheFileButNoReload() async {
        let h = makeStore(status: .disabled)
        serveDirectory()
        await h.store.sync()
        XCTAssertEqual(h.store.extensionStatus, .disabled)
        XCTAssertEqual(h.saved.writes, 1)
        XCTAssertEqual(h.directory.reloads, 0)
        XCTAssertNil(h.store.syncError)
        // Turned on later: the next sync reloads even though the list did not change.
        h.directory.status = .enabled
        await h.store.sync()
        XCTAssertEqual(h.directory.reloads, 1)
    }

    func testReloadFailureIsShownAndTheListStaysSaved() async {
        let h = makeStore()
        h.directory.reloadError = CXErrorFixture.error
        serveDirectory()
        await h.store.sync()
        XCTAssertNotNil(h.store.syncError)
        XCTAssertEqual(h.saved.snapshot?.entries.count, 2)
        XCTAssertEqual(h.store.entryCount, 2)
    }

    func testServerErrorIsShownAndNothingChanges() async {
        let h = makeStore()
        var reported: APIError?
        h.store.onTransportError = { reported = $0 }
        StubURLProtocol.handler = { _ in .init(status: 404, body: #"{"error":{"code":"not_found","message":"no such route"}}"#) }
        await h.store.sync()
        XCTAssertNotNil(h.store.syncError)
        XCTAssertNotNil(reported)
        XCTAssertEqual(h.saved.writes, 0)
        XCTAssertEqual(h.directory.reloads, 0)
        XCTAssertNil(h.store.lastSync)
    }

    func testSignedOutStoreDoesNothing() async {
        let h = makeStore(signedIn: false)
        await h.store.sync()
        await h.store.lookup("+4570405205")
        XCTAssertTrue(StubURLProtocol.requests.isEmpty)
        XCTAssertTrue(h.store.results.isEmpty)
    }

    func testSyncIfDueUsesTheThrottle() async {
        let h = makeStore()
        serveDirectory()
        let now = Date()
        await h.store.syncIfDue(now: now)
        await h.store.syncIfDue(now: now.addingTimeInterval(5 * 60))
        XCTAssertEqual(StubURLProtocol.requests.count, 1)
        await h.store.syncIfDue(now: now.addingTimeInterval(16 * 60))
        XCTAssertEqual(StubURLProtocol.requests.count, 2)
    }

    func testResetRemovesTheSavedListAndEmptiesTheExtension() async throws {
        let h = makeStore()
        serveDirectory()
        await h.store.sync()
        h.store.input = "+4570405205"
        h.store.reset()
        XCTAssertTrue(h.saved.removed)
        XCTAssertNil(h.saved.snapshot)
        XCTAssertNil(h.store.entryCount)
        XCTAssertNil(h.store.lastSync)
        XCTAssertEqual(h.store.input, "")
        XCTAssertNil(h.defaults.object(forKey: CallersStore.lastSyncKey))
        try await Task.sleep(for: .milliseconds(100))
        XCTAssertEqual(h.directory.reloads, 2, "one reload at the sync, one that empties the list")
    }

    func testLookupSendsTheNumbersAndShowsOneRowEach() async throws {
        let h = makeStore()
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/callers/directory" { return .init(body: CallersTests.directoryJSON) }
            XCTAssertEqual(request.url?.path, "/v1/callers/screen")
            XCTAssertEqual(request.httpMethod, "POST")
            return .init(body: CallersTests.screenJSON)
        }
        await h.store.lookup("+45 70 40 52 05\n+357 94 095733")
        XCTAssertNil(h.store.lookupError)
        XCTAssertEqual(h.store.results.count, 2)
        let screen = try XCTUnwrap(StubURLProtocol.requests.first { $0.url?.path == "/v1/callers/screen" })
        let body = try XCTUnwrap(screen.httpBody)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: body) as? [String: Any])
        XCTAssertEqual(object["numbers"] as? [String], ["+45 70 40 52 05", "+357 94 095733"])
        XCTAssertEqual(object["lang"] as? String, CallersStore.requestLanguage())
        XCTAssertNil(object["call"])
    }

    func testLookupOfALongListMakesSeveralRequests() async {
        let h = makeStore()
        StubURLProtocol.handler = { request in
            guard request.url?.path == "/v1/callers/screen", let body = request.httpBody,
                  let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
                  let numbers = object["numbers"] as? [String] else { return .init(body: CallersTests.directoryJSON) }
            XCTAssertLessThanOrEqual(numbers.count, 20)
            let rows = numbers.map { #"{"number":"\#($0)","type":"unknown","source":"rules","reasons":[]}"# }.joined(separator: ",")
            return .init(body: #"{"lang":"en","results":[\#(rows)]}"#)
        }
        let numbers = (0 ..< 45).map { "+4570400\(String(format: "%03d", $0))" }
        await h.store.lookup(numbers.joined(separator: "\n"))
        XCTAssertEqual(h.store.results.map(\.number), numbers)
        XCTAssertEqual(StubURLProtocol.requests.filter { $0.url?.path == "/v1/callers/screen" }.count, 3)
    }

    func testLookupFailureKeepsWhatWasFound() async {
        let h = makeStore()
        var calls = 0
        StubURLProtocol.handler = { request in
            guard request.url?.path == "/v1/callers/screen" else { return .init(body: CallersTests.directoryJSON) }
            calls += 1
            if calls == 1 { return .init(body: #"{"results":[{"number":"+4570405205"}]}"#) }
            return .init(status: 503, body: #"{"error":{"code":"callers_unavailable","message":"Lucina is down"}}"#)
        }
        let numbers = (0 ..< 25).map { "+4570400\(String(format: "%03d", $0))" }
        await h.store.lookup(numbers.joined(separator: "\n"))
        XCTAssertEqual(h.store.results.count, 1)
        XCTAssertNotNil(h.store.lookupError)
    }

    func testEmptyLookupClearsTheResults() async {
        let h = makeStore()
        StubURLProtocol.handler = { _ in .init(body: CallersTests.screenJSON) }
        await h.store.lookup("+4570405205")
        XCTAssertFalse(h.store.results.isEmpty)
        let before = StubURLProtocol.requests.count
        await h.store.lookup("  \n ")
        XCTAssertTrue(h.store.results.isEmpty)
        XCTAssertEqual(StubURLProtocol.requests.count, before, "nothing to look up, nothing sent")
    }

    func testPublicReportDecodesAndDeliveryIsGone() throws {
        let json = """
        {"lang":"en","results":[{"number":"+4520682758","display":"+45 20 68 27 58","type":"mobile",
         "spam":0.67,"kind":"telemarketing","kindP":0.56,"pickUp":0.15,"label":"Likely telemarketing (0.56)",
         "short":"Telemarketing 56%","source":"jev","reasons":["DK mobile","180.dk: 6 reports"],
         "known":{"contact":null,"previousCalls":0,"public":{"source":"180.dk","url":"https://www.180.dk/nummer/20682758",
          "searches":374,"reports":6,"latestComment":"26. oktober 2026 — Irriterende: De ringer i min arbejdstid"}}}]}
        """
        let response = try Fixtures.decode(CallerScreenResponse.self, json)
        let report = try XCTUnwrap(response.results.first?.known?.publicReport)
        XCTAssertEqual(report.source, "180.dk")
        XCTAssertEqual(report.url, "https://www.180.dk/nummer/20682758")
        XCTAssertEqual(report.reports, 6)
        XCTAssertNil(CallerText.kind("delivery"), "no delivery kind any more")
        XCTAssertNotNil(CallerText.kind("telemarketing"))
    }
}

/// An error like the one iOS gives when the extension is off.
private enum CXErrorFixture {
    static let error = NSError(domain: "CXErrorDomainCallDirectoryManager", code: 6, userInfo: [NSLocalizedDescriptionKey: "reload failed"])
}
