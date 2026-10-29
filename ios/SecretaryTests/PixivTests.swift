import AuthenticationServices
import XCTest
@testable import Secretary

@MainActor
final class PixivTests: XCTestCase {
    override func setUp() {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
    }

    // MARK: Models

    func testConnectionDecodesEveryField() throws {
        let json = """
        {"state":"connected","user":{"id":123,"name":"Giuseppe","account":"handle"},"premium":true,"r18":false,"r18g":false,
         "store":"waiting_approval","removal":null,"error":null,"connectedAt":"2026-10-29T15:40:00Z"}
        """
        let c = try Fixtures.decode(PixivConnection.self, json)
        XCTAssertEqual(c.state, .connected)
        XCTAssertEqual(c.user, PixivUser(id: "123", name: "Giuseppe", account: "handle"))
        XCTAssertEqual(c.premium, true)
        XCTAssertEqual(c.r18, false)
        XCTAssertEqual(c.store, .waitingApproval)
        XCTAssertNil(c.removal)
        XCTAssertTrue(c.isWaitingForApproval)
        XCTAssertEqual(PixivText.status(c), "Connected as Giuseppe (@handle) \u{00B7} Premium: Yes")
    }

    func testMissingFieldsAndUnknownStateAreSafe() throws {
        let empty = try Fixtures.decode(PixivConnection.self, "{}")
        XCTAssertEqual(empty.state, .unavailable)
        XCTAssertNil(empty.user)
        XCTAssertFalse(empty.canDisconnect)

        let unknown = try Fixtures.decode(PixivConnection.self, #"{"state":"quantum","store":"???","removal":"later","error":"new_code","user":"nope","premium":"yes"}"#)
        XCTAssertEqual(unknown.state, .unavailable)
        XCTAssertNil(unknown.store)
        XCTAssertNil(unknown.removal)
        XCTAssertNil(unknown.user, "a user of the wrong type does not fail the whole answer")
        XCTAssertNil(unknown.premium)
        XCTAssertEqual(unknown.error, "new_code")
        XCTAssertTrue(PixivText.notes(unknown).isEmpty, "an error code this app doesn't know shows nothing")
    }

    func testSettingsDefaultsAndUnknownValues() throws {
        XCTAssertEqual(try Fixtures.decode(PixivSettings.self, "{}"), PixivSettings(defaultContent: .safe, defaultSort: .dateDesc))
        let s = try Fixtures.decode(PixivSettings.self, #"{"defaultContent":"r18","defaultSort":"popular"}"#)
        XCTAssertEqual(s, PixivSettings(defaultContent: .r18, defaultSort: .popular))
        let odd = try Fixtures.decode(PixivSettings.self, #"{"defaultContent":"nsfw","defaultSort":7}"#)
        XCTAssertEqual(odd, PixivSettings())
    }

    func testStatusKeepsTodaysFieldsOutAndFallsBack() throws {
        let status = try Fixtures.decode(PixivStatus.self, #"{"configured":true,"account":{"session":"ok"},"connection":{"state":"disconnected"},"settings":{"defaultSort":"popular"}}"#)
        XCTAssertEqual(status.connection.state, .disconnected)
        XCTAssertEqual(status.settings, PixivSettings(defaultContent: .safe, defaultSort: .popular))
        let bare = try Fixtures.decode(PixivStatus.self, #"{"configured":true}"#)
        XCTAssertEqual(bare.connection.state, .unavailable)
        XCTAssertEqual(bare.settings, PixivSettings())
    }

    // MARK: Texts

    func testStatusTexts() {
        XCTAssertEqual(PixivText.status(PixivConnection(state: .connected, user: PixivUser(name: "A", account: "a"), premium: false)), "Connected as A (@a) \u{00B7} Premium: No")
        XCTAssertEqual(PixivText.status(PixivConnection(state: .disconnected)), "Not connected. Without an account Pixiv shows all-ages works only.")
        XCTAssertEqual(PixivText.status(PixivConnection(state: .expired, error: "token_rejected")), "Pixiv no longer accepts the saved sign-in. Connect again.")
        XCTAssertEqual(PixivText.notes(PixivConnection(state: .expired, error: "token_rejected")), [], "same text as the status is not repeated")
        XCTAssertEqual(PixivText.status(PixivConnection(state: .unavailable)), "Can't reach the secrets service right now.")
        XCTAssertEqual(PixivText.status(PixivConnection(state: .unavailable, error: "broker_unconfigured")), "Not set up on the server yet.")
        XCTAssertEqual(PixivText.notes(PixivConnection(state: .connected, store: .waitingApproval)),
                       ["Saving the sign-in to your secrets: approve with Face ID."])
        XCTAssertEqual(PixivText.notes(PixivConnection(state: .connected, removal: .waitingApproval)),
                       ["Removing the saved sign-in: approve with Face ID."])
        XCTAssertEqual(PixivText.notes(PixivConnection(state: .disconnected, error: "approval_denied")),
                       ["You declined saving it; nothing was stored."])
        for code in ["approval_expired", "approval_unavailable", "save_failed", "remove_denied", "remove_failed"] {
            XCTAssertNotNil(PixivText.errorText(code), code)
        }
    }

    func testDisconnectIsOfferedWhenSomethingIsSaved() {
        XCTAssertTrue(PixivConnection(state: .connected).canDisconnect)
        XCTAssertTrue(PixivConnection(state: .expired).canDisconnect)
        XCTAssertTrue(PixivConnection(state: .disconnected, store: .saved).canDisconnect)
        XCTAssertFalse(PixivConnection(state: .disconnected).canDisconnect)
    }

    // MARK: Callback code

    func testCallbackCodeExtraction() {
        XCTAssertEqual(PixivCallback.code(from: URL(string: "pixiv://account/login?code=abc-DEF_1.2~3&via=login")!), "abc-DEF_1.2~3")
        XCTAssertNil(PixivCallback.code(from: URL(string: "pixiv://account/login?via=login")!))
        XCTAssertNil(PixivCallback.code(from: URL(string: "pixiv://account/login?code=")!))
        XCTAssertNil(PixivCallback.code(from: URL(string: "pixiv://account/login?code=a%20b")!), "a space is not allowed")
        XCTAssertNil(PixivCallback.code(from: URL(string: "pixiv://account/login?code=a%2Fb%3Dc")!))
        XCTAssertNil(PixivCallback.code(from: URL(string: "pixiv://account/login?code=\(String(repeating: "a", count: 513))")!))
        XCTAssertEqual(PixivCallback.code(from: URL(string: "pixiv://account/login?code=\(String(repeating: "a", count: 512))")!)?.count, 512)
    }

    func testOnlyPixivsLoginPageIsOpened() throws {
        let ok = try Fixtures.decode(PixivConnectStart.self, #"{"loginUrl":"https://app-api.pixiv.net/web/v1/login?code_challenge=x&client=pixiv-android","state":"s","expiresInSecs":600}"#)
        XCTAssertNotNil(ok.url)
        let evil = try Fixtures.decode(PixivConnectStart.self, #"{"loginUrl":"https://evil.example/web/v1/login","state":"s"}"#)
        XCTAssertNil(evil.url)
        let http = try Fixtures.decode(PixivConnectStart.self, #"{"loginUrl":"http://app-api.pixiv.net/web/v1/login","state":"s"}"#)
        XCTAssertNil(http.url)
    }

    // MARK: API

    func testStatusAndSettingsRequests() async throws {
        let api = PixivAPI(client: StubURLProtocol.makeClient())
        StubURLProtocol.handler = { _ in .init(body: #"{"ok":true,"data":{"connection":{"state":"connected"},"settings":{"defaultContent":"all","defaultSort":"popular"}}}"#) }
        let status = try await api.status()
        XCTAssertEqual(status.connection.state, .connected)
        XCTAssertEqual(status.settings.defaultContent, .all)
        XCTAssertEqual(StubURLProtocol.requests.last?.url?.path, "/v1/pixiv/status")

        StubURLProtocol.handler = { _ in .init(body: #"{"ok":true,"data":{"defaultContent":"r18","defaultSort":"date_d"}}"#) }
        let saved = try await api.saveSettings(PixivSettings(defaultContent: .r18, defaultSort: .dateDesc))
        XCTAssertEqual(saved.defaultContent, .r18)
        let request = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(request.httpMethod, "PUT")
        XCTAssertEqual(request.url?.path, "/v1/pixiv/settings")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: request.httpBody ?? Data()) as? [String: String])
        XCTAssertEqual(body, ["defaultContent": "r18", "defaultSort": "date_d"])
    }

    func testConnectErrorMessages() async {
        let cases: [(Int, String, String)] = [
            (502, #"{"ok":false,"error":{"code":"pixiv_login_failed","message":"x"}}"#, "Pixiv didn't accept the sign-in; try again."),
            (502, #"{"ok":false,"error":"pixiv_login_failed"}"#, "Pixiv didn't accept the sign-in; try again."),
            (400, #"{"ok":false,"error":{"code":"connect_expired","message":"x"}}"#, "The sign-in took too long; try again."),
            (400, #"{"ok":false,"error":"connect_expired"}"#, "The sign-in took too long; try again."),
            (409, #"{"ok":false,"error":{"code":"busy","message":"x"}}"#, "Approve or decline the pending request first."),
            (400, #"{"ok":false,"error":{"code":"bad_request","message":"x"}}"#, "Couldn't finish the Pixiv sign-in."),
        ]
        let api = PixivAPI(client: StubURLProtocol.makeClient())
        for (status, body, expected) in cases {
            StubURLProtocol.handler = { _ in .init(status: status, body: body) }
            do {
                _ = try await api.connectFinish(state: "s", code: "c")
                XCTFail("should fail: \(body)")
            } catch {
                XCTAssertEqual(PixivText.connectError(APIError.wrap(error)), expected, body)
            }
        }
    }

    // MARK: Store

    private struct FakeAuth: PixivWebAuthenticating {
        var result: Result<URL, Error>
        func authenticate(url: URL) async throws -> URL { try result.get() }
    }

    private func store(auth: Result<URL, Error>) -> PixivStore {
        PixivStore(api: { PixivAPI(client: StubURLProtocol.makeClient()) }, auth: FakeAuth(result: auth))
    }

    private static let startBody = #"{"ok":true,"data":{"loginUrl":"https://app-api.pixiv.net/web/v1/login?code_challenge=x","state":"st1","expiresInSecs":600}}"#

    func testConnectSendsTheCodeAtOnce() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/pixiv/connect/start"
                ? .init(body: Self.startBody)
                : .init(body: #"{"ok":true,"data":{"state":"connected","store":"waiting_approval"}}"#)
        }
        let s = store(auth: .success(URL(string: "pixiv://account/login?code=thecode&via=login")!))
        await s.connect()
        XCTAssertNil(s.actionError)
        XCTAssertEqual(s.connection?.state, .connected)
        XCTAssertTrue(s.isWaitingForApproval)
        let finish = try XCTUnwrap(StubURLProtocol.requests.last)
        XCTAssertEqual(finish.url?.path, "/v1/pixiv/connect/finish")
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: finish.httpBody ?? Data()) as? [String: String])
        XCTAssertEqual(body, ["state": "st1", "code": "thecode"])
    }

    func testCancellingTheSignInIsSilent() async {
        StubURLProtocol.handler = { _ in .init(body: Self.startBody) }
        let cancel = NSError(domain: ASWebAuthenticationSessionErrorDomain, code: ASWebAuthenticationSessionError.canceledLogin.rawValue)
        let s = store(auth: .failure(cancel))
        await s.connect()
        XCTAssertNil(s.actionError)
        XCTAssertEqual(StubURLProtocol.requests.count, 1, "no finish call")
    }

    func testABadCallbackOrARefusedCodeShowsAMessage() async {
        StubURLProtocol.handler = { _ in .init(body: Self.startBody) }
        let noCode = store(auth: .success(URL(string: "pixiv://account/login?via=login")!))
        await noCode.connect()
        XCTAssertEqual(noCode.actionError, "Couldn't finish the Pixiv sign-in.")

        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/pixiv/connect/start"
                ? .init(body: Self.startBody)
                : .init(status: 502, body: #"{"ok":false,"error":{"code":"pixiv_login_failed","message":"no"}}"#)
        }
        let refused = store(auth: .success(URL(string: "pixiv://account/login?code=abc")!))
        await refused.connect()
        XCTAssertEqual(refused.actionError, "Pixiv didn't accept the sign-in; try again.")
    }

    func testDefaultsAreRolledBackWhenSavingFails() async {
        let s = store(auth: .failure(CancellationError()))
        StubURLProtocol.handler = { _ in .init(status: 500, body: "{}") }
        await s.setDefaults(content: .all)
        XCTAssertEqual(s.settings.defaultContent, .safe)
        XCTAssertNotNil(s.settingsError)
        StubURLProtocol.handler = { _ in .init(body: #"{"ok":true,"data":{"defaultContent":"all","defaultSort":"date_d"}}"#) }
        await s.setDefaults(content: .all)
        XCTAssertEqual(s.settings.defaultContent, .all)
        XCTAssertNil(s.settingsError)
    }
}
