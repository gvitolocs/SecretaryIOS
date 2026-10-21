import SwiftUI

/// Settings → Lucina's access: one switch per source, each explained before iOS asks.
struct AccessView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.openURL) private var openURL
    @State private var explaining: DeviceSource?
    @State private var busy: DeviceSource?

    private var hub: DeviceHub { app.device }

    var body: some View {
        List {
            Section {
                Text("Lucina only sees and changes what you turn on here. Everything goes to your own server and Lucina, never to anyone else.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            Section("On this iPhone") {
                ForEach([DeviceSource.alarms, .reminders, .calendar, .health, .location, .focus]) { row($0) }
            }
            Section {
                ForEach([DeviceSource.safari, .screenTime]) { row($0) }
            } header: {
                Text("Browsing and Screen Time")
            } footer: {
                Text("The Safari extension and the Screen Time report are separate parts of the app; see the notes under each.")
            }
            if hub.bridge.queuedEvents > 0 || !hub.bridge.serverSupportsDevice {
                Section {
                    LabeledContent("Waiting to send", value: "\(hub.bridge.queuedEvents)")
                    if !hub.bridge.serverSupportsDevice {
                        Text("The server doesn't accept device events yet; they wait on the phone and go out once it does.")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                }
            }
            if !hub.bridge.recent.isEmpty {
                Section("Recent commands from Lucina") {
                    ForEach(Array(hub.bridge.recent.enumerated()), id: \.offset) { _, entry in
                        HStack(alignment: .top) {
                            Image(systemName: entry.result.ok ? "checkmark.circle.fill" : "xmark.circle.fill")
                                .foregroundStyle(entry.result.ok ? .green : .red)
                                .accessibilityHidden(true)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(entry.command.summary).font(.subheadline)
                                if let error = entry.result.error { Text(error).font(.caption).foregroundStyle(.secondary) }
                            }
                        }
                        .accessibilityElement(children: .combine)
                    }
                }
            }
        }
        .navigationTitle("Lucina's access")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(item: $explaining) { source in
            Explain(source: source) {
                explaining = nil
                Task {
                    busy = source
                    await hub.set(source, on: true)
                    busy = nil
                }
            }
        }
    }

    private func row(_ source: DeviceSource) -> some View {
        let status = hub.access.status(source)
        return VStack(alignment: .leading, spacing: 6) {
            Toggle(isOn: Binding(
                get: { hub.access.isOn(source) },
                set: { on in
                    if on { explaining = source } else { Task { await hub.set(source, on: false) } }
                }
            )) {
                Label(source.title, systemImage: source.symbol)
            }
            .disabled(busy == source || { if case .unavailable = status { return true } else { return false } }())
            .accessibilityIdentifier("access-\(source.rawValue)")
            switch status {
            case .denied:
                HStack {
                    Text("Turned off in iOS Settings.").font(.caption).foregroundStyle(.orange)
                    Button("Open Settings") {
                        if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                    }
                    .font(.caption.bold())
                }
            case .needsPermission:
                Text("Waiting for permission.").font(.caption).foregroundStyle(.secondary)
            case let .unavailable(reason):
                Text(reason).font(.caption).foregroundStyle(.secondary)
            case .on, .off:
                EmptyView()
            }
            if source == .safari && hub.access.isOn(.safari) {
                Text("Turn on Secretary in Settings → Apps → Safari → Extensions, and allow it on all websites.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if source == .focus && hub.access.isOn(.focus) {
                Text("In Settings → Focus, open a Focus, add a Focus Filter and choose Secretary.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }
}

/// Shown before iOS's own permission prompt.
private struct Explain: View {
    let source: DeviceSource
    let onContinue: () -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: source.symbol)
                .font(.system(size: 44))
                .foregroundStyle(Theme.accent)
                .padding(.top, 28)
                .accessibilityHidden(true)
            Text(source.title).font(.title2.bold())
            Text(source.explanation)
                .font(.body)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 24)
            Spacer()
            PrimaryButton(title: "Continue", icon: "checkmark") { onContinue() }
                .padding(.horizontal, 24)
                .accessibilityIdentifier("accessContinue")
            Button("Not now") { dismiss() }
                .padding(.bottom, 20)
        }
        .presentationDetents([.medium])
    }
}

/// Asks before Lucina deletes something or touches the calendar.
struct CommandConfirmation: View {
    @Environment(AppModel.self) private var app
    let command: DeviceCommand

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "sparkles")
                .font(.system(size: 36))
                .foregroundStyle(Theme.accent)
                .padding(.top, 24)
                .accessibilityHidden(true)
            Text("Lucina wants to").font(.headline).foregroundStyle(.secondary)
            Text(command.summary)
                .font(.title3.bold())
                .multilineTextAlignment(.center)
                .padding(.horizontal, 24)
            Spacer()
            PrimaryButton(title: "Allow", icon: "checkmark") {
                Task { await app.device.bridge.approve(command) }
            }
            .padding(.horizontal, 24)
            .accessibilityIdentifier("commandAllow")
            Button("Don't allow", role: .destructive) {
                Task { await app.device.bridge.decline(command) }
            }
            .padding(.bottom, 20)
            .accessibilityIdentifier("commandDecline")
        }
        .presentationDetents([.medium])
        .interactiveDismissDisabled()
    }
}
