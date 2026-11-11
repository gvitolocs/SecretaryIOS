import SwiftUI

/// One parcel: where it stands, when Lucina checks it next, and the carrier's events as a timeline.
struct ParcelDetailView: View {
    let id: String
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var renaming = false
    @State private var draft = ""
    @State private var confirmStop = false

    var body: some View {
        let store = app.parcels
        Group {
            if let parcel = store.parcel(id: id) {
                content(parcel, store: store)
            } else {
                ContentUnavailableView("This parcel isn't tracked anymore.", systemImage: "shippingbox")
            }
        }
        .lucinaScreen()
        .navigationBarTitleDisplayMode(.inline)
        .task(id: id) { await store.reload(id: id) }
        .alert(Text("Parcels"), isPresented: Binding(get: { store.actionError != nil }, set: { if !$0 { store.actionError = nil } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(store.actionError ?? "")
        }
    }

    private func content(_ parcel: Parcel, store: ParcelsStore) -> some View {
        let busy = store.busy.contains(parcel.id)
        return ScrollView {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                header(parcel)
                if let error = parcel.lastError, !error.isEmpty {
                    ErrorBanner(message: error)
                }
                timeline(parcel)
                actions(parcel, store: store, busy: busy)
            }
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.top, LucinaTokens.Space.sm)
            .padding(.bottom, LucinaTokens.Space.xxl)
        }
        .navigationTitle(ParcelText.title(parcel))
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Rename") {
                    draft = parcel.label ?? ""
                    renaming = true
                }
                .disabled(busy)
            }
        }
        .refreshable { await store.reload(id: parcel.id) }
        .alert(Text("Rename"), isPresented: $renaming) {
            TextField("Label", text: $draft)
            Button("Save") {
                let label = String(draft.prefix(80))
                Task { await store.rename(id: parcel.id, label: label) }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Leave it empty to remove the label.")
        }
        .confirmationDialog(String(localized: "Stop tracking \(ParcelText.title(parcel))? Lucina stops checking it."),
                            isPresented: $confirmStop, titleVisibility: .visible) {
            Button("Stop tracking", role: .destructive) {
                Task {
                    if await store.remove(id: parcel.id) { dismiss() }
                }
            }
        }
    }

    // MARK: Header

    private func header(_ parcel: Parcel) -> some View {
        Card(spacing: LucinaTokens.Space.md) {
            HStack(alignment: .firstTextBaseline) {
                Text(verbatim: ParcelText.title(parcel)).font(.title3.weight(.semibold))
                Spacer(minLength: 0)
                ParcelStageChip(stage: ParcelText.stage(parcel))
            }
            if let text = parcel.status?.text, !text.isEmpty {
                Text(verbatim: text).font(.subheadline).foregroundStyle(.secondary)
            }
            if let carrier = ParcelText.carrierName(parcel.carrier) {
                field("Carrier", carrier)
            }
            field("Tracking number", parcel.number)
                .contextMenu {
                    Button { UIPasteboard.general.string = parcel.number } label: { Label("Copy number", systemImage: "doc.on.doc") }
                }
            if let expected = ParcelText.expected(parcel.status?.expectedDelivery) {
                field("Expected delivery", expected)
            }
            if let country = parcel.status?.destinationCountry, !country.isEmpty {
                field("Destination", Locale.current.localizedString(forRegionCode: country) ?? country)
            }
            if let place = parcel.status?.location, !place.isEmpty {
                field("Last seen at", place)
            }
            if let badge = ParcelText.scheduleBadge(parcel) {
                field("Checks", badge)
            } else {
                field("Checks", String(localized: "Stopped"))
            }
            if ParcelText.isActive(parcel), let next = ParcelText.date(parcel.nextCheck) {
                field("Next check", "\(ParcelText.relative(next)) · \(ParcelText.timeline(next))")
            }
            if let last = ParcelText.date(parcel.lastChecked) {
                field("Last checked", "\(ParcelText.relative(last)) · \(ParcelText.timeline(last))")
            } else {
                field("Last checked", String(localized: "Not checked yet"))
            }
        }
    }

    private func field(_ title: LocalizedStringKey, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(title).font(.subheadline).foregroundStyle(.secondary)
            Spacer(minLength: 12)
            Text(verbatim: value).font(.subheadline).multilineTextAlignment(.trailing).textSelection(.enabled)
        }
        .accessibilityElement(children: .combine)
    }

    // MARK: Timeline

    @ViewBuilder private func timeline(_ parcel: Parcel) -> some View {
        Card(spacing: 0) {
            Eyebrow(text: "Events").padding(.bottom, LucinaTokens.Space.md)
            if parcel.events.isEmpty {
                Text("No events yet. Lucina lists them after the first check.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            } else {
                ForEach(Array(parcel.events.enumerated()), id: \.offset) { index, event in
                    ParcelEventRow(event: event, isLast: index == parcel.events.count - 1)
                }
            }
        }
    }

    // MARK: Actions

    private func actions(_ parcel: Parcel, store: ParcelsStore, busy: Bool) -> some View {
        VStack(spacing: LucinaTokens.Space.md) {
            if ParcelText.isActive(parcel) {
                Button {
                    Task { await store.checkNow(id: parcel.id) }
                } label: {
                    HStack(spacing: 8) {
                        if busy { ProgressView() }
                        Text("Check now").font(.body.weight(.semibold))
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
                }
                .buttonStyle(.borderedProminent)
                .disabled(busy)
                .accessibilityIdentifier("parcelCheckNow")
            }
            Button(role: .destructive) {
                confirmStop = true
            } label: {
                Text("Stop tracking")
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 6)
            }
            .buttonStyle(.bordered)
            .disabled(busy)
        }
    }
}

/// One event: a stage dot on a vertical line, the local time, the description and the place.
private struct ParcelEventRow: View {
    let event: ParcelEvent
    let isLast: Bool

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(spacing: 0) {
                Circle()
                    .fill(ParcelText.stageColor(event.stage))
                    .frame(width: 10, height: 10)
                    .padding(.top, 5)
                if !isLast {
                    Rectangle()
                        .fill(Theme.separator)
                        .frame(width: 2)
                        .frame(maxHeight: .infinity)
                }
            }
            .frame(width: 10)
            .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                if let time = ParcelText.date(event.time) {
                    Text(verbatim: ParcelText.timeline(time)).font(.caption).foregroundStyle(.secondary)
                }
                Text(verbatim: event.description).font(.subheadline)
                if let place = event.location, !place.isEmpty {
                    Text(verbatim: place).font(.caption).foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.bottom, isLast ? 0 : LucinaTokens.Space.md)
        }
        .accessibilityElement(children: .combine)
    }
}
