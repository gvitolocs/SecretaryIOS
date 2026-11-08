import SwiftUI

// Agents (owner, 2026-10-28): the coding agents on nezopt, read-mostly plus steering. The list is
// `GET /v1/agents/items`, kept fresh by the SSE stream; a row opens the agent (AgentDetailView).

enum AgentColors {
    static func bucket(_ b: AgentBucket) -> Color {
        switch b {
        case .needsInput: Theme.warning
        case .failed: Theme.negative
        case .running: Theme.accent
        case .attention: Theme.positive
        case .done: Theme.textSecondary
        }
    }

    static func symbol(_ b: AgentBucket) -> String {
        switch b {
        case .needsInput: "hand.raised.fill"
        case .failed: "exclamationmark.triangle.fill"
        case .running: "play.circle.fill"
        case .attention: "checkmark.circle.fill"
        case .done: "circle"
        }
    }
}

/// Keeps the stream on while the screen is up and the app is active.
private struct AgentsLive: ViewModifier {
    let store: AgentsStore
    @Environment(\.scenePhase) private var phase

    func body(content: Content) -> some View {
        content.task(id: phase) {
            guard phase == .active else { return }
            store.retain()
            await withTaskCancellationHandler {
                try? await Task.sleep(for: .seconds(86_400))
            } onCancel: {
                Task { @MainActor in store.release() }
            }
        }
    }
}

extension View {
    func agentsLive(_ store: AgentsStore) -> some View { modifier(AgentsLive(store: store)) }
}

struct AgentsView: View {
    @Environment(AgentsStore.self) private var store

    var body: some View {
        List {
            if store.unavailable {
                Section { Text("This server doesn't offer Agents yet.").foregroundStyle(.secondary) }
            } else if let error = store.loadError, store.items.isEmpty {
                Section { Text(verbatim: error).foregroundStyle(.secondary) }
            }
            ForEach(store.sections, id: \.bucket) { section in
                Section {
                    ForEach(section.items) { item in
                        NavigationLink {
                            AgentDetailView(agentId: item.id, fallbackTitle: item.displayTitle)
                        } label: {
                            AgentRow(item: item)
                        }
                    }
                } header: {
                    Text(Self.title(section.bucket))
                }
            }
            if store.loaded, store.items.isEmpty, !store.unavailable {
                Section {
                    VStack(spacing: 8) {
                        Image(systemName: "terminal").font(.system(size: 44)).foregroundStyle(Theme.accent)
                        Text(store.showArchived ? "No archived agents" : "No agents yet").font(.headline)
                        Text("Start one from Secretary Code or ask Lucina.").font(.footnote).foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity).padding(.vertical, 24)
                }
            }
        }
        .lucinaList()
        .navigationTitle(Text("Agents"))
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Menu {
                    Toggle(isOn: Binding(get: { store.showArchived }, set: { on in Task { await store.setArchived(on) } })) {
                        Label("Show archived", systemImage: "archivebox")
                    }
                } label: {
                    Image(systemName: store.showArchived ? "line.3.horizontal.decrease.circle.fill" : "line.3.horizontal.decrease.circle")
                }
                .accessibilityLabel(Text("Filter"))
            }
            ToolbarItem(placement: .topBarLeading) {
                if store.isLive {
                    Label("Live", systemImage: "dot.radiowaves.left.and.right")
                        .labelStyle(.iconOnly)
                        .foregroundStyle(Theme.positive)
                        .accessibilityLabel(Text("Live updates on"))
                }
            }
        }
        .refreshable { await store.refresh() }
        .task { await store.refresh() }
        .task {
            // Without the stream, look again every 5 s.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(5))
                if !store.isLive, !store.unavailable { await store.refresh() }
            }
        }
        .onChange(of: store.isLive) { _, live in
            if live { Task { await store.refresh() } }
        }
        .agentsLive(store)
        .accessibilityIdentifier("agentsList")
    }

    static func title(_ b: AgentBucket) -> LocalizedStringKey {
        switch b {
        case .needsInput: "Needs permission"
        case .failed: "Failed"
        case .running: "Running"
        case .attention: "Finished"
        case .done: "Idle and closed"
        }
    }
}

struct AgentRow: View {
    let item: AgentListItem

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: AgentColors.symbol(item.bucket))
                .font(.title3)
                .foregroundStyle(AgentColors.bucket(item.bucket))
                .frame(width: 26)
                .padding(.top, 2)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(verbatim: item.displayTitle)
                    .font(.body.weight(.medium))
                    .lineLimit(2)
                HStack(spacing: 6) {
                    Text(verbatim: item.stateLabel)
                        .foregroundStyle(AgentColors.bucket(item.bucket))
                    if !item.providerLine.isEmpty {
                        Text(verbatim: "·")
                        Text(verbatim: item.providerLine).lineLimit(1)
                    }
                    if item.external {
                        Image(systemName: "laptopcomputer").accessibilityLabel(Text("Runs on the Mac"))
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer(minLength: 0)
            if let when = AgentDates.relative(item.updatedAt) {
                Text(verbatim: when).font(.caption).foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
    }
}
