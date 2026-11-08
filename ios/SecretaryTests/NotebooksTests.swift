import XCTest
@testable import Secretary

/// Notebooks: the citation parsing and the decoding of the contract in docs/design/notebooks-api.md.
final class NotebooksTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    // MARK: Samples (the contract's examples)

    private let notebookJSON = """
    {"id": "nb4f1c09a2e7", "name": "CNS", "description": "Exam prep", "archived": false,
     "created_at": "2026-10-29T13:00:00Z", "updated_at": "2026-10-29T14:30:00Z",
     "counts": {"sources": 3, "notes": 1, "chats": 2, "podcasts": 0}}
    """

    private let sourceJSON = """
    {"id": "s0a9be3c112", "kind": "pdf", "title": "RM Class 2", "origin": "upload:RM Class 2.pdf",
     "status": "ready", "error": null, "chars": 48211, "chunks": 37, "pages": 22, "duration_s": null,
     "has_file": true, "created_at": "2026-10-29T13:01:00Z", "updated_at": "2026-10-29T13:02:00Z",
     "notebooks": ["nb4f1c09a2e7"], "context": "summary"}
    """

    private let messageJSON = """
    {"id": "m77aa00bb11", "role": "assistant",
     "text": "Reading is slow [source:s0a9be3c112-14]. See also [note:rm-class-2-summary].",
     "citations": [
       {"ref": "source:s0a9be3c112-14", "kind": "source", "source_id": "s0a9be3c112", "chunk": 14,
        "title": "RM Class 2", "excerpt": "The reading speed is...", "page": 7, "start_s": null},
       {"ref": "note:rm-class-2-summary", "kind": "note", "title": "RM Class 2 summary", "excerpt": "Key points"}
     ],
     "model": "deepseek-flash", "created_at": "2026-10-29T14:00:00Z"}
    """

    // MARK: Decoding

    func testDecodesNotebookAndCounts() throws {
        let nb = try Fixtures.decode(Notebook.self, notebookJSON)
        XCTAssertEqual(nb.id, "nb4f1c09a2e7")
        XCTAssertEqual(nb.name, "CNS")
        XCTAssertEqual(nb.counts, NotebookCounts(sources: 3, notes: 1, chats: 2, podcasts: 0))
        XCTAssertFalse(nb.archived)
        XCTAssertNotNil(nb.updated)

        // Only the id is required: a half-empty object still shows up.
        let bare = try Fixtures.decode(Notebook.self, #"{"id": "nbx"}"#)
        XCTAssertEqual(bare.name, "nbx")
        XCTAssertEqual(bare.counts, NotebookCounts())
    }

    func testDecodesNotebookDetailWithEverythingInIt() throws {
        let json = """
        {"id": "nb1", "name": "CNS", "description": "", "archived": false, "created_at": "", "updated_at": "",
         "counts": {"sources": 1, "notes": 1, "chats": 1, "podcasts": 1},
         "sources": [\(sourceJSON), {"broken": true}],
         "notes": [{"slug": "rm-class-2-summary", "title": "Summary", "excerpt": "Key points", "updated_at": "2026-10-29T13:00:00Z", "indexed": true}],
         "chats": [{"id": "c1", "notebook_id": "nb1", "title": "Exam prep", "created_at": "", "updated_at": "", "messages": 4}],
         "podcasts": [{"id": "p1", "notebook_id": "nb1", "title": "Deep dive", "episode_profile": "deep-dive", "speaker_profile": "duo-en",
                       "briefing": "", "status": "voicing", "error": null, "outline": [{"name": "Intro", "description": "d", "size": "short"}],
                       "transcript": [{"speaker": "Lucina", "text": "Hi"}, {"speaker": "Marco", "text": "Hello"}],
                       "duration_s": 312.4, "audio": false, "created_at": ""}]}
        """
        let detail = try Fixtures.decode(NotebookDetail.self, json)
        XCTAssertEqual(detail.notebook.id, "nb1")
        XCTAssertEqual(detail.sources.map(\.id), ["s0a9be3c112"], "a bad entry does not hide the others")
        XCTAssertEqual(detail.notes.first?.slug, "rm-class-2-summary")
        XCTAssertEqual(detail.notes.first?.indexed, true)
        XCTAssertEqual(detail.chats.first?.messageCount, 4)
        let podcast = try XCTUnwrap(detail.podcasts.first)
        XCTAssertTrue(podcast.isPending)
        XCTAssertEqual(podcast.statusLabel.isEmpty, false)
        XCTAssertEqual(podcast.transcript.map(\.speaker), ["Lucina", "Marco"])
        XCTAssertEqual(podcast.transcript.map(\.id), [0, 1])
        XCTAssertEqual(podcast.outline.first?.name, "Intro")
        XCTAssertEqual(podcast.durationS ?? 0, 312.4, accuracy: 0.001)
    }

    func testDecodesSourcesAndTheirStatus() throws {
        let source = try Fixtures.decode(NotebookSource.self, sourceJSON)
        XCTAssertEqual(source.kind, "pdf")
        XCTAssertEqual(source.pages, 22)
        XCTAssertEqual(source.chunks, 37)
        XCTAssertNil(source.durationS)
        XCTAssertTrue(source.hasFile)
        XCTAssertTrue(source.isReady)
        XCTAssertFalse(source.isPending)
        XCTAssertEqual(source.sourceContext, .summary)
        XCTAssertEqual(source.kindIcon, "doc.richtext")

        for (status, pending, failed) in [("queued", true, false), ("extracting", true, false), ("embedding", true, false),
                                          ("ready", false, false), ("failed", false, true)] {
            let s = try Fixtures.decode(NotebookSource.self, #"{"id": "s1", "status": "\#(status)", "error": "boom"}"#)
            XCTAssertEqual(s.isPending, pending, status)
            XCTAssertEqual(s.isFailed, failed, status)
        }
        // A listing with no context defaults to the full text; `web` and `url` share an icon.
        let web = try Fixtures.decode(NotebookSource.self, #"{"id": "s2", "kind": "web", "title": "Page"}"#)
        XCTAssertEqual(web.sourceContext, .full)
        XCTAssertEqual(web.kindIcon, NotebookSource(id: "s3", kind: "url").kindIcon)
    }

    func testDecodesChunksTransformationsInsightsAndProfiles() throws {
        let chunk = try Fixtures.decode(SourceChunk.self, #"{"id": "s1-14", "ordinal": 14, "text": "Hello", "page": 7}"#)
        XCTAssertEqual(chunk.position, "p. 7")
        let timed = try Fixtures.decode(SourceChunk.self, #"{"id": "s1-3", "ordinal": 3, "text": "Hi", "start_s": 65.0}"#)
        XCTAssertEqual(timed.position, "1:05")
        let plain = try Fixtures.decode(SourceChunk.self, #"{"id": "s1-0", "ordinal": 0, "text": "Hi"}"#)
        XCTAssertEqual(plain.position, "")

        let transformation = try Fixtures.decode(Transformation.self,
            #"{"id": "t-dense-summary", "name": "Dense Summary", "description": "d", "prompt": "p", "apply_default": true, "builtin": true}"#)
        XCTAssertTrue(transformation.applyDefault)
        XCTAssertTrue(transformation.builtin)

        let insight = try Fixtures.decode(SourceInsight.self,
            #"{"id": "i1", "source_id": "s1", "transformation_id": "t-dense-summary", "title": "Dense Summary", "status": "running", "text": "", "created_at": ""}"#)
        XCTAssertTrue(insight.isPending)
        XCTAssertEqual(insight.transformationId, "t-dense-summary")

        let profiles = try Fixtures.decode(PodcastProfiles.self, """
        {"episode_profiles": [{"id": "deep-dive", "name": "Deep dive", "description": "d", "speaker_profile": "duo-en", "briefing": "b", "segments": 4}],
         "speaker_profiles": [{"id": "duo-en", "name": "Lucina and Marco (English)", "language": "English",
                               "speakers": [{"name": "Lucina", "voice": "af_heart", "backstory": "", "personality": ""}, {"name": "Marco", "voice": "am_michael"}]}]}
        """)
        XCTAssertEqual(profiles.episodes.first?.speakerProfile, "duo-en")
        XCTAssertEqual(profiles.speakers.first?.speakers, ["Lucina", "Marco"])
    }

    func testDecodesMessagesWithCitations() throws {
        let message = try Fixtures.decode(NotebookMessage.self, messageJSON)
        XCTAssertFalse(message.isUser)
        XCTAssertEqual(message.model, "deepseek-flash")
        XCTAssertEqual(message.citations.count, 2)
        let source = message.citations[0]
        XCTAssertEqual(source.ref, "source:s0a9be3c112-14")
        XCTAssertEqual(source.kind, "source")
        XCTAssertEqual(source.sourceId, "s0a9be3c112")
        XCTAssertEqual(source.chunk, 14)
        XCTAssertEqual(source.page, 7)
        XCTAssertNil(source.startS)
        XCTAssertEqual(source.position, "p. 7")
        let note = message.citations[1]
        XCTAssertEqual(note.kind, "note")
        XCTAssertNil(note.sourceId)

        // The kind is derived from the ref when the server leaves it out.
        let bare = try Fixtures.decode(NotebookCitation.self, #"{"ref": "insight:i1"}"#)
        XCTAssertEqual(bare.kind, "insight")
    }

    func testChatsDecodeWithACountOrTheMessages() throws {
        let summary = try Fixtures.decode(NotebookChat.self, #"{"id": "c1", "notebook_id": "nb1", "title": "T", "messages": 4}"#)
        XCTAssertEqual(summary.messageCount, 4)
        let detail = try Fixtures.decode(NotebookChatDetail.self, """
        {"id": "c1", "notebook_id": "nb1", "title": "T", "created_at": "", "updated_at": "", "messages": [\(messageJSON), {"nope": 1}]}
        """)
        XCTAssertEqual(detail.chat.messageCount, 2, "the count is the length of the array the server sent")
        XCTAssertEqual(detail.messages.map(\.id), ["m77aa00bb11"])
        XCTAssertEqual(NotebookChat(id: "c2").displayTitle, "New chat")
    }

    func testDecodesAnExchange() throws {
        let exchange = try Fixtures.decode(NotebookExchange.self, """
        {"user": {"id": "m1", "role": "user", "text": "What is reading speed?", "citations": []}, "reply": \(messageJSON)}
        """)
        XCTAssertTrue(exchange.user.isUser)
        XCTAssertEqual(exchange.reply.citations.count, 2)
    }

    // MARK: Citation parsing

    func testNumbersCitationsInOrderOfFirstUse() {
        let text = "A [source:s1-4]. B [note:my-note]. Again A [source:s1-4] and C [insight:i9]."
        let parsed = NotebookCitations.parse(text, citations: [
            NotebookCitation(ref: "insight:i9", title: "Insight"),
            NotebookCitation(ref: "source:s1-4", title: "Book", excerpt: "x", page: 2),
            NotebookCitation(ref: "note:my-note", title: "Note"),
        ])
        XCTAssertEqual(parsed.sources.map(\.ref), ["source:s1-4", "note:my-note", "insight:i9"])
        XCTAssertEqual(parsed.sources[0].title, "Book", "the message's citation is used, not a rebuilt one")
        XCTAssertEqual(NotebookCitations.pieces(of: parsed.text), [
            .text("A "), .mark(1), .text(". B "), .mark(2), .text(". Again A "), .mark(1), .text(" and C "), .mark(3), .text("."),
        ])
    }

    func testGroupedAndAdjacentTokens() {
        let text = "Both agree [source:s1-1, source:s1-2][note:n]."
        let parsed = NotebookCitations.parse(text, citations: [])
        XCTAssertEqual(parsed.sources.map(\.ref), ["source:s1-1", "source:s1-2", "note:n"])
        XCTAssertEqual(NotebookCitations.pieces(of: parsed.text), [.text("Both agree "), .mark(1), .mark(2), .mark(3), .text(".")])
    }

    func testAnUnresolvedTokenStillGetsAChipFromItsId() {
        let parsed = NotebookCitations.parse("Claim [source:s0a9be3c112-14].", citations: [])
        guard let only = parsed.sources.first else { return XCTFail("no chip for an unresolved token") }
        XCTAssertEqual(only.kind, "source")
        XCTAssertEqual(only.sourceId, "s0a9be3c112")
        XCTAssertEqual(only.chunk, 14)
        XCTAssertEqual(only.title, "")
    }

    func testTextWithoutTokensIsUntouchedAndOtherBracketsStay() {
        let plain = "Use [1] as in the book, and [see below] too; a [link](https://example.com)."
        let parsed = NotebookCitations.parse(plain, citations: [])
        XCTAssertEqual(parsed.text, plain)
        XCTAssertTrue(parsed.sources.isEmpty)
        XCTAssertEqual(NotebookCitations.pieces(of: parsed.text), [.text(plain)])
        // A token cut off mid-way is not a citation.
        XCTAssertTrue(NotebookCitations.parse("Cut [source:s1-", citations: []).sources.isEmpty)
        XCTAssertTrue(NotebookCitations.parse("Empty [source:]", citations: []).sources.isEmpty)
    }

    func testSlugsWithUnderscoresAndDashesAndMultiline() {
        let text = "- First [note:rm_class_2_summary]\n- Second [source:s0a9be3c112-14]\n\n**Bold** [note:rm_class_2_summary]"
        let parsed = NotebookCitations.parse(text, citations: [])
        XCTAssertEqual(parsed.sources.map(\.ref), ["note:rm_class_2_summary", "source:s0a9be3c112-14"])
        XCTAssertFalse(parsed.text.contains("rm_class"), "the slug never reaches the markdown parser")
        XCTAssertTrue(parsed.text.contains("**Bold**"))
    }

    func testRefsSplitIntoSourceAndChunk() {
        XCTAssertEqual(NotebookCitations.sourceId(of: "source:s0a9be3c112-14"), "s0a9be3c112")
        XCTAssertEqual(NotebookCitations.chunk(of: "source:s0a9be3c112-14"), 14)
        XCTAssertNil(NotebookCitations.sourceId(of: "note:slug-with-dash"))
        XCTAssertNil(NotebookCitations.chunk(of: "note:slug-with-dash"))
    }

    func testStrippedRemovesTheTokens() {
        XCTAssertEqual(NotebookCitations.stripped("Reading is slow [source:s1-1]. Done [note:n]."), "Reading is slow. Done.")
    }

    func testTimesAndPositions() {
        XCTAssertEqual(NotebookTime.clock(312.4), "5:12")
        XCTAssertEqual(NotebookTime.clock(3725), "1:02:05")
        XCTAssertEqual(NotebookTime.clock(-3), "0:00")
        XCTAssertEqual(NotebookTime.position(page: 3, startS: 10), "p. 3")
        XCTAssertEqual(NotebookTime.position(page: nil, startS: 10), "0:10")
        XCTAssertEqual(NotebookTime.position(page: nil, startS: nil), "")
    }

    // MARK: Requests

    func testListsNotebooksLeniently() async throws {
        StubURLProtocol.handler = { [notebookJSON] _ in .init(body: #"{"notebooks": [\#(notebookJSON), {"x": 1}, {"id": "nb2", "name": "B"}]}"#) }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        let list = try await api.list(.only)
        XCTAssertEqual(list.map(\.id), ["nb4f1c09a2e7", "nb2"])
        let request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.url?.path, "/v1/notebooks")
        XCTAssertEqual(URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems, [URLQueryItem(name: "archived", value: "only")])
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer sec_test")
    }

    func testCreateAndUpdateBodiesAreSnakeCaseJSON() async throws {
        StubURLProtocol.handler = { [notebookJSON] _ in .init(status: 201, body: notebookJSON) }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        _ = try await api.create(name: "CNS", description: "")
        var request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
        XCTAssertEqual(try body(request), ["name": "CNS"])

        _ = try await api.update("nb1", archived: true)
        request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "PATCH")
        XCTAssertEqual(request.url?.path, "/v1/notebooks/nb1")
        XCTAssertEqual(try body(request), ["archived": true])

        try await api.delete("nb1", deleteSources: true)
        request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "DELETE")
        XCTAssertEqual(request.url?.query, "sources=delete-exclusive")
    }

    func testAddSourceBodiesFollowTheContract() async throws {
        StubURLProtocol.handler = { [sourceJSON] _ in .init(status: 202, body: sourceJSON) }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        _ = try await api.addSource("nb1", .url("https://youtu.be/abc"))
        XCTAssertEqual(try body(StubURLProtocol.requests.last), ["kind": "url", "url": "https://youtu.be/abc"])
        _ = try await api.addSource("nb1", .text(title: "", text: "Hello"))
        XCTAssertEqual(try body(StubURLProtocol.requests.last), ["kind": "text", "text": "Hello"])
        _ = try await api.addSource("nb1", .text(title: "Memo", text: "Hello"), transformations: ["t-dense-summary"])
        XCTAssertEqual(try body(StubURLProtocol.requests.last), ["kind": "text", "title": "Memo", "text": "Hello", "transformations": ["t-dense-summary"]])
        _ = try await api.addSource("nb1", .sandbox(path: "/home/cua/Documents/a.pdf"))
        XCTAssertEqual(try body(StubURLProtocol.requests.last), ["kind": "sandbox", "path": "/home/cua/Documents/a.pdf"])
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.path, "/v1/notebooks/nb1/sources")
    }

    func testUploadSendsMultipartWithTheFilePart() async throws {
        StubURLProtocol.handler = { [sourceJSON] _ in .init(status: 202, body: sourceJSON) }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        _ = try await api.upload("nb1", data: Data("%PDF-1.7 hello".utf8), filename: "RM Class 2.pdf", title: nil)
        let request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.url?.path, "/v1/notebooks/nb1/sources/upload")
        let type = try XCTUnwrap(request.value(forHTTPHeaderField: "Content-Type"))
        XCTAssertTrue(type.hasPrefix("multipart/form-data; boundary="), "not overwritten with application/json: \(type)")
        let payload = String(decoding: request.httpBody ?? Data(), as: UTF8.self)
        XCTAssertTrue(payload.contains(#"Content-Disposition: form-data; name="file"; filename="RM Class 2.pdf""#))
        XCTAssertTrue(payload.contains("%PDF-1.7 hello"))
        XCTAssertFalse(payload.contains(#"name="title""#), "an empty title is not sent")
        let boundary = String(type.dropFirst("multipart/form-data; boundary=".count))
        XCTAssertTrue(payload.hasSuffix("--\(boundary)--\r\n"))
    }

    func testMultipartEscapesQuotesInNames() {
        let multipart = Multipart(parts: [.file(name: "file", filename: "a\"b\r\nc.pdf", data: Data([1, 2]))], boundary: "B")
        let text = String(decoding: multipart.data, as: UTF8.self)
        XCTAssertTrue(text.contains(#"filename="a%22b  c.pdf""#))
        XCTAssertEqual(multipart.contentType, "multipart/form-data; boundary=B")
    }

    func testChatCallsUseTheContractShapes() async throws {
        let exchange = #"{"user": {"id": "m1", "role": "user", "text": "Q"}, "reply": \#(messageJSON)}"#
        StubURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/note") == true {
                return .init(status: 201, body: #"{"slug": "exam-prep", "title": "Exam prep", "excerpt": "", "updated_at": "", "indexed": false}"#)
            }
            return .init(body: exchange)
        }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        let result = try await api.ask("c1", text: "What is reading speed?", model: .qwen)
        var request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.url?.path, "/v1/notebooks/chats/c1/messages")
        XCTAssertEqual(try body(request), ["text": "What is reading speed?", "model": "qwen"])
        XCTAssertEqual(result.reply.citations.first?.page, 7)

        let note = try await api.saveAnswer(chat: "c1", message: "m77aa00bb11")
        request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.url?.path, "/v1/notebooks/chats/c1/messages/m77aa00bb11/note")
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(note.slug, "exam-prep")
    }

    func testPodcastCreateAndNoteBodies() async throws {
        StubURLProtocol.handler = { _ in .init(status: 202, body: #"{"id": "p1", "status": "queued"}"#) }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        let podcast = try await api.createPodcast("nb1", episodeProfile: "deep-dive", speakerProfile: "duo-en", briefing: "Focus on week 2", minutes: 8)
        XCTAssertEqual(podcast.status, "queued")
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.path, "/v1/notebooks/nb1/podcasts")
        XCTAssertEqual(try body(StubURLProtocol.requests.last),
                       ["episode_profile": "deep-dive", "speaker_profile": "duo-en", "briefing": "Focus on week 2", "minutes": 8])

        StubURLProtocol.handler = { _ in .init(status: 201, body: #"{"slug": "my-note", "title": "T"}"#) }
        _ = try await api.writeNote("nb1", title: "T", text: "Body")
        XCTAssertEqual(try body(StubURLProtocol.requests.last), ["title": "T", "text": "Body"])
        _ = try await api.linkNote("nb1", slug: "my-note")
        XCTAssertEqual(try body(StubURLProtocol.requests.last), ["slug": "my-note"])
    }

    func testAudioIsFetchedWithTheBearerToken() async throws {
        StubURLProtocol.handler = { _ in .init(status: 200, body: "ID3-bytes", headers: ["Content-Type": "audio/mpeg"]) }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        let data = try await api.podcastAudio("p1")
        XCTAssertEqual(String(decoding: data, as: UTF8.self), "ID3-bytes")
        let request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.url?.path, "/v1/notebooks/podcasts/p1/audio")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer sec_test")
    }

    func testIdsAreEscapedInPaths() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"id": "x"}"#) }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        _ = try? await api.unlinkNote("nb1", slug: "a/b c")
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.absoluteString, "https://example.test/v1/notebooks/nb1/notes/a%2Fb%20c")
    }

    // MARK: Errors

    func testServerErrorsUseTheEnvelopeAndUnavailableIsRecognised() async throws {
        StubURLProtocol.handler = { _ in .init(status: 404, body: #"{"error": {"code": "not_found", "message": "No such notebook."}}"#) }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        do {
            _ = try await api.detail("nbmissing")
            XCTFail("expected a 404")
        } catch {
            XCTAssertEqual((error as? APIError)?.status, 404)
            XCTAssertEqual(NotebooksError.message(error), "No such notebook.")
            XCTAssertFalse(NotebooksError.isUnavailable(error), "a missing notebook is not a missing feature")
        }

        // p1 without the feature answers a bare 404 (or 503 while the service is down).
        StubURLProtocol.handler = { _ in .init(status: 404, body: "") }
        do {
            _ = try await api.list()
            XCTFail("expected a 404")
        } catch {
            XCTAssertTrue(NotebooksError.isUnavailable(error))
            XCTAssertEqual(NotebooksError.message(error), "Notebooks aren't available on the server yet.")
        }
        StubURLProtocol.handler = { _ in .init(status: 503, body: #"{"error": {"code": "unavailable", "message": "Lucina's notebooks are offline."}}"#) }
        do {
            _ = try await api.list()
            XCTFail("expected a 503")
        } catch {
            XCTAssertTrue(NotebooksError.isUnavailable(error))
            XCTAssertEqual(NotebooksError.message(error), "Lucina's notebooks are offline.")
        }
    }

    func testAnAnswerThatDoesNotMatchTheContractIsADecodingError() async {
        StubURLProtocol.handler = { _ in .init(body: #"{"unexpected": true}"#) }
        let api = NotebooksAPI(client: StubURLProtocol.makeClient())
        do {
            _ = try await api.ask("c1", text: "Q", model: .deepseek)
            XCTFail("expected a decoding error")
        } catch {
            XCTAssertEqual((error as? APIError)?.code, "decoding")
        }
    }

    // MARK: Helpers

    private func body(_ request: URLRequest?) throws -> NSDictionary {
        let data = try XCTUnwrap(request?.httpBody)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? NSDictionary)
    }
}
