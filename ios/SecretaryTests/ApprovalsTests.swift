import LocalAuthentication
import UserNotifications
import XCTest
@testable import Secretary

/// A signer that needs no Secure Enclave: records what it was asked to sign.
final class FakeApprovalSigner: ApprovalSigning, @unchecked Sendable {
    var keyId: String? = "key_1"
    var error: Error?
    var prepareError: Error?
    private(set) var signed: [(message: String, reason: String)] = []

    func prepareKey() throws -> Data {
        if let prepareError { throw prepareError }
        return Data([4, 1, 2, 3])
    }
    func commitKey(keyId: String) throws { self.keyId = keyId }
    func removeKey() throws { keyId = nil }

    func sign(_ message: String, reason: String) async throws -> ApprovalProof {
        signed.append((message, reason))
        if let error { throw error }
        return ApprovalProof(keyId: keyId ?? "", signature: "c2ln")
    }
}

/// Records what would go to `UNUserNotificationCenter`, delivered removals included.
final class ApprovalFakeCenter: NotificationScheduling, @unchecked Sendable {
    var added: [UNNotificationRequest] = []
    var delivered: [String] = []

    func add(_ request: UNNotificationRequest) async throws { added.append(request) }
    func pendingIdentifiers() async -> [String] { [] }
    func removePending(_ identifiers: [String]) {}
    func authorizationStatus() async -> UNAuthorizationStatus { .authorized }
    func requestAuthorization() async throws -> Bool { true }
    func setBadge(_ count: Int) async {}
    func removeDelivered(_ identifiers: [String]) async { delivered.append(contentsOf: identifiers) }
}

@MainActor
final class FakeApprovalNotifier: ApprovalNotifying {
    private(set) var posted: [String] = []
    private(set) var previewsSeen: [Bool] = []
    private(set) var failures: [String] = []
    private(set) var removed: [String] = []

    func post(_ item: ApprovalItem, previews: Bool) async {
        posted.append(item.id)
        previewsSeen.append(previews)
    }

    func postFailure(for item: ApprovalItem) async { failures.append(item.id) }
    func remove(ids: [String]) async { removed.append(contentsOf: ids) }
}

private func itemJSON(id: String, sensitive: Bool, status: String? = nil, extra: String = "") -> String {
    """
    {"id":"\(id)","requester":{"kind":"lucina","name":"Lucina","detail":"ask_lucina from Claude Code"},
     "action":"\(sensitive ? "root.exec" : "agent.permission")","summary":"Install the unit","details":"systemctl daemon-reload",
     "risk":"high","sensitive":\(sensitive),"payloadHash":"sha256:\(String(repeating: "ab", count: 32))",
     "createdAt":"2026-10-28T10:00:00.123456789Z","expiresAt":"2026-10-28T10:05:00Z","escalated":false,"youAreMain":true\
     \(status.map { ",\"status\":\"\($0)\",\"decidedBy\":\"iPad\"" } ?? "")\(extra)}
    """
}

@MainActor
final class ApprovalsTests: XCTestCase {
    var signer: FakeApprovalSigner!
    var notifier: FakeApprovalNotifier!
    var defaults: UserDefaults!
    var store: ApprovalsStore!
    var transportErrors: [APIError] = []

    override func setUp() {
        super.setUp()
        StubURLProtocol.requests = []
        StubURLProtocol.handler = nil
        signer = FakeApprovalSigner()
        notifier = FakeApprovalNotifier()
        defaults = UserDefaults(suiteName: "approvals-tests-\(UUID().uuidString)")
        transportErrors = []
        let client = StubURLProtocol.makeClient()
        store = ApprovalsStore(api: { ApprovalsAPI(client: client) }, signer: signer, notifier: notifier, defaults: defaults)
        store.onTransportError = { [unowned self] in transportErrors.append($0) }
    }

    private func decodeBody(_ request: URLRequest) -> [String: Any] {
        (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data())) as? [String: Any] ?? [:]
    }

    private func decideRequests() -> [URLRequest] {
        StubURLProtocol.requests.filter { $0.url?.path.hasSuffix("/decide") == true }
    }

    // MARK: Decoding

    func testDecodesAnItemWithNanosecondDates() throws {
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_1", sensitive: true))
        XCTAssertEqual(item.id, "apr_1")
        XCTAssertEqual(item.requester, ApprovalRequester(kind: "lucina", name: "Lucina", detail: "ask_lucina from Claude Code"))
        XCTAssertEqual(item.risk, .high)
        XCTAssertTrue(item.sensitive)
        XCTAssertEqual(item.details, "systemctl daemon-reload")
        XCTAssertEqual(item.createdAt, ISO8601DateFormatter().date(from: "2026-10-28T10:00:00Z"))
        XCTAssertEqual(item.expiresAt, ISO8601DateFormatter().date(from: "2026-10-28T10:05:00Z"))
        XCTAssertTrue(item.youAreMain)
        XCTAssertNil(item.status)
    }

    func testDecodingIsLenient() throws {
        // Nothing but an id: unknown means sensitive (the safe side), medium risk, no dates.
        let bare = try Fixtures.decode(ApprovalItem.self, #"{"id":"apr_2"}"#)
        XCTAssertEqual(bare.risk, .medium)
        XCTAssertTrue(bare.sensitive)
        XCTAssertEqual(bare.summary, "")
        XCTAssertNil(bare.expiresAt)
        XCTAssertEqual(bare.requester.displayName, "Something")
        // Mistyped and unknown fields are ignored.
        let odd = try Fixtures.decode(ApprovalItem.self, #"{"id":"apr_3","risk":"critical","sensitive":"yes","expiresAt":1792663500,"extra":{"a":1},"summary":7}"#)
        XCTAssertEqual(odd.risk, .medium)
        XCTAssertEqual(odd.expiresAt, Date(timeIntervalSince1970: 1_792_663_500))
        XCTAssertEqual(odd.summary, "")
        // A decided one from GET /v1/approvals/{id}.
        let decided = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_4", sensitive: false, status: "approved"))
        XCTAssertEqual(decided.status, .approved)
        XCTAssertEqual(decided.decidedBy, "iPad")
    }

    func testListAcceptsAnArrayOrAnObjectAndSkipsBadElements() throws {
        let one = itemJSON(id: "apr_a", sensitive: false)
        let bare = try Fixtures.decode(ApprovalList.self, "[\(one), {\"nope\": true}, \(itemJSON(id: "apr_b", sensitive: true))]")
        XCTAssertEqual(bare.items.map(\.id), ["apr_a", "apr_b"])
        let wrapped = try Fixtures.decode(ApprovalList.self, "{\"approvals\": [\(one)], \"next\": null}")
        XCTAssertEqual(wrapped.items.map(\.id), ["apr_a"])
        let empty = try Fixtures.decode(ApprovalList.self, "{}")
        XCTAssertTrue(empty.items.isEmpty)
        let envelope = try Fixtures.decode(ApprovalEnvelope.self, "{\"approval\": \(one)}")
        XCTAssertEqual(envelope.item.id, "apr_a")
    }

    func testDecodesSettings() throws {
        let s = try Fixtures.decode(ApprovalSettings.self, """
        {"mainDeviceId":"dev_1","escalateAfterSecs":30,"thisDeviceId":"dev_1",
         "devices":[{"id":"dev_1","name":"Giuseppe - iPhone","isMain":true,"hasBiometricKey":true,"lastSeen":"2026-10-28T10:00:00Z"},
                    {"id":"dev_2","name":"Mac"}, {"name":"broken"}]}
        """)
        XCTAssertEqual(s.devices.map(\.id), ["dev_1", "dev_2"])
        XCTAssertTrue(s.thisIsMain)
        XCTAssertTrue(s.serverHasKeyForThisDevice)
        let none = try Fixtures.decode(ApprovalSettings.self, #"{"mainDeviceId":null,"thisDeviceId":"dev_2","devices":[{"id":"dev_2","name":"Mac"}]}"#)
        XCTAssertFalse(none.thisIsMain)
        XCTAssertFalse(none.serverHasKeyForThisDevice)
    }

    // MARK: The signed text

    func testCanonicalSigningString() {
        let hash = "sha256:" + String(repeating: "0f", count: 32)
        XCTAssertEqual(ApprovalProof.signingString(id: "apr_x", payloadHash: hash, decision: .approve),
                       "secretary-approval-v1\napr_x\n\(hash)\napprove")
        let item = ApprovalItem(id: "apr_x", summary: "s", payloadHash: hash)
        XCTAssertEqual(item.signingString(for: .deny), "secretary-approval-v1\napr_x\n\(hash)\ndeny")
        XCTAssertFalse(item.signingString(for: .approve).hasSuffix("\n"))
    }

    // MARK: Answering

    func testSensitiveApproveSignsAndSendsTheProof() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"approved"}"#) }
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_s", sensitive: true))
        let result = await store.answer(item, .approve)
        XCTAssertEqual(result, .approved)
        XCTAssertEqual(signer.signed.count, 1)
        XCTAssertEqual(signer.signed[0].message, "secretary-approval-v1\napr_s\n\(item.payloadHash)\napprove")
        XCTAssertEqual(signer.signed[0].reason, "Install the unit")
        let request = try XCTUnwrap(decideRequests().first)
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.path, "/v1/approvals/apr_s/decide")
        let body = decodeBody(request)
        XCTAssertEqual(body["decision"] as? String, "approve")
        XCTAssertEqual(body["payloadHash"] as? String, item.payloadHash)
        let proof = try XCTUnwrap(body["proof"] as? [String: String])
        XCTAssertEqual(proof, ["keyId": "key_1", "signature": "c2ln"])
    }

    func testNonSensitiveApproveDoesNotSign() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"approved"}"#) }
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_n", sensitive: false))
        let result = await store.answer(item, .approve)
        XCTAssertEqual(result, .approved)
        XCTAssertTrue(signer.signed.isEmpty)
        let body = decodeBody(try XCTUnwrap(decideRequests().first))
        XCTAssertEqual(body["decision"] as? String, "approve")
        XCTAssertNil(body["proof"])
    }

    func testDenyNeverSignsEvenWhenSensitive() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"denied"}"#) }
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_d", sensitive: true))
        let result = await store.answer(item, .deny)
        XCTAssertEqual(result, .denied)
        XCTAssertTrue(signer.signed.isEmpty)
        let body = decodeBody(try XCTUnwrap(decideRequests().first))
        XCTAssertEqual(body["decision"] as? String, "deny")
        XCTAssertNil(body["proof"])
    }

    func testSensitiveApproveWithoutAKeyNeedsSetupAndSendsNothing() async throws {
        signer.keyId = nil
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_k", sensitive: true))
        let result = await store.answer(item, .approve)
        XCTAssertEqual(result, .needsSetup)
        XCTAssertTrue(StubURLProtocol.requests.isEmpty)
    }

    func testCancellingFaceIDSendsNothingAndLeavesThePrompt() async throws {
        signer.error = ApprovalSignError.cancelled
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_c", sensitive: true))
        await store.apply([item])
        XCTAssertEqual(store.sheetItem?.id, "apr_c")
        await store.decide(.approve)
        XCTAssertNil(store.outcome)
        XCTAssertNil(store.working)
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertEqual(store.sheetItem?.id, "apr_c")
    }

    func testDecidingFromThePromptSettlesTheList() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"approved"}"#) }
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_p", sensitive: true))
        await store.apply([item])
        await store.decide(.approve)
        XCTAssertEqual(store.outcome, .approved)
        XCTAssertTrue(store.pending.isEmpty)
        XCTAssertEqual(notifier.removed, ["apr_p"])
        await store.dismissSheet()
        XCTAssertNil(store.sheetItem)
    }

    func testAlreadyDecidedExpiredAndGoneAreMapped() {
        func outcome(_ status: Int, _ code: String) -> ApprovalOutcome {
            ApprovalOutcome.from(APIError.server(status: status, code: code, message: ""))
        }
        XCTAssertEqual(outcome(409, "approval_decided"), .alreadyDecided(nil, by: nil))
        XCTAssertEqual(outcome(410, "approval_expired"), .expired)
        XCTAssertEqual(outcome(404, "approval_not_found"), .gone)
        XCTAssertEqual(outcome(403, "biometric_required"), .needsSetup)
        XCTAssertTrue(outcome(409, "payload_mismatch") != .alreadyDecided(nil, by: nil))
        if case .failed = outcome(403, "not_main_device") {} else { XCTFail("not_main_device should be a message") }
        if case .failed = outcome(403, "self_approval") {} else { XCTFail("self_approval should be a message") }
        if case .failed = outcome(500, "internal") {} else { XCTFail("a server error should be a message") }
        XCTAssertTrue(ApprovalOutcome.alreadyDecided(nil, by: nil).isFinal)
        XCTAssertTrue(ApprovalOutcome.expired.isFinal)
        XCTAssertFalse(ApprovalOutcome.needsSetup.isFinal)
        XCTAssertFalse(ApprovalOutcome.failed("x").isFinal)
    }

    func testAServerAnswerOf409And410ReachesThePrompt() async throws {
        StubURLProtocol.handler = { request in
            if request.url?.path.hasSuffix("/decide") == true {
                return .init(status: 409, body: #"{"error":{"code":"approval_decided","message":"already decided"}}"#)
            }
            return .init(body: itemJSON(id: "apr_9", sensitive: false, status: "denied"))
        }
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_9", sensitive: false))
        await store.apply([item])
        await store.decide(.approve)
        // The prompt learns who answered first.
        XCTAssertEqual(store.outcome, .alreadyDecided(.denied, by: "iPad"))
        XCTAssertTrue(store.pending.isEmpty)

        StubURLProtocol.handler = { _ in .init(status: 410, body: #"{"error":{"code":"approval_expired","message":"expired"}}"#) }
        let second = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_10", sensitive: false))
        await store.dismissSheet()
        await store.apply([second])
        await store.decide(.deny)
        XCTAssertEqual(store.outcome, .expired)
    }

    // MARK: Notifications

    func testNotificationActionRouting() {
        XCTAssertEqual(ApprovalNotificationAction(identifier: ApprovalNotifier.approveAction), .approve)
        XCTAssertEqual(ApprovalNotificationAction(identifier: ApprovalNotifier.denyAction), .deny)
        XCTAssertEqual(ApprovalNotificationAction(identifier: UNNotificationDefaultActionIdentifier), .open)
        XCTAssertNil(ApprovalNotificationAction(identifier: UNNotificationDismissActionIdentifier))
        XCTAssertNil(ApprovalNotificationAction(identifier: "SOMETHING_ELSE"))

        XCTAssertEqual(ApprovalNotificationAction.deny.route(sensitive: true), .decide(.deny))
        XCTAssertEqual(ApprovalNotificationAction.deny.route(sensitive: false), .decide(.deny))
        XCTAssertEqual(ApprovalNotificationAction.approve.route(sensitive: false), .decide(.approve))
        XCTAssertEqual(ApprovalNotificationAction.approve.route(sensitive: true), .openSheet)
        XCTAssertEqual(ApprovalNotificationAction.open.route(sensitive: false), .openSheet)
        XCTAssertEqual(ApprovalNotificationAction.open.route(sensitive: true), .openSheet)
    }

    func testNotificationContentAndCategories() throws {
        let plain = ApprovalItem(id: "apr_1", summary: "Read 12 notes", sensitive: false)
        let secret = ApprovalItem(id: "apr_2", summary: "Reveal the key", sensitive: true)
        let a = ApprovalNotifier.content(for: plain, previews: true)
        XCTAssertEqual(a.categoryIdentifier, "APPROVAL")
        XCTAssertEqual(a.body, "Read 12 notes")
        XCTAssertEqual(a.userInfo[ApprovalNotifier.idKey] as? String, "apr_1")
        XCTAssertEqual(a.userInfo[ApprovalNotifier.sensitiveKey] as? Bool, false)
        let b = ApprovalNotifier.content(for: secret, previews: false)
        XCTAssertEqual(b.categoryIdentifier, "APPROVAL_SENSITIVE")
        XCTAssertFalse(b.body.contains("Reveal the key"))
        XCTAssertEqual(ApprovalNotifier.identifier("apr_1"), "approval.apr_1")
    }

    func testNotifierPostsOneRequestPerIdAndRemovesDelivered() async throws {
        let center = ApprovalFakeCenter()
        let notifier = ApprovalNotifier(center: center)
        await notifier.post(ApprovalItem(id: "apr_1", summary: "x"), previews: true)
        XCTAssertEqual(center.added.map(\.identifier), ["approval.apr_1"])
        XCTAssertNil(center.added[0].trigger)
        await notifier.remove(ids: ["apr_1"])
        XCTAssertEqual(center.delivered, ["approval.apr_1", "approval.failed.apr_1"])
    }

    func testStoreNotifiesOncePerIdAndRemovesWhenAnswered() async throws {
        let a = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_a", sensitive: false))
        let b = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_b", sensitive: true))
        await store.apply([a])
        await store.apply([a])
        await store.apply([b, a])
        XCTAssertEqual(notifier.posted, ["apr_a", "apr_b"])
        // Answered elsewhere: it leaves the list, the lock screen drops it, and it is not notified again.
        await store.apply([b])
        XCTAssertEqual(notifier.removed, ["apr_a"])
        await store.apply([b, a])
        XCTAssertEqual(notifier.posted, ["apr_a", "apr_b"])
        // A new store (the app restarted) remembers.
        let again = ApprovalsStore(api: { nil }, signer: signer, notifier: notifier, defaults: defaults)
        await again.apply([a, b])
        XCTAssertEqual(notifier.posted, ["apr_a", "apr_b"])
    }

    func testPreviewsOffAreHandedToTheNotifier() async throws {
        let store = ApprovalsStore(api: { nil }, signer: signer, notifier: notifier, previews: { false }, defaults: defaults)
        await store.apply([ApprovalItem(id: "apr_z", summary: "secret thing")])
        XCTAssertEqual(notifier.previewsSeen, [false])
    }

    func testButtonsFromTheNotification() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path.hasSuffix("/decide") == true ? .init(body: #"{"status":"ok"}"#) : .init(body: "{}")
        }
        let plain = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_plain", sensitive: false))
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([plain, secret])

        // Approve a non-sensitive request straight from the notification: no sheet, no signing.
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: "apr_plain", sensitive: false)
        XCTAssertEqual(decideRequests().count, 1)
        XCTAssertEqual(decodeBody(decideRequests()[0])["decision"] as? String, "approve")
        XCTAssertTrue(signer.signed.isEmpty)
        XCTAssertEqual(store.pending.map(\.id), ["apr_secret"])
        XCTAssertTrue(notifier.removed.contains("apr_plain"))

        // Deny a sensitive one: sent without a proof.
        await store.handleNotification(actionIdentifier: ApprovalNotifier.denyAction, approvalId: "apr_secret", sensitive: true)
        XCTAssertEqual(decideRequests().count, 2)
        XCTAssertEqual(decodeBody(decideRequests()[1])["decision"] as? String, "deny")
        XCTAssertNil(decodeBody(decideRequests()[1])["proof"])
        XCTAssertTrue(signer.signed.isEmpty)
    }

    func testSensitiveApproveButtonOpensThePromptInsteadOfSigning() async throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        await store.dismissSheet()
        XCTAssertNil(store.sheetItem)
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: "apr_secret", sensitive: true)
        XCTAssertEqual(store.sheetItem?.id, "apr_secret")
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertTrue(signer.signed.isEmpty)
        // A request that turned sensitive after the notification was made is not approved blindly either.
        let turned = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_turned", sensitive: true))
        await store.apply([secret, turned])
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: "apr_turned", sensitive: false)
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertEqual(store.sheetItem?.id, "apr_turned")
    }

    func testAFailedButtonAnswerTellsTheOwner() async throws {
        StubURLProtocol.handler = { _ in .init(status: 500, body: #"{"error":{"code":"internal","message":"boom"}}"#) }
        let plain = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_plain", sensitive: false))
        await store.apply([plain])
        await store.handleNotification(actionIdentifier: ApprovalNotifier.denyAction, approvalId: "apr_plain", sensitive: false)
        XCTAssertEqual(notifier.failures, ["apr_plain"])
        XCTAssertEqual(store.pending.map(\.id), ["apr_plain"])
    }

    // MARK: Polling

    func testPollingTheOldServerIsQuiet() async throws {
        StubURLProtocol.handler = { _ in .init(status: 404, body: #"{"error":{"code":"not_found","message":"no route"}}"#) }
        await store.poll()
        XCTAssertEqual(store.availability, .unsupported)
        XCTAssertNil(store.sheetItem)
        XCTAssertTrue(store.pending.isEmpty)
        XCTAssertTrue(transportErrors.isEmpty)
        XCTAssertTrue(notifier.posted.isEmpty)
        XCTAssertEqual(store.interval, ApprovalsStore.backoffInterval)
        XCTAssertEqual(StubURLProtocol.requests.count, 1)
        XCTAssertEqual(StubURLProtocol.requests[0].url?.path, "/v1/approvals")
        XCTAssertEqual(StubURLProtocol.requests[0].url?.query, "status=pending")
    }

    func testPollingShowsThePromptAndNotifies() async throws {
        StubURLProtocol.handler = { _ in .init(body: "{\"approvals\":[\(itemJSON(id: "apr_1", sensitive: true))]}") }
        await store.poll()
        XCTAssertEqual(store.availability, .available)
        XCTAssertEqual(store.sheetItem?.id, "apr_1")
        XCTAssertEqual(notifier.posted, ["apr_1"])
        XCTAssertEqual(store.interval, ApprovalsStore.pollInterval)
        await store.poll()
        XCTAssertEqual(notifier.posted, ["apr_1"])
    }

    func testOfflineAndServerErrorsStayQuietButSignOutEndsTheSession() async throws {
        StubURLProtocol.handler = { _ in .init(error: URLError(.notConnectedToInternet)) }
        await store.poll()
        XCTAssertEqual(store.availability, .failing)
        XCTAssertTrue(transportErrors.isEmpty)
        StubURLProtocol.handler = { _ in .init(status: 401, body: #"{"error":{"code":"unauthorized","message":"no"}}"#) }
        await store.poll()
        XCTAssertEqual(transportErrors.count, 1)
    }

    func testAClosedPromptComesBackWhenTheAppBecomesActive() async throws {
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_1", sensitive: false))
        await store.apply([item])
        XCTAssertEqual(store.sheetItem?.id, "apr_1")
        await store.dismissSheet()
        XCTAssertNil(store.sheetItem)
        await store.apply([item])
        XCTAssertNil(store.sheetItem, "the same request does not pop up again while the app stays active")
        StubURLProtocol.handler = { _ in .init(body: "[\(itemJSON(id: "apr_1", sensitive: false))]") }
        let task = Task { await store.foregroundLoop() }
        try await Task.sleep(for: .milliseconds(200))
        task.cancel()
        XCTAssertEqual(store.sheetItem?.id, "apr_1")
    }

    func testARequestAnsweredElsewhereUpdatesThePrompt() async throws {
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_1", sensitive: false))
        await store.apply([item])
        StubURLProtocol.handler = { _ in .init(body: itemJSON(id: "apr_1", sensitive: false, status: "approved")) }
        await store.apply([])
        XCTAssertEqual(store.outcome, .alreadyDecided(.approved, by: "iPad"))
        XCTAssertEqual(notifier.removed, ["apr_1"])
    }

    // MARK: API shapes

    func testSettingsAndKeyRequests() async throws {
        StubURLProtocol.handler = { request in
            switch request.url?.path {
            case "/v1/approvals/keys" where request.httpMethod == "POST": return .init(status: 201, body: #"{"keyId":"k1b2c3d4"}"#)
            default: return .init(body: "{}")
            }
        }
        let api = ApprovalsAPI(client: StubURLProtocol.makeClient())
        try await api.setMainDevice("dev_1")
        try await api.setMainDevice(nil)
        let keyId = try await api.registerKey(publicKey: Data([4, 9, 9]), label: "Face ID on iPhone", password: "pw")
        try await api.deleteKey()
        XCTAssertEqual(keyId, "k1b2c3d4")
        let r = StubURLProtocol.requests
        XCTAssertEqual(r.map { "\($0.httpMethod ?? "") \($0.url?.path ?? "")" },
                       ["PUT /v1/approvals/settings", "PUT /v1/approvals/settings", "POST /v1/approvals/keys", "DELETE /v1/approvals/keys"])
        XCTAssertEqual(String(data: r[0].httpBody ?? Data(), encoding: .utf8), #"{"mainDeviceId":"dev_1"}"#)
        XCTAssertEqual(String(data: r[1].httpBody ?? Data(), encoding: .utf8), #"{"mainDeviceId":null}"#)
        let body = decodeBody(r[2])
        XCTAssertEqual(body["publicKey"] as? String, Data([4, 9, 9]).base64EncodedString())
        XCTAssertEqual(body["label"] as? String, "Face ID on iPhone")
        XCTAssertEqual(body["password"] as? String, "pw")
        XCTAssertTrue(r.allSatisfy { $0.value(forHTTPHeaderField: "Authorization")?.hasPrefix("Bearer ") ?? false })
    }

    func testSettingUpFaceIDRegistersAndStoresTheKey() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { request in
            switch (request.httpMethod, request.url?.path) {
            case ("POST", "/v1/approvals/keys"): return .init(status: 201, body: #"{"keyId":"key_new"}"#)
            case ("GET", "/v1/approvals/settings"):
                return .init(body: #"{"mainDeviceId":null,"thisDeviceId":"dev_1","devices":[{"id":"dev_1","name":"iPhone","hasBiometricKey":true}]}"#)
            default: return .init(body: "{}")
            }
        }
        await store.loadSettings()
        XCTAssertEqual(store.keyStatus, .lostLocally)
        await store.setUpFaceID(password: "owner-password")
        XCTAssertEqual(signer.keyId, "key_new")
        XCTAssertEqual(store.keyStatus, .ready)
        XCTAssertNil(store.settingsMessage)
    }

    func testSettingUpOnAServerWithoutApprovalsSaysSo() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { _ in .init(status: 404, body: #"{"error":{"code":"not_found","message":"no route"}}"#) }
        await store.setUpFaceID(password: "pw")
        XCTAssertEqual(store.settingsMessage, "Approvals aren't on the server yet. Try again later.")
        XCTAssertNil(signer.keyId)
        XCTAssertEqual(store.availability, .unsupported)
    }

    func testDeviceSecurityFailuresGetClearMessages() async throws {
        for code in [LAError.Code.biometryNotEnrolled, .biometryLockout, .passcodeNotSet, .biometryNotAvailable] {
            XCTAssertEqual(ApprovalSignError.from(LAError(code)), .needsDeviceSecurity, "\(code)")
        }
        XCTAssertEqual(ApprovalSignError.from(LAError(.userCancel)), .cancelled)
        XCTAssertEqual(ApprovalSignError.from(LAError(.systemCancel)), .cancelled)
        XCTAssertTrue(ApprovalSignError.needsDeviceSecurity.userMessage.contains("Face ID & Passcode"))
        // Setting up with no Face ID and no passcode: the message says what to do, nothing is stored.
        let failing = FakeApprovalSigner()
        failing.keyId = nil
        failing.prepareError = ApprovalSignError.needsDeviceSecurity
        let client = StubURLProtocol.makeClient()
        let store = ApprovalsStore(api: { ApprovalsAPI(client: client) }, signer: failing, notifier: notifier, defaults: defaults)
        await store.setUpFaceID(password: "pw")
        XCTAssertEqual(store.settingsMessage, ApprovalSignError.needsDeviceSecurity.userMessage)
        XCTAssertTrue(StubURLProtocol.requests.filter { $0.url?.path == "/v1/approvals/keys" }.isEmpty)
        // Signing refused for the same reason is a message on the prompt, not a silent dead end.
        let item = ApprovalItem(id: "apr_1", summary: "x", sensitive: true)
        failing.keyId = "k"
        failing.error = ApprovalSignError.needsDeviceSecurity
        let result = await store.answer(item, .approve)
        XCTAssertEqual(result, .failed(ApprovalSignError.needsDeviceSecurity.userMessage))
    }

    func testAWrongPasswordIsSaidPlainlyAndStoresNothing() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { request in
            if request.httpMethod == "POST" { return .init(status: 403, body: #"{"error":{"code":"invalid_password","message":"no"}}"#) }
            return .init(body: #"{"thisDeviceId":"dev_1","devices":[{"id":"dev_1","name":"iPhone"}]}"#)
        }
        await store.setUpFaceID(password: "nope")
        XCTAssertNil(signer.keyId)
        XCTAssertEqual(store.settingsMessage, "That password isn't right.")
        XCTAssertEqual(store.keyStatus, .notSetUp)
    }
}
