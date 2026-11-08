import XCTest
@testable import Secretary

/// Lucina message format v1 (docs/lucina-message-format.md).
final class RichMessageTests: XCTestCase {
    private func json(_ s: String) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: Data(s.utf8))
    }

    /// The owner's 2026-10-22 "inventario" answer, as DeepSeek formats it now (trimmed).
    private let inventario = """
    {"format":"v1","lang":"it",
     "bubbles":["Ecco l'inventario di ciò che è live ora ✅","In sintesi: approvazioni strutturate e morning brief **no** ⚠️"],
     "blocks":[
      {"type":"checklist","title":"Live adesso","items":[{"text":"Notifiche app: push PWA, iOS, inbox","done":true},{"text":"Morning brief"}]},
      {"type":"card","emoji":"⚠️","title":"Limiti certi","tone":"warning","body":"Niente email."},
      {"type":"table","columns":["a","b","c","d","e"],"rows":[["1","2","3","4","5"],["6"],["7"],["8"],["9"],["10"],["11"]]},
      {"type":"link_preview","url":"javascript:alert(1)","title":"evil"},
      {"type":"iframe","src":"x"}
     ],
     "actions":[
      {"id":"details","label":"Dettagli","emoji":"🔎","kind":"reply","payload":{"text":"Dammi i dettagli"}},
      {"id":"hint-reminder","label":"Promemoria","kind":"reply","payload":{"text":"Ricordami alle 21:30 di","prefill":true}},
      {"id":"out","label":"Out","kind":"route","payload":{"path":"https://evil.example"}},
      {"id":"up","label":"Up","kind":"route","payload":{"path":"/../x"}},
      {"id":"notes","label":"Note","kind":"route","payload":{"path":"/notes"}},
      {"id":"bad","label":"Bad","kind":"confirm","payload":{"answer":"maybe","text":"x"}},
      {"id":"yes","label":"Sì","kind":"confirm","payload":{"answer":"yes","text":"sì"}}
     ]}
    """

    func testParseKeepsWhatFitsAndDropsTheRest() throws {
        let m = try XCTUnwrap(RichMessage.parse(try json(inventario)))
        XCTAssertEqual(m.bubbles.count, 2)
        XCTAssertEqual(m.lang, "it")
        XCTAssertEqual(m.blocks.count, 3, "unsafe link and unknown block dropped")
        guard case let .table(_, columns, rows) = m.blocks[2] else { return XCTFail("table") }
        XCTAssertEqual(columns.count, 4)
        XCTAssertEqual(rows.count, 6)
        XCTAssertEqual(m.actions.map(\.id), ["details", "hint-reminder", "notes", "yes"])
        XCTAssertTrue(m.actions[1].prefill)
        XCTAssertFalse(m.actions[1].sendsMessage)
        XCTAssertTrue(m.actions[3].sendsMessage)
        XCTAssertNil(RichMessage.parse(try json(#"{"format":"v2","bubbles":["x"]}"#)))
        XCTAssertNil(RichMessage.parse(try json(#"{"format":"v1","bubbles":[]}"#)))
    }

    /// The shared vectors (tests/conformance/rich/*.json, copied into the test bundle by
    /// project.yml), also run by the server (crates/lucina-store/tests/conformance_rich.rs) and the
    /// web (richConformance.test.ts).
    func testConformanceVectors() throws {
        let dir = try XCTUnwrap(Bundle(for: Self.self).resourceURL).appendingPathComponent("rich")
        let files = try FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
        XCTAssertGreaterThanOrEqual(files.count, 4)
        for file in files {
            struct Expect: Decodable { let bubbles: [String]; let blocks: [String]; let actions: [String]; let speech: String }
            struct Vector: Decodable { let name: String; let input: JSONValue; let expect: Expect }
            let v = try JSONDecoder().decode(Vector.self, from: Data(contentsOf: file))
            let m = try XCTUnwrap(RichMessage.parse(v.input), v.name)
            XCTAssertEqual(m.bubbles, v.expect.bubbles, v.name)
            XCTAssertEqual(m.blocks.map(Self.kind), v.expect.blocks, v.name)
            XCTAssertEqual(m.actions.map(\.id), v.expect.actions, v.name)
            XCTAssertEqual(m.speechText, v.expect.speech, v.name)
        }
    }

    private static func kind(_ b: RichBlock) -> String {
        switch b {
        case .checklist: return "checklist"
        case .card: return "card"
        case .stat: return "stat"
        case .list: return "list"
        case .table: return "table"
        case .steps: return "steps"
        case .quote: return "quote"
        case .link: return "link_preview"
        case .place: return "place"
        }
    }

    func testSpeechReadsOnlyTheBubblesWithoutMarkdown() throws {
        let m = try XCTUnwrap(RichMessage.parse(try json(inventario)))
        XCTAssertEqual(m.speechText, "Ecco l'inventario di ciò che è live ora ✅ In sintesi: approvazioni strutturate e morning brief no ⚠️")
    }

    func testTimelineItemsCarryTheRichMessageAndSurviveTheCache() throws {
        let raw = """
        {"id":"01K","ts":1761140000000,"channel":"web","role":"lucina","text":"plain fallback",
         "meta":\(inventario)}
        """
        let item = try JSONDecoder().decode(TimelineItem.self, from: Data(raw.utf8))
        XCTAssertEqual(item.rich?.bubbles.first, "Ecco l'inventario di ciò che è live ora ✅")
        XCTAssertEqual(item.text, "plain fallback")
        let cached = try JSONDecoder().decode(TimelineItem.self, from: JSONEncoder().encode(item))
        XCTAssertEqual(cached.rich, item.rich)
        let plain = try JSONDecoder().decode(TimelineItem.self, from: Data(#"{"id":"2","ts":1,"channel":"web","role":"lucina","text":"**Ciao**"}"#.utf8))
        XCTAssertNil(plain.rich)
    }

    func testChatResponseDecodesTheRichFields() throws {
        let body = inventario.replacingOccurrences(of: #"{"format":"v1","#, with: #"{"reply":"fallback","action":"reply","job":null,"lucinaItemId":"L1","format":"v1","#)
        let response = try JSONDecoder().decode(ChatResponse.self, from: Data(body.utf8))
        XCTAssertEqual(response.reply, "fallback")
        XCTAssertEqual(response.lucinaItemId, "L1")
        XCTAssertEqual(response.rich?.actions.count, 4)
        let old = try JSONDecoder().decode(ChatResponse.self, from: Data(#"{"reply":"Ciao","job":null}"#.utf8))
        XCTAssertNil(old.rich)
    }

    func testMarkdownIsStrippedAndPlainRepliesRenderInline() {
        XCTAssertEqual(RichMessage.stripMarkdown("**Fatto** ✅ *ok* [qui](https://e.com) `x`"), "Fatto ✅ ok qui x")
        let rendered = NoteText.inline("**Inventario** e [link](javascript:alert(1))")
        XCTAssertEqual(String(rendered.characters), "Inventario e link")
        XCTAssertTrue(rendered.runs.allSatisfy { $0.link == nil }, "unsafe links never become links")
    }

    func testTypingDelayGrowsWithTheBubble() {
        XCTAssertEqual(RichMessage.typingDelay(""), .milliseconds(350))
        XCTAssertEqual(RichMessage.typingDelay(String(repeating: "x", count: 1000)), .milliseconds(900))
    }

    /// Lucina's "Naviga" buttons: long `/map/place?navigate=1&…` routes to the in-app navigation
    /// (they were dropped by the 121-character app-path rule until 2026-10-28).
    func testNavigaMapRoutesSurvive() throws {
        let path = "/map/place?navigate=1&lat=56.172282&lng=10.192006&title=Security%20and%20Privacy&when=2026-11-02T07%3A00%3A00.000Z&address=5789-003B%20Seminarrum&end=2026-11-02T11%3A00%3A00.000Z&eventId=iphone%3A317472A7-C62C-48AE-A2C5-B500C692EFFC%3A2020%21261176-15%40mytimetable.au.dk%401793602800000"
        let raw = try json("""
        {"format":"v1","bubbles":["Lunedì hai Security and Privacy 📅"],
         "actions":[
          {"id":"nav-1","label":"Naviga","emoji":"🧭","kind":"route","payload":{"path":"\(path)"}},
          {"id":"nomap","label":"x","kind":"route","payload":{"path":"/map/place?"}},
          {"id":"js","label":"x","kind":"route","payload":{"path":"/map/place?title=a<script>"}},
          {"id":"other","label":"x","kind":"route","payload":{"path":"/notes?x=1"}}
         ]}
        """)
        let message = try XCTUnwrap(RichMessage.parse(raw))
        XCTAssertEqual(message.actions.map(\.id), ["nav-1"])
        let action = try XCTUnwrap(message.actions.first)
        XCTAssertEqual(action.path, path)
        XCTAssertTrue(PlaceLinks.navigates(path))
        XCTAssertEqual(Place.fromRoute(path)?.title, "Security and Privacy")
        XCTAssertFalse(RichMessage.isMapRoute("/map/place?title=" + String(repeating: "x", count: 600)))
    }

    // MARK: Secret cards (SecretCard.swift)

    func testASecretActionKeepsNamesAndTheRequestAndDropsValues() throws {
        let raw = try json("""
        {"format":"v1","bubbles":["claude-code asks for the secret GBGO_WIFI_PSK"],
         "actions":[
          {"id":"secret-request","label":"Add GBGO_WIFI_PSK","kind":"secret","payload":{"mode":"save","name":"GBGO_WIFI_PSK",
            "project":"lucinaos","environment":"prod","path":"/","target":"lucinaos/prod","request":"sr-abc","value":"must-not-survive"}},
          {"id":"secret-request-dismiss","label":"Not now","kind":"dismiss","payload":{}},
          {"id":"bad-mode","label":"x","kind":"secret","payload":{"mode":"reveal"}}
         ]}
        """)
        let m = try XCTUnwrap(RichMessage.parse(raw))
        XCTAssertEqual(m.actions.map(\.id), ["secret-request", "secret-request-dismiss"])
        let s = try XCTUnwrap(m.actions[0].secret)
        XCTAssertEqual(s.request, "sr-abc")
        XCTAssertEqual(s.name, "GBGO_WIFI_PSK")
        let encoded = String(decoding: try JSONEncoder().encode(m), as: UTF8.self)
        XCTAssertFalse(encoded.contains("must-not-survive"))
        XCTAssertFalse(m.actions[0].sendsMessage)
    }

    func testAnOldActionReadsItsTargetIntoAPlace() {
        let e = SecretAction(mode: "save", name: "POKO_KEY", target: "poko/prod/Pokobot/sub", held: "held-1").entry
        XCTAssertEqual([e.name, e.project, e.environment, e.path], ["POKO_KEY", "poko", "prod", "/Pokobot/sub"])
        let bare = SecretAction().entry
        XCTAssertEqual([bare.name, bare.project, bare.environment, bare.path], ["", "", "", "/"])
    }

    func testProjectsAndFolders() {
        let p = SecretProject(id: "p-1", name: "LucinaOS kernel", slug: "lucinaos-x1", alias: "los", environments: [])
        for ref in ["p-1", "los", "LOS", "lucinaos-x1", "lucinaos kernel", "lucinaos"] {
            XCTAssertEqual(SecretProject.find(ref, in: [p])?.id, "p-1", ref)
        }
        XCTAssertNil(SecretProject.find("poko", in: [p]))
        XCTAssertEqual(SecretFolder.normalize(""), "/")
        XCTAssertEqual(SecretFolder.normalize("gbgo/wifi/"), "/gbgo/wifi")
        XCTAssertNil(SecretFolder.normalize("/a/../b"))
        XCTAssertNil(SecretFolder.normalize("/a b"))
        XCTAssertTrue(SecretName.isValid("GBGO_WIFI_PSK"))
        XCTAssertFalse(SecretName.isValid("a b"))
    }

    /// `GET /v1/timeline/cards`: a card with no button still closes.
    func testServerCardsDecode() throws {
        let body = #"{"closed":true,"status":"decided","action":"dc-1-2"}"#
        let card = try JSONDecoder().decode(ServerCard.self, from: Data(body.utf8))
        XCTAssertEqual(card, ServerCard(closed: true, status: "decided", action: "dc-1-2"))
        let bare = try JSONDecoder().decode(ServerCard.self, from: Data(#"{"closed":true,"status":"expired"}"#.utf8))
        XCTAssertNil(bare.action)
        XCTAssertTrue(bare.closed)
    }

    /// The bubble of tests/conformance/rich/linebreaks-lists.json as the chat draws it.
    @MainActor func testChatBubbleKeepsLineBreaksAndDrawsListBullets() {
        let shown = String(ChatMarkdown.inline("Two things:\n- the first\n- the second\n\nThat is all.").characters)
        XCTAssertEqual(shown, "Two things:\n• the first\n• the second\n\nThat is all.")
        XCTAssertEqual(ChatMarkdown.listBullets("  * nested\nnot-a-list - here"), "  • nested\nnot-a-list - here")
    }
}
