import SwiftUI

/// Settings → Plugins → Pixiv: connect the owner's Pixiv account (he signs in on pixiv's own page), see the
/// state of the saved sign-in, and choose what Pixiv searches show first.
struct PixivSettingsScreen: View {
    @State private var store: PixivStore
    @State private var confirmDisconnect = false

    init(app: AppModel) {
        _store = State(initialValue: PixivStore(
            api: { app.client.accessKey?.isEmpty == false ? PixivAPI(client: app.client) : nil },
            onSessionEnded: { app.handle($0) }))
    }

    var body: some View {
        Form {
            if let connection = store.connection {
                accountSection(connection)
                if connection.state == .connected, connection.r18 == false { r18Section }
                defaultsSection
            } else if let error = store.loadError {
                Section {
                    Text(error).font(.footnote).foregroundStyle(.red)
                    Button("Try again") { Task { await store.load() } }
                }
            } else {
                Section { HStack { Spacer(); ProgressView(); Spacer() } }
            }
        }
        .navigationTitle("Pixiv")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await store.load() }
        .task { await store.load() }
        // While a save or removal waits for Face ID, ask again every 3 s (5 min at most). The approval itself
        // arrives through the Approvals flow.
        .task(id: store.isWaitingForApproval) {
            guard store.isWaitingForApproval else { return }
            await store.pollWhileWaiting()
        }
        .confirmationDialog("Disconnect Pixiv?", isPresented: $confirmDisconnect, titleVisibility: .visible) {
            Button("Disconnect", role: .destructive) { Task { await store.disconnect() } }
        } message: {
            Text("The saved sign-in is deleted from your secrets after you approve with Face ID.")
        }
    }

    // MARK: Account

    @ViewBuilder private func accountSection(_ connection: PixivConnection) -> some View {
        Section {
            HStack {
                Text(PixivText.status(connection))
                    .accessibilityIdentifier("pixivStatus")
                if connection.isWaitingForApproval {
                    Spacer()
                    ProgressView()
                }
            }
            ForEach(PixivText.notes(connection), id: \.self) { line in
                Text(line).font(.footnote).foregroundStyle(.secondary)
            }
            if let error = store.loadError {
                Text(error).font(.footnote).foregroundStyle(.red)
            }

            Button {
                Task { await store.connect() }
            } label: {
                HStack {
                    Label(connection.state == .connected || connection.state == .expired ? "Reconnect" : "Connect Pixiv",
                          systemImage: "person.badge.key")
                    Spacer()
                    if store.busy == .connecting { ProgressView() }
                }
            }
            .disabled(store.busy != nil)
            .accessibilityIdentifier("pixivConnect")

            if let error = store.actionError {
                Text(error).font(.footnote).foregroundStyle(.red)
                    .accessibilityIdentifier("pixivActionError")
            }

            if connection.canDisconnect {
                Button(role: .destructive) {
                    confirmDisconnect = true
                } label: {
                    HStack {
                        Label("Disconnect", systemImage: "person.badge.minus")
                        Spacer()
                        if store.busy == .disconnecting { ProgressView() }
                    }
                }
                .disabled(store.busy != nil)
                .accessibilityIdentifier("pixivDisconnect")
            }
        } header: {
            Text("Account")
        } footer: {
            Text("You sign in on pixiv's own page. The app only receives a sign-in token, which goes to your secrets after you approve it with Face ID.")
        }
    }

    // MARK: R-18

    private var r18Section: some View {
        Section {
            Text("Your Pixiv account hides R-18 works. Pixiv only lets you change this on its website: Settings \u{2192} Viewing restrictions \u{2192} Explicit content (R-18): Show.")
                .font(.footnote)
            if let url = URL(string: "https://www.pixiv.net/settings/viewing") {
                Link(destination: url) {
                    Label("Open pixiv.net settings", systemImage: "safari")
                }
            }
        }
    }

    // MARK: Defaults

    private var defaultsSection: some View {
        Section {
            Picker("Default content", selection: Binding(
                get: { store.settings.defaultContent },
                set: { value in Task { await store.setDefaults(content: value) } })
            ) {
                Text("All ages").tag(PixivSettings.Content.safe)
                Text("R-18").tag(PixivSettings.Content.r18)
                Text("All").tag(PixivSettings.Content.all)
            }
            .accessibilityIdentifier("pixivDefaultContent")
            Picker("Default sort", selection: Binding(
                get: { store.settings.defaultSort },
                set: { value in Task { await store.setDefaults(sort: value) } })
            ) {
                Text("Newest").tag(PixivSettings.Sort.dateDesc)
                Text("Popular").tag(PixivSettings.Sort.popular)
            }
            .accessibilityIdentifier("pixivDefaultSort")
            if let error = store.settingsError {
                Text(error).font(.footnote).foregroundStyle(.red)
            }
        } header: {
            Text("Defaults")
        }
    }
}
