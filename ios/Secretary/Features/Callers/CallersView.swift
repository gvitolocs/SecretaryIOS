import SwiftUI

/// "Who called?": paste a number or a list, see what Jev makes of each, and check that the Call Directory
/// extension is on (it labels the same numbers on the incoming-call screen).
struct CallersView: View {
    @Environment(AppModel.self) private var app
    @FocusState private var focused: Bool

    var body: some View {
        @Bindable var store = app.callers
        ScrollView {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                if store.extensionStatus != .enabled { labelsCard(store) }
                lookupCard(store)
                if let error = store.lookupError { ErrorBanner(message: error) }
                ForEach(Array(store.results.enumerated()), id: \.offset) { _, result in
                    CallerResultCard(result: result)
                }
                if store.extensionStatus == .enabled { labelsCard(store) }
            }
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.top, LucinaTokens.Space.sm)
            .padding(.bottom, LucinaTokens.Space.xxl)
        }
        .lucinaScreen("Who called?")
        .scrollDismissesKeyboard(.interactively)
        .task {
            await store.refreshStatus()
            #if DEBUG || SCREENSHOTS
            // `-SecretaryCallersLookup "+45 70 40 52 05"`: fills the field and looks it up (screenshots, device tests).
            if let count = UserDefaults.standard.string(forKey: "SecretaryCallersSeed").flatMap(Int.init), count > 0 {
                await store.seedForTesting(count: count)
            }
            if UserDefaults.standard.bool(forKey: "SecretaryCallersReset") { store.reset() }
            if let text = UserDefaults.standard.string(forKey: "SecretaryCallersLookup"), !text.isEmpty, store.input.isEmpty {
                store.input = text
                await store.lookup(text)
            }
            #endif
        }
        .refreshable { await store.sync() }
    }

    private func lookupCard(_ store: CallersStore) -> some View {
        @Bindable var store = store
        return Card(spacing: LucinaTokens.Space.md) {
            Eyebrow(text: "Number or list")
            TextField("+45 70 40 52 05", text: $store.input, axis: .vertical)
                .lineLimit(1 ... 6)
                .keyboardType(.numbersAndPunctuation)
                .textContentType(.telephoneNumber)
                .autocorrectionDisabled()
                .focused($focused)
                .padding(LucinaTokens.Space.md)
                .background(Theme.field, in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.field, style: .continuous))
                .accessibilityIdentifier("callersInput")
            Text("One number, or a pasted list: one per line, or separated by commas.")
                .font(LucinaTokens.Fonts.caption2)
                .foregroundStyle(Theme.textSecondary)
            HStack(spacing: LucinaTokens.Space.md) {
                PasteButton(payloadType: String.self) { strings in
                    Task { @MainActor in store.input = strings.joined(separator: "\n") }
                }
                .buttonBorderShape(.capsule)
                .labelStyle(.titleAndIcon)
                PrimaryButton(title: "Look up", icon: "magnifyingglass", isBusy: store.isLookingUp) {
                    focused = false
                    Task { await store.lookup(store.input) }
                }
                .disabled(CallerNumbers.parse(store.input).isEmpty)
                .accessibilityIdentifier("callersLookup")
            }
        }
    }

    private func labelsCard(_ store: CallersStore) -> some View {
        Card(spacing: LucinaTokens.Space.md) {
            HStack(spacing: 8) {
                Image(systemName: store.extensionStatus.icon)
                    .foregroundStyle(store.extensionStatus.color)
                    .accessibilityHidden(true)
                Text("Caller labels on this iPhone").font(LucinaTokens.Fonts.headline)
            }
            Text(store.extensionStatus.explanation)
                .font(LucinaTokens.Fonts.subheadline)
                .foregroundStyle(Theme.textSecondary)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityIdentifier("callersExtensionStatus")
            if let error = store.syncError {
                Text(error).font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.warning)
            }
            Text(syncLine(store))
                .font(LucinaTokens.Fonts.caption2)
                .foregroundStyle(Theme.textSecondary)
            HStack(spacing: LucinaTokens.Space.md) {
                if store.extensionStatus != .enabled, store.extensionStatus != .unavailable {
                    Button {
                        Task { await store.openCallBlockingSettings() }
                    } label: {
                        Label("Open Settings", systemImage: "gearshape").lineLimit(1).minimumScaleFactor(0.7)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accentFill)
                    .accessibilityIdentifier("callersOpenSettings")
                }
                Button {
                    Task {
                        await store.sync()
                        await store.refreshStatus()
                    }
                } label: {
                    if store.isSyncing { ProgressView() } else { Label("Update labels", systemImage: "arrow.clockwise").lineLimit(1).minimumScaleFactor(0.7) }
                }
                .buttonStyle(.bordered)
                .disabled(store.isSyncing)
                .accessibilityIdentifier("callersUpdate")
            }
        }
    }

    private func syncLine(_ store: CallersStore) -> String {
        guard let count = store.entryCount, let last = store.lastSync else { return String(localized: "Not updated yet.") }
        let when = last.formatted(date: .abbreviated, time: .shortened)
        return String(localized: "\(count) labels saved, updated \(when)")
    }
}

extension CallerExtensionStatus {
    var icon: String {
        switch self {
        case .enabled: return "checkmark.circle.fill"
        case .disabled: return "exclamationmark.triangle.fill"
        case .unknown: return "questionmark.circle"
        case .unavailable: return "xmark.circle"
        }
    }

    var color: Color {
        switch self {
        case .enabled: return Theme.positive
        case .disabled: return Theme.warning
        case .unknown, .unavailable: return Theme.textSecondary
        }
    }

    var explanation: String {
        switch self {
        case .enabled:
            return String(localized: "On. Numbers Lucina has screened show their label when they call.")
        case .disabled:
            return String(localized: "Off. Turn on Secretary under Settings › Phone › Call Blocking & Identification, so calls from numbers Lucina has screened show a label.")
        case .unknown:
            return String(localized: "Checking whether caller labels are on…")
        case .unavailable:
            return String(localized: "Caller labels aren't available in this build.")
        }
    }
}

/// One screened number: the number, Jev's label, the odds, why, and where the verdict came from.
struct CallerResultCard: View {
    let result: CallerResult

    var body: some View {
        Card(spacing: LucinaTokens.Space.sm) {
            HStack(alignment: .firstTextBaseline) {
                Text(verbatim: result.display ?? result.number)
                    .font(LucinaTokens.Fonts.headline.monospacedDigit())
                    .foregroundStyle(Theme.text)
                Spacer(minLength: 8)
                if let source = CallerText.source(result.source) {
                    Text(source)
                        .font(LucinaTokens.Fonts.caption2)
                        .foregroundStyle(Theme.textSecondary)
                }
            }
            if let place = CallerText.place(result) {
                Text(verbatim: place).font(LucinaTokens.Fonts.caption2).foregroundStyle(Theme.textSecondary)
            }
            if let label = result.label, !label.isEmpty {
                Text(verbatim: label)
                    .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                    .foregroundStyle(result.isInvalid ? Theme.textSecondary : CallerText.color(spam: result.spam))
                    .accessibilityIdentifier("callerLabel")
            }
            if !result.isInvalid {
                HStack(alignment: .top, spacing: LucinaTokens.Space.md) {
                    Metric(value: CallerText.percent(result.spam), label: "Spam")
                    Metric(value: CallerText.percent(result.pickUp), label: "Pick up")
                }
                .padding(.top, 2)
                if let kind = CallerText.kind(result.kind) {
                    VStack(alignment: .leading, spacing: 2) {
                        Eyebrow(text: "Kind")
                        Text(verbatim: result.kindP.map { "\(kind) \(CallerText.percent($0))" } ?? kind)
                            .font(LucinaTokens.Fonts.callout)
                            .foregroundStyle(Theme.text)
                    }
                }
                if let contact = result.known?.contact, !contact.isEmpty {
                    Text("Contact: \(contact)").font(LucinaTokens.Fonts.callout).foregroundStyle(Theme.text)
                }
            }
            if !result.reasons.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(result.reasons.enumerated()), id: \.offset) { _, reason in
                        Text(verbatim: "· \(reason)")
                            .font(LucinaTokens.Fonts.footnote)
                            .foregroundStyle(Theme.textSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("callerResult")
    }
}

extension CallerText {
    static func color(spam: Double?) -> Color {
        guard let spam else { return Theme.text }
        if spam >= 0.7 { return Theme.negative }
        if spam >= 0.4 { return Theme.warning }
        return Theme.positive
    }
}
