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
    private(set) var preparedSigned: [String] = []
    var preparedError: Error?

    func prepareKey() throws -> Data {
        if let prepareError { throw prepareError }
        return Data([4, 1, 2, 3])
    }
    func signWithPreparedKey(_ message: String, reason: String) async throws -> String {
        preparedSigned.append(message)
        if let preparedError { throw preparedError }
        return "bmV3c2ln"
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
    /// Approval ids the server's APNs push already put on the lock screen (identifier `apns-<id>`).
    var pushed: Set<String> = []
    func deliveredApprovals(_ ids: Set<String>) async -> [String] { ids.intersection(pushed).map { "apns-\($0)" } }
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
    private(set) var followUps: [String] = []
    func postOpenToApprove(for item: ApprovalItem) async { followUps.append(item.id) }
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

/// The fixtures' payload hash, and a moment while they are still pending (they expire 2026-10-28T10:05:00Z).
private let fixtureHash = "sha256:" + String(repeating: "ab", count: 32)
private let fixtureNow = Date(timeIntervalSince1970: 1793181660) // 2026-10-28T10:01:00Z

private func settingsJSON(hasPin: Bool, serverKey: Bool = false) -> String {
    """
    {"mainDeviceId":null,"thisDeviceId":"dev_1","hasPin":\(hasPin),
     "devices":[{"id":"dev_1","name":"iPhone","hasBiometricKey":\(serverKey)}]}
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
        let result = await store.answer(item, .approve(.cardApproveButton))
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
        let result = await store.answer(item, .approve(.cardApproveButton))
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
        let result = await store.answer(item, .approve(.cardApproveButton))
        XCTAssertEqual(result, .needsSetup)
        XCTAssertTrue(StubURLProtocol.requests.isEmpty)
    }

    func testCancellingFaceIDSendsNothingAndLeavesThePrompt() async throws {
        signer.error = ApprovalSignError.cancelled
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_c", sensitive: true))
        await store.apply([item])
        XCTAssertEqual(store.sheetItem?.id, "apr_c")
        await store.decide(.approve(.cardApproveButton))
        XCTAssertNil(store.outcome)
        XCTAssertNil(store.working)
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertEqual(store.sheetItem?.id, "apr_c")
    }

    func testDecidingFromThePromptSettlesTheList() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"approved"}"#) }
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_p", sensitive: true))
        await store.apply([item])
        await store.decide(.approve(.cardApproveButton))
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
        await store.decide(.approve(.cardApproveButton))
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
        typealias N = ApprovalNotificationAction
        for sensitive in [true, false] {
            for inline in [true, false] {
                XCTAssertEqual(N.deny.route(sensitive: sensitive, inline: inline), .decide(.deny), "deny always decides")
                XCTAssertEqual(N.open.route(sensitive: sensitive, inline: inline), .openSheet, "a tap opens the card")
            }
        }
        XCTAssertEqual(N.approve.route(sensitive: false, inline: false), .decide(.approve), "plain approve")
        XCTAssertEqual(N.approve.route(sensitive: false, inline: true), .decide(.approve))
        XCTAssertEqual(N.approve.route(sensitive: true, inline: true), .openSheetAndApprove, "the whole plan was shown: Face ID at once")
        XCTAssertEqual(N.approve.route(sensitive: true, inline: false), .openSheet, "not the whole plan: the card to read")
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
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: "apr_plain", sensitive: false, trigger: .notificationApproveAction)
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

    func testSensitiveApproveButtonOpensTheCardInsteadOfSigning() async throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        await store.dismissSheet()
        XCTAssertNil(store.sheetItem)
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: "apr_secret", sensitive: true, trigger: .notificationApproveAction)
        XCTAssertEqual(store.sheetItem?.id, "apr_secret")
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertTrue(signer.signed.isEmpty, "the passcode is asked only when Approve is tapped on the card")
        // A request that turned sensitive after the notification was made is not approved blindly either.
        let turned = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_turned", sensitive: true))
        await store.apply([secret, turned])
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: "apr_turned", sensitive: false, trigger: .notificationApproveAction)
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertEqual(store.sheetItem?.id, "apr_turned")
        XCTAssertEqual(notifier.followUps, ["apr_turned"], "this action stays in the background: the follow-up leads to the card")
    }

    func testARequestAlreadyPushedIsNotPostedAgain() async throws {
        let center = ApprovalFakeCenter()
        center.pushed = ["apr_1"]
        let notifier = ApprovalNotifier(center: center)
        let pushed = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_1", sensitive: true))
        let fresh = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_2", sensitive: true))
        await notifier.post(pushed, previews: true)
        await notifier.post(fresh, previews: true)
        XCTAssertEqual(center.added.map(\.identifier), ["approval.apr_2"])
        XCTAssertEqual(center.added.first?.content.categoryIdentifier, ApprovalNotifier.sensitiveInlineCategory,
                       "its one command is in the body")
        // Answered: the APNs one goes too.
        await notifier.remove(ids: ["apr_1"])
        XCTAssertTrue(center.delivered.contains("apns-apr_1"))
        XCTAssertTrue(center.delivered.contains("approval.apr_1"))
    }

    func testAFailedButtonAnswerTellsTheOwner() async throws {
        StubURLProtocol.handler = { _ in .init(status: 500, body: #"{"error":{"code":"internal","message":"boom"}}"#) }
        let plain = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_plain", sensitive: false))
        await store.apply([plain])
        await store.handleNotification(actionIdentifier: ApprovalNotifier.denyAction, approvalId: "apr_plain", sensitive: false)
        XCTAssertEqual(notifier.failures, ["apr_plain"])
        XCTAssertEqual(store.pending.map(\.id), ["apr_plain"])
    }

    // MARK: Live Activity

    func testLiveActivityButtonsReportTheAnswer() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path.hasSuffix("/decide") == true ? .init(body: #"{"status":"ok"}"#) : .init(body: "{}")
        }
        var settled: [String: ApprovalOutcome] = [:]
        store.onSettled = { id, outcome in settled[id] = outcome }
        var learned: [[String]] = []
        store.onPendingLearned = { items in learned.append(items.map(\.id)) }
        let plain = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_plain", sensitive: false))
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([plain, secret])
        await store.apply([plain, secret])
        XCTAssertEqual(learned, [["apr_plain", "apr_secret"], ["apr_plain", "apr_secret"]], "every list, changed or not")
        // The activity's Deny and plain Approve answer through the notification's path, and end the activity.
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: "apr_plain", sensitive: false, trigger: .notificationApproveAction)
        await store.handleNotification(actionIdentifier: ApprovalNotifier.denyAction, approvalId: "apr_secret", sensitive: true)
        XCTAssertEqual(settled, ["apr_plain": .approved, "apr_secret": .denied])
    }

    func testTheActivitysSensitiveApproveStartsTheApproveStepOnTheCard() async throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        await store.open(id: "apr_secret", startApprove: .activityApproveButton)
        XCTAssertEqual(store.sheetItem?.id, "apr_secret")
        XCTAssertTrue(signer.signed.isEmpty, "nothing is signed until the card runs its approve action")
        XCTAssertEqual(store.takeStartApprove("apr_secret")?.trigger, .activityApproveButton)
        XCTAssertNil(store.takeStartApprove("apr_secret"), "once")
        // A plain open (a notification tap) does not start it.
        await store.open(id: "apr_secret")
        XCTAssertNil(store.takeStartApprove("apr_secret"))
    }

    func testTheActivitysApproveRoute() {
        XCTAssertEqual(ApprovalsStore.activityApproveRoute(sensitive: false), .decide)
        XCTAssertEqual(ApprovalsStore.activityApproveRoute(sensitive: true), .openCard, "never signed in the background")
    }

    func testTheActivitysSensitiveApproveAlwaysGoesToTheCard() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path.hasSuffix("/decide") == true ? .init(body: #"{"status":"ok"}"#) : .init(body: "{}")
        }
        var settled: [String: ApprovalOutcome] = [:]
        store.onSettled = { id, outcome in settled[id] = outcome }
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        let plain = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_plain", sensitive: false))
        await store.apply([secret, plain])

        // A root plan: nothing signed in the background, the card with its approve step (Face ID there).
        let card = await store.approveFromActivity(id: "apr_secret", shownHash: fixtureHash, trigger: .activityApproveButton, now: fixtureNow)
        XCTAssertEqual(card, .openCard(startApprove: true))
        XCTAssertTrue(signer.signed.isEmpty, "the signer is never called in the background")
        XCTAssertTrue(decideRequests().isEmpty)

        // A plain request is answered at once, without a signature.
        let plainCard = await store.approveFromActivity(id: "apr_plain", shownHash: fixtureHash, trigger: .activityApproveButton, now: fixtureNow)
        XCTAssertEqual(plainCard, .done)
        XCTAssertEqual(settled["apr_plain"], .approved)
        XCTAssertEqual(decideRequests().count, 1)
        XCTAssertTrue(signer.signed.isEmpty)
    }

    /// The push-to-start attributes exactly as the server sends them (crates/secretary-server/src/notify/live.rs,
    /// `start_body_is_exact`).
    func testLiveActivityAttributesDecodeTheServersStartPush() throws {
        let json = """
        {"approvalId": "apr_AbCdEfGhIjKlMnOpQrStUv", "requester": "Lucina", "summary": "Restart the tunnel on nezopt",
         "commands": "systemctl restart cloudflared\\njournalctl -u cloudflared -n 20\\nsystemctl status cloudflared",
         "commandCount": 4, "sensitive": true,
         "payloadHash": "sha256:abababababababababababababababababababababababababababababababab",
         "expiresAtUnix": 1761048300}
        """
        let a = try JSONDecoder().decode(ApprovalActivityAttributes.self, from: Data(json.utf8))
        XCTAssertEqual(a.approvalId, "apr_AbCdEfGhIjKlMnOpQrStUv")
        XCTAssertEqual(a.requester, "Lucina")
        XCTAssertEqual(a.commands.split(separator: "\n").count, 3)
        XCTAssertEqual(a.commandCount, 4)
        XCTAssertTrue(a.sensitive)
        XCTAssertEqual(a.expiresAt, Date(timeIntervalSince1970: 1_761_048_300))
        let state = try JSONDecoder().decode(ApprovalActivityAttributes.ContentState.self, from: Data(#"{"status": "pending"}"#.utf8))
        XCTAssertTrue(state.isPending)
        // The app builds the same commands from a request's details.
        let local = ApprovalActivityAttributes.commandLines("systemctl restart cloudflared\n\njournalctl -u cloudflared -n 20\nsystemctl status cloudflared\nuptime\n")
        XCTAssertEqual(local.commands, a.commands)
        XCTAssertEqual(local.count, 4)
        XCTAssertEqual(ApprovalActivityAttributes.commandLines("").count, 0)
    }

    func testActivitiesOfRequestsNoLongerPendingEnd() {
        XCTAssertEqual(ApprovalActivityAttributes.toEnd(activityIds: ["apr_a", "apr_b", "apr_c", "apr_b"], pending: ["apr_a"]),
                       ["apr_b", "apr_c"])
        XCTAssertEqual(ApprovalActivityAttributes.toEnd(activityIds: [], pending: ["apr_a"]), [])
        XCTAssertEqual(ApprovalActivityAttributes.toEnd(activityIds: ["apr_a"], pending: []), ["apr_a"])
    }

    // MARK: No approve without the owner's tap

    /// Nothing was decided, signed or set to start an approve step.
    private func assertNothingApproved(_ when: String, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(decideRequests().isEmpty, "no decide: \(when)", file: file, line: line)
        XCTAssertTrue(signer.signed.isEmpty, "no signature: \(when)", file: file, line: line)
        XCTAssertNil(store.startApprove, "no start-approve: \(when)", file: file, line: line)
    }

    func testANewRequestArrivingIsNeverApproved() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/approvals"
                ? .init(body: "[\(itemJSON(id: "apr_pushed", sensitive: true))]")
                : .init(body: "{}")
        }
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        let plain = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_plain", sensitive: false))
        var started: [String] = []
        // Live Activities allowed, and the clock before the fixtures' expiry (the device run had no starts: they were
        // past 2026-10-28T10:05).
        let activities = ApprovalActivities(defaults: defaults, requestActivity: { attributes, _ in started.append(attributes.approvalId) },
                                            liveActivities: { [] }, activitiesEnabled: { true }, now: { fixtureNow })

        // In front: the list, the prompt and the local Live Activity start.
        store.onPendingLearned = { activities.sync(pending: $0, foreground: true) }
        await store.apply([secret, plain])
        XCTAssertEqual(started, ["apr_secret", "apr_plain"], "the local activity starts")
        XCTAssertEqual(store.sheetItem?.id, "apr_secret", "the prompt shows")
        assertNothingApproved("apply in front, local activity start")

        // In the background (an approval push woke the app).
        store.onPendingLearned = { activities.sync(pending: $0, foreground: false) }
        await store.apply([secret, plain])
        assertNothingApproved("apply in the background")

        // The push wake (didReceiveRemoteNotification: markNotified, then poll) and the background refresh (poll).
        store.markNotified("apr_pushed")
        await store.poll()
        XCTAssertEqual(store.pending.map(\.id), ["apr_pushed"])
        await store.poll()
        assertNothingApproved("push wake and background refresh")
        XCTAssertEqual(started, ["apr_secret", "apr_plain"], "no activity started in the background")
    }

    func testOpeningACardWithoutTheActivityButtonStartsNothing() async throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        await store.dismissSheet()
        await store.open(id: "apr_secret")
        XCTAssertEqual(store.sheetItem?.id, "apr_secret")
        XCTAssertNil(store.takeStartApprove("apr_secret"))
        assertNothingApproved("a notification tap or the Details link")
    }

    func testAStartApproveExpiresAfterTenSeconds() async throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        let tap = Date(timeIntervalSince1970: 1_800_000_000)
        await store.open(id: "apr_secret", startApprove: .activityApproveButton, now: tap)
        XCTAssertNil(store.takeStartApprove("apr_secret", now: tap.addingTimeInterval(11)), "too late")
        XCTAssertNil(store.takeStartApprove("apr_secret", now: tap.addingTimeInterval(1)), "and gone")
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertTrue(signer.signed.isEmpty)
        // Within the window it is handed over once, with the activity's trigger.
        await store.open(id: "apr_secret", startApprove: .activityApproveButton, now: tap)
        XCTAssertEqual(store.takeStartApprove("apr_secret", now: tap.addingTimeInterval(9))?.trigger, .activityApproveButton)
    }

    func testAStaleStartApproveNeverReachesAnotherCard() async throws {
        let a = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_a", sensitive: true))
        let b = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_b", sensitive: true))
        await store.apply([a, b])
        let tap = Date()
        await store.open(id: "apr_a", startApprove: .activityApproveButton, now: tap)
        // B's card shows instead: it does not take A's start-approve, and A's is gone with it.
        XCTAssertNil(store.takeStartApprove("apr_b", now: tap))
        XCTAssertNil(store.takeStartApprove("apr_a", now: tap))
        assertNothingApproved("B's card after A's tap")

        // Cleared when the sheet closes, when the request leaves the list, and in the background.
        await store.open(id: "apr_a", startApprove: .activityApproveButton)
        await store.dismissSheet()
        XCTAssertNil(store.startApprove, "sheet closed")
        await store.open(id: "apr_a", startApprove: .activityApproveButton)
        await store.apply([b])
        XCTAssertNil(store.startApprove, "answered elsewhere")
        await store.apply([a, b])
        await store.open(id: "apr_a", startApprove: .activityApproveButton)
        store.sceneBackgrounded()
        XCTAssertNil(store.startApprove, "app in the background")
        await store.open(id: "apr_a", startApprove: .activityApproveButton)
        store.reset()
        XCTAssertNil(store.startApprove, "signed out")
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertTrue(signer.signed.isEmpty)
    }

    func testTheActivitysApproveLeavesARequestThatIsNoLongerPending() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/approvals/apr_done"
                ? .init(body: itemJSON(id: "apr_done", sensitive: true, status: "approved"))
                : .init(body: "{}")
        }
        let needsCard = await store.approveFromActivity(id: "apr_done", shownHash: fixtureHash, trigger: .activityApproveButton, now: fixtureNow)
        XCTAssertEqual(needsCard, .done)
        assertNothingApproved("already answered on another device")
        // A trigger from elsewhere is refused outright, even for a pending request.
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        let refused = await store.approveFromActivity(id: "apr_secret", shownHash: fixtureHash, trigger: .notificationApproveAction, now: fixtureNow)
        XCTAssertEqual(refused, .done)
        assertNothingApproved("a notification trigger on the activity path")
    }

    func testTheNotificationsApproveNeedsItsTrigger() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"ok"}"#) }
        let plain = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_plain", sensitive: false))
        await store.apply([plain])
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: "apr_plain", sensitive: false)
        XCTAssertTrue(decideRequests().isEmpty, "no trigger, no approve")
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: "apr_plain", sensitive: false,
                                       trigger: .cardApproveButton)
        XCTAssertTrue(decideRequests().isEmpty, "only the notification's own trigger")
    }

    // MARK: Review fixes

    func testTheActivitysApproveIsBoundToWhatItShowedAndToTheTime() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"ok"}"#) }
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        // The request changed since the activity showed it: the card, without its approve step.
        let changed = await store.approveFromActivity(id: "apr_secret", shownHash: "sha256:" + String(repeating: "cd", count: 32),
                                                      trigger: .activityApproveButton, now: fixtureNow)
        XCTAssertEqual(changed, .openCard(startApprove: false))
        // Its time is up.
        let late = await store.approveFromActivity(id: "apr_secret", shownHash: fixtureHash, trigger: .activityApproveButton,
                                                   now: fixtureNow.addingTimeInterval(600))
        XCTAssertEqual(late, .openCard(startApprove: false))
        assertNothingApproved("changed or expired")
    }

    func testASecondTapWhileTheFirstIsOnItsWayDoesNothing() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path.hasSuffix("/decide") == true ? .init(body: #"{"status":"ok"}"#) : .init(body: "{}")
        }
        let plain = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_plain", sensitive: false))
        await store.apply([plain])
        async let first = store.approveFromActivity(id: "apr_plain", shownHash: fixtureHash, trigger: .activityApproveButton, now: fixtureNow)
        async let second = store.approveFromActivity(id: "apr_plain", shownHash: fixtureHash, trigger: .activityApproveButton, now: fixtureNow)
        _ = await (first, second)
        XCTAssertEqual(decideRequests().count, 1, "one answer")
    }

    func testTheCardsAnswerIsBoundToTheRequestOnScreen() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"ok"}"#) }
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        await store.decide(.approve(.cardApproveButton), shownHash: "sha256:" + String(repeating: "cd", count: 32))
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertTrue(signer.signed.isEmpty)
        guard case .failed? = store.outcome else { return XCTFail("the owner is told the request changed") }
        await store.decide(.approve(.cardApproveButton), shownHash: fixtureHash)
        XCTAssertEqual(decideRequests().count, 1)
    }

    func testTheErrorPlaceholderCannotBeAnswered() async throws {
        StubURLProtocol.handler = { _ in .init(status: 500, body: #"{"error":{"code":"internal","message":"boom"}}"#) }
        await store.open(id: "apr_missing")
        XCTAssertEqual(store.sheetItem?.id, "apr_missing")
        XCTAssertEqual(store.sheetItem?.canAnswer, false)
        await store.decide(.approve(.cardApproveButton))
        await store.decide(.deny)
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertTrue(signer.signed.isEmpty)
    }

    func testEverySignatureAsksFaceIDNow() {
        // The Face ID unlock is never reused (the Secure Enclave key refuses such a context), and Face ID may show.
        let context = LAContext()
        context.touchIDAuthenticationAllowableReuseDuration = 300
        context.interactionNotAllowed = true
        ApprovalSigningContext.configure(context)
        XCTAssertEqual(context.touchIDAuthenticationAllowableReuseDuration, 0)
        XCTAssertFalse(context.interactionNotAllowed)
        // Anything but a cancel is "not signed", which the card says (never a silent failure).
        XCTAssertEqual(ApprovalSignError.from(LAError(.authenticationFailed)), .notSigned(domain: LAError.errorDomain, code: LAError.Code.authenticationFailed.rawValue))
        XCTAssertEqual(ApprovalSignError.from(keyError: NSError(domain: "CryptoTokenKit", code: -5)), .notSigned(domain: "CryptoTokenKit", code: -5))
        XCTAssertEqual(ApprovalSignError.from(keyError: NSError(domain: NSOSStatusErrorDomain, code: -128)), .cancelled)
        XCTAssertFalse(ApprovalSignError.notSigned(domain: "CryptoTokenKit", code: -5).userMessage.isEmpty)
    }

    func testAKeyThatRefusesToSignIsSaidOnTheCard() async throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        signer.error = ApprovalSignError.notSigned(domain: "CryptoTokenKit", code: -5)
        await store.decide(.approve(.cardApproveButton), shownHash: fixtureHash)
        XCTAssertEqual(store.outcome, .failed(ApprovalSignError.notSigned(domain: "CryptoTokenKit", code: -5).userMessage))
        XCTAssertEqual(store.sheetItem?.id, "apr_secret", "the card stays, Approve tries again")
        XCTAssertTrue(decideRequests().isEmpty)
    }

    func testOnlyTheNotificationsApproveActionIsATrigger() {
        XCTAssertEqual(AppDelegate.approvalTrigger(forAction: ApprovalNotifier.approveAction), .notificationApproveAction)
        XCTAssertNil(AppDelegate.approvalTrigger(forAction: ApprovalNotifier.denyAction))
        XCTAssertNil(AppDelegate.approvalTrigger(forAction: UNNotificationDefaultActionIdentifier), "a tap opens the card")
        XCTAssertNil(AppDelegate.approvalTrigger(forAction: UNNotificationDismissActionIdentifier))
    }

    func testHowTheCardOpensAfterTheActivitysApprove() {
        typealias A = ApprovalActivities
        XCTAssertEqual(A.cardOpening(startRequested: true, appActive: true, continued: false), .withApproveStep)
        XCTAssertEqual(A.cardOpening(startRequested: true, appActive: false, continued: true), .withApproveStep)
        XCTAssertEqual(A.cardOpening(startRequested: true, appActive: false, continued: false), .withoutApproveStep,
                       "declined: the card only, nothing in the background")
        XCTAssertEqual(A.cardOpening(startRequested: false, appActive: true, continued: false), .withoutApproveStep)
    }

    func testTheActivitysApproveIsNeverLeftWithNothingToSee() {
        typealias A = ApprovalActivities
        XCTAssertEqual(A.handOff(appActive: true, canContinue: false, continueSucceeded: false), .openInFront)
        XCTAssertEqual(A.handOff(appActive: false, canContinue: true, continueSucceeded: true), .openInFront, "brought to the front")
        XCTAssertEqual(A.handOff(appActive: false, canContinue: true, continueSucceeded: false), .openAndNotify,
                       "continuing failed: open-to-approve")
        XCTAssertEqual(A.handOff(appActive: false, canContinue: false, continueSucceeded: false), .openAndNotify,
                       "no way to the front: open-to-approve")
        XCTAssertEqual(A.handOff(appActive: false, canContinue: false, continueSucceeded: true), .openAndNotify)
        // Brought to the front: the approve step starts there; left behind: the card only.
        XCTAssertEqual(A.cardOpening(startRequested: true, appActive: false, continued: true), .withApproveStep)
        XCTAssertEqual(A.cardOpening(startRequested: true, appActive: false, continued: false), .withoutApproveStep)
    }

    func testOpenToApproveNamesTheRequest() async throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        await store.postOpenToApprove(id: "apr_secret")
        await store.postOpenToApprove(id: "apr_unknown")
        XCTAssertEqual(notifier.followUps, ["apr_secret", "apr_unknown"])
        assertNothingApproved("a follow-up only")
    }

    func testALocalActivityHonoursThePreviewsSetting() throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        let shown = ApprovalActivities.localAttributes(for: secret, previews: true, now: fixtureNow)
        XCTAssertEqual(shown.summary, "Install the unit")
        XCTAssertEqual(shown.commands, "systemctl daemon-reload")
        let hidden = ApprovalActivities.localAttributes(for: secret, previews: false, now: fixtureNow)
        XCTAssertEqual(hidden.summary, "Open Secretary to see what it is.")
        XCTAssertEqual(hidden.commands, "")
        XCTAssertEqual(hidden.commandCount, 0)
        XCTAssertEqual(hidden.payloadHash, fixtureHash)
    }

    // MARK: The root-plan notification's Approve in place

    func testARootPlanNotificationCarriesItsCommandsAndHash() {
        let short = ApprovalItem(id: "apr_s", summary: "Restart the tunnel", details: "systemctl daemon-reload\nsystemctl restart x",
                                 sensitive: true, payloadHash: fixtureHash)
        let c = ApprovalNotifier.content(for: short, previews: true)
        XCTAssertEqual(c.categoryIdentifier, ApprovalNotifier.sensitiveInlineCategory)
        XCTAssertEqual(c.body, "Restart the tunnel\nsystemctl daemon-reload\nsystemctl restart x")
        XCTAssertEqual(c.userInfo[ApprovalNotifier.payloadHashKey] as? String, fixtureHash)
        XCTAssertEqual(ApprovalNotifier.payloadHash(in: c.userInfo), fixtureHash)
        XCTAssertEqual(ApprovalNotifier.payloadHash(in: ["payloadHash": "sha256:00"]), "sha256:00", "the server push's key")

        let long = (0..<400).map { "echo line-\($0)" }.joined(separator: "\n")
        XCTAssertGreaterThan(long.utf8.count, ApprovalNotifier.detailBytes)
        let big = ApprovalNotifier.content(for: ApprovalItem(id: "apr_l", summary: "Many", details: long, sensitive: true), previews: true)
        XCTAssertEqual(big.categoryIdentifier, ApprovalNotifier.sensitiveCategory, "cut: Approve opens the card")
        XCTAssertTrue(big.body.hasSuffix("\n…"))
        XCTAssertLessThanOrEqual(big.body.utf8.count, "Many\n".utf8.count + ApprovalNotifier.detailBytes + "\n…".utf8.count)
        XCTAssertTrue(big.body.split(separator: "\n").dropFirst().allSatisfy { $0 == "…" || $0.hasPrefix("echo line-") }, "cut at a line")

        let hidden = ApprovalNotifier.content(for: short, previews: false)
        XCTAssertEqual(hidden.categoryIdentifier, ApprovalNotifier.sensitiveCategory)
        XCTAssertEqual(hidden.body, "Open Secretary to see what it is.")
        XCTAssertFalse(hidden.body.contains("systemctl"))
        XCTAssertEqual(hidden.userInfo[ApprovalNotifier.payloadHashKey] as? String, fixtureHash)

        let none = ApprovalNotifier.content(for: ApprovalItem(id: "apr_n", summary: "Reveal", sensitive: true), previews: true)
        XCTAssertEqual(none.categoryIdentifier, ApprovalNotifier.sensitiveCategory, "no commands to show")
        XCTAssertEqual(ApprovalNotifier.clipBytes("ab").cut, false)
        XCTAssertEqual(ApprovalNotifier.clipBytes(String(repeating: "è", count: 1201)).text.utf8.count <= ApprovalNotifier.detailBytes + 4, true)
    }

    /// The notification's Approve on an `APPROVAL_SENSITIVE_INLINE` (or, `inline: false`, `APPROVAL_SENSITIVE`) request.
    private func approveFromNotification(_ id: String, hash: String?, inline: Bool = true, now: Date = fixtureNow) async {
        await store.handleNotification(actionIdentifier: ApprovalNotifier.approveAction, approvalId: id, sensitive: true,
                                       inline: inline, shownHash: hash, trigger: .notificationApproveAction, now: now)
    }

    func testTheInlineApproveOpensTheCardWithFaceIDAtOnce() async throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        await store.dismissSheet()
        await approveFromNotification("apr_secret", hash: fixtureHash)
        XCTAssertEqual(store.sheetItem?.id, "apr_secret")
        XCTAssertEqual(store.startApprove?.trigger, .notificationApproveAction, "the approve step starts on the card")
        XCTAssertTrue(signer.signed.isEmpty, "Face ID is asked by the card, not here")
        XCTAssertTrue(decideRequests().isEmpty)
    }

    func testTheNotificationsApproveOpensTheCardWithoutTheStepOnAnyDoubt() async throws {
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        for (hash, inline, now, why) in [
            ("sha256:" + String(repeating: "cd", count: 32), true, fixtureNow, "a different plan than the one shown"),
            (nil, true, fixtureNow, "no hash"),
            (fixtureHash, true, fixtureNow.addingTimeInterval(600), "expired"),
            (fixtureHash, false, fixtureNow, "APPROVAL_SENSITIVE: not the whole plan"),
        ] as [(String?, Bool, Date, String)] {
            await store.dismissSheet()
            await approveFromNotification("apr_secret", hash: hash, inline: inline, now: now)
            XCTAssertEqual(store.sheetItem?.id, "apr_secret", why)
            XCTAssertNil(store.startApprove, why)
        }
        XCTAssertTrue(signer.signed.isEmpty)
        XCTAssertTrue(decideRequests().isEmpty)
        XCTAssertFalse(ApprovalsStore.cardApproveStep(trigger: nil, shownAndPending: true))
        XCTAssertFalse(ApprovalsStore.cardApproveStep(trigger: .activityApproveButton, shownAndPending: true))
        XCTAssertTrue(ApprovalsStore.cardApproveStep(trigger: .notificationApproveAction, shownAndPending: true))
        XCTAssertFalse(ApprovalsStore.cardApproveStep(trigger: .notificationApproveAction, shownAndPending: false))
    }

    func testDenyFromAnInlineNotificationIsUnchanged() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path.hasSuffix("/decide") == true ? .init(body: #"{"status":"ok"}"#) : .init(body: "{}")
        }
        let secret = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_secret", sensitive: true))
        await store.apply([secret])
        await store.handleNotification(actionIdentifier: ApprovalNotifier.denyAction, approvalId: "apr_secret", sensitive: true,
                                       inline: true, shownHash: nil)
        XCTAssertEqual(decodeBody(try XCTUnwrap(decideRequests().first))["decision"] as? String, "deny")
        XCTAssertNil(decodeBody(decideRequests()[0])["proof"])
        XCTAssertTrue(signer.signed.isEmpty)
        XCTAssertTrue(notifier.followUps.isEmpty)
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

    func testTheActiveAppLongPollsFromTheLastCursor() async throws {
        // p1 saw ~20 polls a minute from the iPhone (2026-10-29); now one long poll per change or per 25 s.
        StubURLProtocol.handler = { _ in .init(body: #"{"approvals":[],"cursor":"1792.4"}"#) }
        let task = Task { await store.foregroundLoop() }
        try await Task.sleep(for: .milliseconds(1600))
        task.cancel()
        let queries = StubURLProtocol.requests.map { $0.url?.query ?? "" }
        XCTAssertEqual(queries.first, "status=pending")
        XCTAssertEqual(queries.dropFirst().first, "status=pending&since=1792.4&wait=25")
        XCTAssertLessThanOrEqual(queries.count, 3)
        XCTAssertEqual(store.interval, ApprovalsStore.longPollPause)
        XCTAssertGreaterThan(StubURLProtocol.requests[1].timeoutInterval, 25)
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
        let keyId = try await api.registerKey(publicKey: "BAkJ", label: "Face ID on iPhone", signature: "c2ln")
        try await api.deleteKey()
        XCTAssertEqual(keyId, "k1b2c3d4")
        let r = StubURLProtocol.requests
        XCTAssertEqual(r.map { "\($0.httpMethod ?? "") \($0.url?.path ?? "")" },
                       ["PUT /v1/approvals/settings", "PUT /v1/approvals/settings", "POST /v1/approvals/keys", "DELETE /v1/approvals/keys"])
        XCTAssertEqual(String(data: r[0].httpBody ?? Data(), encoding: .utf8), #"{"mainDeviceId":"dev_1"}"#)
        XCTAssertEqual(String(data: r[1].httpBody ?? Data(), encoding: .utf8), #"{"mainDeviceId":null}"#)
        XCTAssertTrue(r.allSatisfy { $0.value(forHTTPHeaderField: "Authorization")?.hasPrefix("Bearer ") ?? false })
    }

    // MARK: v2: setup without a password

    func testRegistrationSigningStringAndBodyHaveNoPassword() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { request in
            switch (request.httpMethod, request.url?.path) {
            case ("POST", "/v1/approvals/keys"): return .init(status: 201, body: #"{"keyId":"key_new"}"#)
            case ("GET", "/v1/approvals/settings"): return .init(body: settingsJSON(hasPin: false, serverKey: true))
            default: return .init(body: "{}")
            }
        }
        XCTAssertEqual(ApprovalsAPI.registrationString(deviceId: "dev_1", publicKey: "BAECAw=="), "secretary-approval-key-v1\ndev_1\nBAECAw==")
        let ok = await store.setUpFaceID()
        XCTAssertTrue(ok)
        // The new key signs "secretary-approval-key-v1\n{thisDeviceId}\n{publicKeyB64}" with the exact string it sends.
        let sentKey = Data([4, 1, 2, 3]).base64EncodedString()
        XCTAssertEqual(signer.preparedSigned, ["secretary-approval-key-v1\ndev_1\n\(sentKey)"])
        XCTAssertEqual(signer.keyId, "key_new")
        XCTAssertEqual(store.keyStatus, .ready)
        XCTAssertNil(store.settingsMessage)
        let post = try XCTUnwrap(StubURLProtocol.requests.first { $0.httpMethod == "POST" && $0.url?.path == "/v1/approvals/keys" })
        let body = decodeBody(post)
        XCTAssertEqual(body["publicKey"] as? String, sentKey)
        XCTAssertEqual(body["label"] as? String, "Face ID on iPhone")
        XCTAssertEqual(body["proof"] as? [String: String], ["signature": "bmV3c2ln"])
        XCTAssertNil(body["password"])
        XCTAssertEqual(Set(body.keys), ["publicKey", "label", "proof"])
    }

    func testCancellingFaceIDDuringSetupRegistersNothing() async throws {
        signer.keyId = nil
        signer.preparedError = ApprovalSignError.cancelled
        StubURLProtocol.handler = { _ in .init(body: settingsJSON(hasPin: false)) }
        let ok = await store.setUpFaceID()
        XCTAssertFalse(ok)
        XCTAssertNil(signer.keyId)
        XCTAssertNil(store.settingsMessage)
        XCTAssertTrue(StubURLProtocol.requests.filter { $0.httpMethod == "POST" }.isEmpty)
    }

    func testSettingUpOnAServerWithoutApprovalsSaysSo() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { _ in .init(status: 404, body: #"{"error":{"code":"not_found","message":"no route"}}"#) }
        await store.setUpFaceID()
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
        StubURLProtocol.handler = { _ in .init(body: settingsJSON(hasPin: false)) }
        let client = StubURLProtocol.makeClient()
        let store = ApprovalsStore(api: { ApprovalsAPI(client: client) }, signer: failing, notifier: notifier, defaults: defaults)
        await store.setUpFaceID()
        XCTAssertEqual(store.settingsMessage, ApprovalSignError.needsDeviceSecurity.userMessage)
        XCTAssertTrue(StubURLProtocol.requests.filter { $0.url?.path == "/v1/approvals/keys" }.isEmpty)
        // Signing refused for the same reason is a message on the prompt, not a silent dead end.
        let item = ApprovalItem(id: "apr_1", summary: "x", sensitive: true)
        failing.keyId = "k"
        failing.error = ApprovalSignError.needsDeviceSecurity
        let result = await store.answer(item, .approve(.cardApproveButton))
        XCTAssertEqual(result, .failed(ApprovalSignError.needsDeviceSecurity.userMessage))
    }

    // MARK: v2: the PIN

    func testPinRequestBodies() async throws {
        let api = ApprovalsAPI(client: StubURLProtocol.makeClient())
        try await api.setPin("123456")
        try await api.setPin("654321", currentPin: "123456")
        try await api.setPin("111111", proof: .init(signature: "c2ln"))
        try await api.deletePin(currentPin: "654321")
        try await api.deletePin(proof: .init(signature: "c2ln"))
        let r = StubURLProtocol.requests
        XCTAssertEqual(r.map { "\($0.httpMethod ?? "") \($0.url?.path ?? "")" },
                       ["PUT /v1/approvals/pin", "PUT /v1/approvals/pin", "PUT /v1/approvals/pin", "DELETE /v1/approvals/pin", "DELETE /v1/approvals/pin"])
        XCTAssertEqual(decodeBody(r[0]) as NSDictionary, ["pin": "123456"])
        XCTAssertEqual(decodeBody(r[1]) as NSDictionary, ["pin": "654321", "currentPin": "123456"])
        XCTAssertEqual(decodeBody(r[2]) as NSDictionary, ["pin": "111111", "proof": ["signature": "c2ln"]])
        XCTAssertEqual(decodeBody(r[3]) as NSDictionary, ["currentPin": "654321"])
        XCTAssertEqual(decodeBody(r[4]) as NSDictionary, ["proof": ["signature": "c2ln"]])
    }

    func testChoosingAPinNeedsNoProofAndTheStoreSendsIt() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/approvals/settings" ? .init(body: settingsJSON(hasPin: false, serverKey: true)) : .init(body: "{}")
        }
        let result = await store.setPIN("123456")
        XCTAssertEqual(result, .done)
        XCTAssertTrue(signer.signed.isEmpty)
        let put = try XCTUnwrap(StubURLProtocol.requests.first { $0.httpMethod == "PUT" && $0.url?.path == "/v1/approvals/pin" })
        XCTAssertEqual(decodeBody(put) as NSDictionary, ["pin": "123456"])
        // Not six digits: refused before anything is sent.
        let before = StubURLProtocol.requests.count
        let bad = await store.setPIN("12345")
        XCTAssertEqual(bad, .refused("A PIN is 6 digits.", currentWasWrong: false))
        XCTAssertEqual(StubURLProtocol.requests.count, before)
    }

    func testChangingThePinSignsTheProofStringWhenThereIsAKey() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/approvals/settings" ? .init(body: settingsJSON(hasPin: true, serverKey: true)) : .init(body: "{}")
        }
        XCTAssertEqual(ApprovalsAPI.pinProofString(deviceId: "dev_1"), "secretary-approval-pin-v1\ndev_1")
        let result = await store.setPIN("654321")
        XCTAssertEqual(result, .done)
        XCTAssertEqual(signer.signed.map(\.message), ["secretary-approval-pin-v1\ndev_1"])
        let put = try XCTUnwrap(StubURLProtocol.requests.first { $0.httpMethod == "PUT" && $0.url?.path == "/v1/approvals/pin" })
        XCTAssertEqual(decodeBody(put) as NSDictionary, ["pin": "654321", "proof": ["signature": "c2ln"]])
        // Removing it the same way.
        let removed = await store.removePIN()
        XCTAssertEqual(removed, .done)
        XCTAssertEqual(signer.signed.map(\.message), ["secretary-approval-pin-v1\ndev_1", "secretary-approval-pin-v1\ndev_1"])
        let delete = try XCTUnwrap(StubURLProtocol.requests.first { $0.httpMethod == "DELETE" && $0.url?.path == "/v1/approvals/pin" })
        XCTAssertEqual(decodeBody(delete) as NSDictionary, ["proof": ["signature": "c2ln"]])
    }

    func testChangingThePinWithTheCurrentPinSignsNothing() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/approvals/settings" ? .init(body: settingsJSON(hasPin: true)) : .init(body: "{}")
        }
        let result = await store.setPIN("654321", currentPin: "123456")
        XCTAssertEqual(result, .done)
        XCTAssertTrue(signer.signed.isEmpty)
        let put = try XCTUnwrap(StubURLProtocol.requests.first { $0.httpMethod == "PUT" && $0.url?.path == "/v1/approvals/pin" })
        XCTAssertEqual(decodeBody(put) as NSDictionary, ["pin": "654321", "currentPin": "123456"])
    }

    func testAWrongCurrentPinSendsTheOwnerBackToTheCurrentStep() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { request in
            if request.httpMethod == "PUT" { return .init(status: 403, body: #"{"error":{"code":"pin_wrong","message":"no"},"attemptsLeft":2}"#) }
            return .init(body: settingsJSON(hasPin: true))
        }
        let result = await store.setPIN("654321", currentPin: "000000")
        XCTAssertEqual(result, .refused("Wrong PIN, 2 tries left", currentWasWrong: true))
    }

    func testDecodesHasPin() throws {
        XCTAssertTrue(try Fixtures.decode(ApprovalSettings.self, settingsJSON(hasPin: true)).hasPin)
        XCTAssertFalse(try Fixtures.decode(ApprovalSettings.self, settingsJSON(hasPin: false)).hasPin)
        XCTAssertFalse(try Fixtures.decode(ApprovalSettings.self, #"{"thisDeviceId":"dev_1","devices":[]}"#).hasPin)
    }

    // MARK: v2: approving with the PIN

    func testPinFallbackWhenThereIsNoKey() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/approvals/settings" { return .init(body: settingsJSON(hasPin: true)) }
            return .init(body: #"{"status":"approved"}"#)
        }
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_pin", sensitive: true))
        await store.loadSettings()
        // No key, PIN set: nothing is sent, the pad opens.
        let first = await store.answer(item, .approve(.cardApproveButton))
        XCTAssertEqual(first, .pinRequired)
        XCTAssertTrue(decideRequests().isEmpty)
        await store.apply([item])
        await store.decide(.approve(.cardApproveButton))
        XCTAssertTrue(store.pinEntry)
        XCTAssertNil(store.outcome)
        XCTAssertTrue(decideRequests().isEmpty)
        // The PIN goes in the decide body, with no proof, and nothing is signed.
        await store.decide(.approve(.cardApproveButton), pin: "123456")
        XCTAssertEqual(store.outcome, .approved)
        XCTAssertTrue(signer.signed.isEmpty)
        let body = decodeBody(try XCTUnwrap(decideRequests().first))
        XCTAssertEqual(body["decision"] as? String, "approve")
        XCTAssertEqual(body["payloadHash"] as? String, item.payloadHash)
        XCTAssertEqual(body["pin"] as? String, "123456")
        XCTAssertNil(body["proof"])
    }

    func testUsePinInsteadWhenTheKeyExists() async throws {
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"approved"}"#) }
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_pin2", sensitive: true))
        let result = await store.answer(item, .approve(.cardApproveButton), pin: "246810")
        XCTAssertEqual(result, .approved)
        XCTAssertTrue(signer.signed.isEmpty)
        XCTAssertEqual(decodeBody(try XCTUnwrap(decideRequests().first))["pin"] as? String, "246810")
    }

    func testFaceIDUnavailableFallsBackToThePinWhenSet() async throws {
        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/approvals/settings" ? .init(body: settingsJSON(hasPin: true, serverKey: true)) : .init(body: "{}")
        }
        await store.loadSettings()
        signer.error = ApprovalSignError.needsDeviceSecurity
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_lock", sensitive: true))
        let result = await store.answer(item, .approve(.cardApproveButton))
        XCTAssertEqual(result, .pinRequired)
        XCTAssertTrue(decideRequests().isEmpty)
    }

    func testNeitherKeyNorPinMeansSetUpFirst() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { request in
            request.url?.path == "/v1/approvals/settings" ? .init(body: settingsJSON(hasPin: false)) : .init(body: "{}")
        }
        await store.loadSettings()
        XCTAssertFalse(store.hasPin)
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_setup", sensitive: true))
        let result = await store.answer(item, .approve(.cardApproveButton))
        XCTAssertEqual(result, .needsSetup)
        XCTAssertTrue(decideRequests().isEmpty)
        // A request that is not sensitive never needs either.
        let plain = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_plain2", sensitive: false))
        StubURLProtocol.handler = { _ in .init(body: #"{"status":"approved"}"#) }
        let ok = await store.answer(plain, .approve(.cardApproveButton))
        XCTAssertEqual(ok, .approved)
    }

    func testPinWrongAndPinLockedAreMapped() async throws {
        func outcome(_ status: Int, _ code: String, body: String, headers: [String: String] = [:]) async throws -> ApprovalOutcome {
            StubURLProtocol.handler = { _ in
                .init(status: status, body: body, headers: ["Content-Type": "application/json"].merging(headers) { $1 })
            }
            let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_m", sensitive: true))
            let result = await store.answer(item, .approve(.cardApproveButton), pin: "000000")
            return try XCTUnwrap(result)
        }
        // attemptsLeft next to the error, or inside it.
        let a = try await outcome(403, "pin_wrong", body: #"{"error":{"code":"pin_wrong","message":"Wrong PIN"},"attemptsLeft":3}"#)
        XCTAssertEqual(a, .pinWrong(attemptsLeft: 3))
        XCTAssertEqual(a.pinMessage, "Wrong PIN, 3 tries left")
        let b = try await outcome(403, "pin_wrong", body: #"{"error":{"code":"pin_wrong","message":"Wrong PIN","attemptsLeft":1}}"#)
        XCTAssertEqual(b, .pinWrong(attemptsLeft: 1))
        XCTAssertEqual(b.pinMessage, "Wrong PIN, 1 try left")
        let c = try await outcome(403, "pin_wrong", body: #"{"error":{"code":"pin_wrong","message":"Wrong PIN"}}"#)
        XCTAssertEqual(c, .pinWrong(attemptsLeft: nil))
        XCTAssertEqual(c.pinMessage, "Wrong PIN")
        let locked = try await outcome(429, "pin_locked", body: #"{"error":{"code":"pin_locked","message":"locked"}}"#, headers: ["Retry-After": "900"])
        XCTAssertEqual(locked, .pinLocked(minutes: 15))
        XCTAssertEqual(locked.pinMessage, "PIN locked, try again in 15 min")
        let soon = try await outcome(429, "pin_locked", body: #"{"error":{"code":"pin_locked","message":"locked"}}"#, headers: ["Retry-After": "61"])
        XCTAssertEqual(soon, .pinLocked(minutes: 2))
        XCTAssertFalse(a.isFinal)
        XCTAssertFalse(locked.isFinal)
    }

    func testAWrongPinKeepsThePadOpenWithTheTriesLeft() async throws {
        signer.keyId = nil
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/approvals/settings" { return .init(body: settingsJSON(hasPin: true)) }
            return .init(status: 403, body: #"{"error":{"code":"pin_wrong","message":"no"},"attemptsLeft":4}"#)
        }
        await store.loadSettings()
        let item = try Fixtures.decode(ApprovalItem.self, itemJSON(id: "apr_w", sensitive: true))
        await store.apply([item])
        await store.decide(.approve(.cardApproveButton), pin: "000000")
        XCTAssertEqual(store.outcome, .pinWrong(attemptsLeft: 4))
        XCTAssertTrue(store.pinEntry)
        XCTAssertEqual(store.sheetItem?.id, "apr_w")
        XCTAssertFalse(store.outcome?.isFinal ?? true)
    }
}
