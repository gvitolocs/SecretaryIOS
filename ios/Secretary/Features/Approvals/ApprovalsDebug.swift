import Foundation

/// What the debug event log keeps of approvals (docs/debug-events.md): the id, the action's kind, sensitive, the
/// decision, how it was proven, the outcome and error codes. Never the summary, the commands, the plan, the PIN, a
/// proof or a signature. A separate file, so the approval code itself only gains one-line calls.
enum ApprovalsDebug {
    static func decide(_ id: String, _ decision: ApprovalDecision, sensitive: Bool, via: String,
                       outcome: ApprovalOutcome? = nil, error: Error? = nil) {
        var fields: [String: Any] = ["id": id, "decision": decision.rawValue, "sensitive": sensitive, "via": via]
        if let outcome { fields["outcome"] = outcome.debugName }
        if let error { fields["error"] = error }
        DebugEvents.record("approval.decide", fields)
    }

    /// The card's Approve / Deny (a button, the PIN pad, or the approve step a notification or the Live Activity
    /// started). `ignored`: the card was busy or already answered, so nothing was sent.
    static func answer(_ choice: ApprovalAnswer, id: String?, pad: Bool, ignored: Bool) {
        var fields: [String: Any] = ["screen": "approval_sheet", "pad": pad, "ignored": ignored]
        switch choice {
        case .deny: fields["element"] = "deny"
        case let .approve(trigger): fields["element"] = "approve"; fields["trigger"] = trigger.rawValue
        }
        if let id { fields["id"] = id }
        DebugEvents.record("ui.tap", fields)
    }

    /// A step of the notification's one-tap Approve that ended it early (`not_pending`, `expired`, `changed`, …).
    static func oneTap(_ id: String, _ step: String) {
        DebugEvents.record("approval.one_tap", ["id": id, "step": step])
    }

    /// What came of the Live Activity's Approve: `decided_unlock_key`, `decided_plain`, `opened_card` or `refused`, and why.
    static func activityApprove(_ id: String, outcome: String, reason: String) {
        DebugEvents.record("approval.activity_approve", ["id": id, "outcome": outcome, "reason": reason])
    }

    /// The unlock key's signature for a one-tap Approve (notification or Live Activity).
    static func unlockSign(_ id: String, trigger: ApprovalTrigger, error: Error?) {
        var fields: [String: Any] = ["signer": "unlock", "id": id, "trigger": trigger.rawValue, "ok": error == nil]
        if let error { fields["error"] = error }
        DebugEvents.record("auth.result", fields)
    }

    /// A step of the Face ID key setup or the unlock-key enrollment.
    static func enroll(_ type: String, _ step: String, error: Error? = nil) {
        var fields: [String: Any] = ["step": step]
        if let error { fields["error"] = error }
        DebugEvents.record(type, fields)
    }

    /// A button on the approval card or in Settings → Approvals.
    static func tap(_ element: String, screen: String, id: String? = nil, on: Bool? = nil) {
        var fields: [String: Any] = ["element": element, "screen": screen]
        if let id { fields["id"] = id }
        if let on { fields["on"] = on }
        DebugEvents.record("ui.tap", fields)
    }
}

extension ApprovalOutcome {
    /// The case without its text: `approved`, `already_decided_approved`, `pin_wrong`, `failed`.
    var debugName: String {
        switch self {
        case .approved: return "approved"
        case .denied: return "denied"
        case let .alreadyDecided(status, _): return "already_decided" + (status.map { "_" + $0.rawValue } ?? "")
        case .expired: return "expired"
        case .gone: return "gone"
        case .needsSetup: return "needs_setup"
        case .pinRequired: return "pin_required"
        case .pinWrong: return "pin_wrong"
        case .pinLocked: return "pin_locked"
        case .failed: return "failed"
        }
    }
}
