import ActivityKit
import AppIntents
import SwiftUI
import WidgetKit

/// An approval request on the Lock Screen and in the Dynamic Island: who asks, the reason, the first commands, the
/// time left, and Deny / Approve side by side (no long-press), plus Details for the app's card. Once answered,
/// withdrawn or expired it shows the outcome without buttons. Wording and look follow the approvals widget.
///
/// Deny is the card's only intent button: on iOS 27 the renderer maps every tap on a card with two of them to one
/// action (taps on Deny ran Approve's intent). Approve is a link to the card in the app, which asks Face ID there.
struct ApprovalLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: ApprovalActivityAttributes.self) { context in
            ApprovalLockScreenView(attributes: context.attributes, phase: .init(context))
                .activityBackgroundTint(LucinaTokens.Colors.card.opacity(0.92))
                .activitySystemActionForegroundColor(LucinaTokens.Colors.text)
                .widgetURL(Self.detailsURL(context.attributes))
        } dynamicIsland: { context in
            let phase = ApprovalActivityPhase(context)
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Label(context.attributes.requester, systemImage: "lock.shield")
                        .font(LucinaTokens.Fonts.caption)
                        .foregroundStyle(LucinaTokens.Colors.warning)
                        .lineLimit(1)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    if phase == .pending {
                        ApprovalCountdownText(expiresAt: context.attributes.expiresAt)
                            .font(LucinaTokens.Fonts.caption.monospacedDigit())
                            .foregroundStyle(LucinaTokens.Colors.textSecondary)
                    }
                }
                DynamicIslandExpandedRegion(.bottom) {
                    ApprovalActivityBody(attributes: context.attributes, phase: phase, compact: true)
                }
            } compactLeading: {
                Image(systemName: "lock.shield").foregroundStyle(LucinaTokens.Colors.warning)
            } compactTrailing: {
                if phase == .pending {
                    ApprovalCountdownText(expiresAt: context.attributes.expiresAt)
                        .monospacedDigit()
                        .frame(maxWidth: 44)
                } else {
                    Image(systemName: phase.symbol).foregroundStyle(phase.tint)
                }
            } minimal: {
                Image(systemName: "lock.shield").foregroundStyle(LucinaTokens.Colors.warning)
            }
            .widgetURL(Self.detailsURL(context.attributes))
        }
    }

    static func detailsURL(_ attributes: ApprovalActivityAttributes) -> URL {
        ApprovalLink.url(id: attributes.approvalId)
    }

    /// The card with its approve step, bound to the request shown here.
    static func approveURL(_ attributes: ApprovalActivityAttributes) -> URL {
        ApprovalLink.url(id: attributes.approvalId, approve: attributes.payloadHash)
    }
}

/// Where the request stands, as the activity shows it.
enum ApprovalActivityPhase: Equatable {
    case pending, approved, denied, expired, withdrawn

    init(_ context: ActivityViewContext<ApprovalActivityAttributes>) {
        self.init(status: context.state.status, stale: context.isStale, expiresAt: context.attributes.expiresAt)
    }

    /// A pending request past its expiry (or past the stale date) counts as expired.
    init(status: String, stale: Bool, expiresAt: Date, now: Date = Date()) {
        switch status {
        case "approved": self = .approved
        case "denied": self = .denied
        case "withdrawn": self = .withdrawn
        case "expired": self = .expired
        default: self = stale || now >= expiresAt ? .expired : .pending
        }
    }

    var title: LocalizedStringKey {
        switch self {
        case .pending: return "Waiting for your OK"
        case .approved: return "Approved"
        case .denied: return "Denied"
        case .expired: return "Expired"
        case .withdrawn: return "Withdrawn"
        }
    }

    var symbol: String {
        switch self {
        case .pending: return "lock.shield"
        case .approved: return "checkmark.circle.fill"
        case .denied: return "xmark.circle.fill"
        case .expired: return "clock.badge.xmark"
        case .withdrawn: return "arrow.uturn.backward.circle"
        }
    }

    var tint: Color {
        switch self {
        case .pending: return LucinaTokens.Colors.warning
        case .approved: return LucinaTokens.Colors.positive
        case .denied: return LucinaTokens.Colors.negative
        case .expired, .withdrawn: return LucinaTokens.Colors.textSecondary
        }
    }
}

/// "4:48", counting down to the expiry.
struct ApprovalCountdownText: View {
    let expiresAt: Date

    var body: some View {
        let now = Date()
        Text(timerInterval: now...max(now, expiresAt), countsDown: true)
    }
}

private struct ApprovalLockScreenView: View {
    let attributes: ApprovalActivityAttributes
    let phase: ApprovalActivityPhase

    var body: some View {
        VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
            HStack(spacing: LucinaTokens.Space.sm) {
                Image(systemName: "lock.shield").foregroundStyle(LucinaTokens.Colors.warning)
                Text("\(attributes.requester) is asking")
                    .font(LucinaTokens.Fonts.caption)
                    .foregroundStyle(LucinaTokens.Colors.textSecondary)
                    .lineLimit(1)
                Spacer(minLength: LucinaTokens.Space.xs)
                if phase == .pending {
                    ApprovalCountdownText(expiresAt: attributes.expiresAt)
                        .font(LucinaTokens.Fonts.caption.monospacedDigit())
                        .foregroundStyle(LucinaTokens.Colors.textSecondary)
                        .multilineTextAlignment(.trailing)
                        .frame(maxWidth: 60, alignment: .trailing)
                }
            }
            ApprovalActivityBody(attributes: attributes, phase: phase, compact: false)
        }
        .padding(LucinaTokens.Space.lg)
    }
}

/// The reason, the commands and the buttons (pending), or the outcome.
private struct ApprovalActivityBody: View {
    let attributes: ApprovalActivityAttributes
    let phase: ApprovalActivityPhase
    let compact: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: compact ? LucinaTokens.Space.xs : LucinaTokens.Space.sm) {
            // What is asked stays hidden while the iPhone is locked (WidgetKit redacts it); who asks, the time left and
            // the buttons stay.
            Text(attributes.summary)
                .font(compact ? LucinaTokens.Fonts.subheadline.weight(.semibold) : LucinaTokens.Fonts.headline)
                .foregroundStyle(LucinaTokens.Colors.text)
                .lineLimit(2)
                .privacySensitive()
            if phase == .pending {
                commands
                buttons
            } else {
                // The outcome, readable at a glance for the few seconds the activity stays: no buttons.
                HStack(spacing: LucinaTokens.Space.sm) {
                    Image(systemName: phase.symbol)
                        .font(.system(size: compact ? 24 : 32, weight: .semibold))
                    Text(phase.title)
                        .font(compact ? LucinaTokens.Fonts.title3 : LucinaTokens.Fonts.title)
                }
                .foregroundStyle(phase.tint)
                .frame(maxWidth: .infinity, alignment: .leading)
                .accessibilityElement(children: .combine)
            }
        }
    }

    @ViewBuilder private var commands: some View {
        let lines = attributes.commands.split(separator: "\n", omittingEmptySubsequences: true)
        if !lines.isEmpty {
            VStack(alignment: .leading, spacing: 1) {
                // The same lines in the Dynamic Island and on the Lock Screen, wrapped, never cut in the middle (the card
                // shows them all before Face ID).
                ForEach(Array(lines.prefix(ApprovalActivityAttributes.shownLines).enumerated()), id: \.offset) { _, line in
                    Text(String(line))
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundStyle(LucinaTokens.Colors.textSecondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
                let shown = min(lines.count, ApprovalActivityAttributes.shownLines)
                if attributes.commandCount > shown {
                    Text("+\(attributes.commandCount - shown) more")
                        .font(LucinaTokens.Fonts.caption2)
                        .foregroundStyle(LucinaTokens.Colors.textTertiary)
                }
            }
            .privacySensitive()
        }
    }

    private var buttons: some View {
        VStack(spacing: LucinaTokens.Space.xs) {
            HStack(spacing: LucinaTokens.Space.sm) {
                // The card's one intent button (see ApprovalLiveActivity).
                Button(intent: DenyApprovalActivityIntent(approvalId: attributes.approvalId, sensitive: attributes.sensitive)) {
                    Label("Deny", systemImage: "xmark").frame(maxWidth: .infinity, minHeight: compact ? 28 : 36)
                }
                .buttonStyle(.borderedProminent)
                .tint(LucinaTokens.Colors.negative)
                // A link, drawn like the prominent button beside it: the app's card asks Face ID.
                Link(destination: ApprovalLiveActivity.approveURL(attributes)) {
                    Label("Approve", systemImage: attributes.sensitive ? "faceid" : "checkmark")
                        .foregroundStyle(.white)
                        .frame(maxWidth: .infinity, minHeight: compact ? 28 : 36)
                        .background(LucinaTokens.Colors.accent, in: Capsule())
                }
            }
            .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
            .labelStyle(.titleAndIcon)
            if !compact {
                Link(destination: ApprovalLiveActivity.detailsURL(attributes)) {
                    Text("Details").font(LucinaTokens.Fonts.caption).foregroundStyle(LucinaTokens.Colors.accent)
                }
            }
        }
    }
}
