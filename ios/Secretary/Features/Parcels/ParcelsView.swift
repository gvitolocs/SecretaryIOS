import SwiftUI

/// Parcels: what Lucina tracks for the owner. Active parcels first, delivered and stopped ones under them.
/// Tracking itself happens at Lucina; this lists what she knows and adds or stops numbers.
struct ParcelsView: View {
    @Environment(AppModel.self) private var app
    @State private var adding = false
    @State private var confirming: Parcel?

    var body: some View {
        let store = app.parcels
        List {
            if store.isUnavailable {
                Section {
                    Label(ParcelsStore.unavailableMessage, systemImage: "wifi.slash")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            } else if let error = store.loadError, !store.hasLoaded {
                Section { Text(error).foregroundStyle(.secondary) }
            }
            if !store.active.isEmpty {
                Section("Being tracked") {
                    ForEach(store.active) { parcel in row(parcel) }
                }
            }
            if !store.finished.isEmpty {
                Section("Delivered and stopped") {
                    ForEach(store.finished) { parcel in row(parcel) }
                }
            }
            if store.hasLoaded, store.parcels.isEmpty, !store.isUnavailable {
                Section { emptyState }
            } else if !store.hasLoaded, store.isLoading {
                Section {
                    ProgressView().frame(maxWidth: .infinity).padding(.vertical, 24)
                }
            }
        }
        .lucinaList()
        .navigationTitle(Text("Parcels"))
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button { adding = true } label: { Image(systemName: "plus") }
                    .accessibilityLabel("Add a parcel")
                    .accessibilityIdentifier("parcelsAdd")
            }
        }
        .refreshable { await store.load() }
        .task { await store.load() }
        .sheet(isPresented: $adding) { ParcelAddSheet() }
        .confirmationDialog(confirmTitle, isPresented: Binding(get: { confirming != nil }, set: { if !$0 { confirming = nil } }),
                            titleVisibility: .visible) {
            Button("Stop tracking", role: .destructive) {
                guard let parcel = confirming else { return }
                confirming = nil
                Task { await store.remove(id: parcel.id) }
            }
        }
        .alert(Text("Parcels"), isPresented: Binding(get: { store.actionError != nil }, set: { if !$0 { store.actionError = nil } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(store.actionError ?? "")
        }
    }

    private var confirmTitle: String {
        guard let parcel = confirming else { return "" }
        return String(localized: "Stop tracking \(ParcelText.title(parcel))? Lucina stops checking it.")
    }

    private func row(_ parcel: Parcel) -> some View {
        NavigationLink {
            ParcelDetailView(id: parcel.id)
        } label: {
            ParcelRow(parcel: parcel)
        }
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            Button(role: .destructive) { confirming = parcel } label: { Label("Stop tracking", systemImage: "trash") }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "shippingbox").font(.system(size: 44)).foregroundStyle(Theme.accent).accessibilityHidden(true)
            Text("Nothing is tracked yet").font(.headline)
            Text("Add a tracking number with the plus button, or tell Lucina “track this <number>”.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    }
}

/// The stage as a small coloured capsule.
struct ParcelStageChip: View {
    let stage: ParcelStage

    var body: some View {
        let color = ParcelText.stageColor(stage)
        Label {
            Text(verbatim: ParcelText.stageName(stage))
        } icon: {
            Image(systemName: ParcelText.stageSymbol(stage))
        }
        .font(.caption.weight(.semibold))
        .foregroundStyle(color)
        .padding(.horizontal, 8)
        .padding(.vertical, 3)
        .background(color.opacity(0.14), in: Capsule())
    }
}

struct ParcelRow: View {
    let parcel: Parcel

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(verbatim: ParcelText.title(parcel))
                    .font(.body.weight(.semibold))
                    .lineLimit(2)
                Spacer(minLength: 0)
                ParcelStageChip(stage: ParcelText.stage(parcel))
            }
            if let subtitle = ParcelText.subtitle(parcel) {
                Text(verbatim: subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            if let text = parcel.status?.text, !text.isEmpty {
                Text(verbatim: text).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
            }
            if let seen = ParcelText.lastSeen(parcel) {
                Label {
                    Text(verbatim: seen)
                } icon: {
                    Image(systemName: "mappin.and.ellipse")
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            schedule
            if let error = parcel.lastError, !error.isEmpty {
                Text(verbatim: error).font(.caption).foregroundStyle(Theme.warning).lineLimit(2)
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var schedule: some View {
        let badge = ParcelText.scheduleBadge(parcel)
        let next = ParcelText.isActive(parcel) ? ParcelText.date(parcel.nextCheck) : nil
        if badge != nil || next != nil {
            HStack(spacing: 8) {
                if let badge {
                    Text(verbatim: badge)
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(parcel.near ? Theme.onAccent : Theme.textSecondary)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 2)
                        .background(parcel.near ? Theme.accentFill : Theme.field, in: Capsule())
                }
                if let next {
                    Text("Next check \(ParcelText.relative(next))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}

/// Tracking number (required), carrier id and label (optional). The server's message shows on a 400.
struct ParcelAddSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var number = ""
    @State private var carrier = ""
    @State private var label = ""
    @FocusState private var numberFocused: Bool

    private static let minNumberLength = 4

    var body: some View {
        let store = app.parcels
        NavigationStack {
            Form {
                Section {
                    TextField("Tracking number", text: $number)
                        .textInputAutocapitalization(.characters)
                        .autocorrectionDisabled()
                        .focused($numberFocused)
                        .accessibilityIdentifier("parcelNumber")
                } footer: {
                    Text("Lucina finds the carrier from the number.")
                }
                Section {
                    TextField("Carrier (optional), like postnord", text: $carrier)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    TextField("Label (optional), like Headphones", text: $label)
                }
                if let error = store.addError {
                    Section { Text(error).foregroundStyle(Theme.negative) }
                }
            }
            .navigationTitle(Text("Add a parcel"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    if store.isAdding {
                        ProgressView()
                    } else {
                        Button("Add") {
                            Task {
                                if await store.add(number: number, carrier: carrier, label: label) { dismiss() }
                            }
                        }
                        .disabled(number.trimmingCharacters(in: .whitespacesAndNewlines).count < Self.minNumberLength)
                        .accessibilityIdentifier("parcelAddConfirm")
                    }
                }
            }
            .onAppear {
                store.addError = nil
                numberFocused = true
            }
        }
        .lucinaSheet()
        .presentationDetents([.medium, .large])
    }
}
