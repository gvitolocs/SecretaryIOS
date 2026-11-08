import SwiftUI

/// The Countdown plugin: the events pinned for the countdown widget, then the coming year's
/// events to pin (the pin button, or a long-press). The widget shows the days left to the pinned
/// ones; past events drop off by themselves.
struct CountdownsView: View {
    @Environment(AppModel.self) private var app
    @State private var pinned: [CountdownEvent] = []
    @State private var upcoming: [CountdownEvent] = []
    @State private var loading = false

    private var shownPinned: [CountdownEvent] { Countdown.visible(pinned, fresher: upcoming, now: Date()) }
    private var unpinned: [CountdownEvent] {
        let ids = Set(pinned.map(\.id))
        return upcoming.filter { !ids.contains($0.id) }
    }

    var body: some View {
        List {
            Section {
                if shownPinned.isEmpty {
                    Text("Pin an event to see the days left to it on the Countdown widget.")
                        .font(LucinaTokens.Fonts.subheadline)
                        .foregroundStyle(Theme.textSecondary)
                }
                ForEach(shownPinned) { event in
                    CountdownRow(event: event, pinned: true) { set(event, pinned: false) }
                        .swipeActions {
                            Button(role: .destructive) { set(event, pinned: false) } label: {
                                Label("Unpin from countdown", systemImage: "pin.slash")
                            }
                        }
                }
            } header: {
                Text("Pinned events")
            }
            Section {
                if unpinned.isEmpty {
                    if loading {
                        ProgressView().frame(maxWidth: .infinity)
                    } else {
                        Text("No events in the coming year.").foregroundStyle(Theme.textSecondary)
                    }
                }
                ForEach(unpinned) { event in
                    CountdownRow(event: event, pinned: false) { set(event, pinned: true) }
                }
            } header: {
                Text("Upcoming")
            }
        }
        .lucinaList()
        .navigationTitle("Countdown")
        .task { await load(force: false) }
        .refreshable { await load(force: true) }
    }

    private func load(force: Bool) async {
        pinned = Countdown.pinned
        upcoming = Countdown.upcoming
        loading = true
        let fetcher = WidgetFetcher(client: app.client, mediaClient: app.mediaClient)
        upcoming = await Countdown.refreshUpcoming(fetcher: fetcher, force: force)
        loading = false
    }

    private func set(_ event: CountdownEvent, pinned pin: Bool) {
        Countdown.setPinned(event, pin)
        pinned = Countdown.pinned
    }
}

/// One event: the days left in the accent colour, the title, the date; the pin toggles it.
struct CountdownRow: View {
    let event: CountdownEvent
    let pinned: Bool
    let toggle: () -> Void

    var body: some View {
        HStack(spacing: LucinaTokens.Space.md) {
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: Countdown.label(event, now: Date()))
                    .font(LucinaTokens.Fonts.headline.monospacedDigit())
                    .foregroundStyle(Theme.accent)
                Text(verbatim: event.title)
                    .font(LucinaTokens.Fonts.body)
                    .foregroundStyle(Theme.text)
                    .lineLimit(2)
                Text(verbatim: Countdown.dateText(event))
                    .font(LucinaTokens.Fonts.caption2)
                    .foregroundStyle(Theme.textSecondary)
            }
            Spacer(minLength: 8)
            Button(action: toggle) {
                Image(systemName: pinned ? "pin.fill" : "pin")
                    .font(.body.weight(.semibold))
                    .foregroundStyle(pinned ? Theme.accent : Theme.textSecondary)
                    .frame(width: LucinaTokens.Space.tap, height: LucinaTokens.Space.tap)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(pinned ? Text("Unpin from countdown") : Text("Pin to countdown"))
        }
        .accessibilityElement(children: .combine)
        .contextMenu { CountdownPinButton(event: event, pinned: pinned, toggle: toggle) }
    }
}

/// "Pin to countdown" / "Unpin from countdown", for an event's menus.
struct CountdownPinButton: View {
    let event: CountdownEvent
    let pinned: Bool
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            if pinned {
                Label("Unpin from countdown", systemImage: "pin.slash")
            } else {
                Label("Pin to countdown", systemImage: "pin")
            }
        }
    }
}

extension Place {
    /// The calendar event behind an event's place (a reminder, a widget, an agenda card), for the
    /// countdown widget; nil for a plain place.
    var countdownEvent: CountdownEvent? {
        guard kind == .event, let when else { return nil }
        return CountdownEvent(id: eventId ?? "place-\(title)-\(Int(when.timeIntervalSince1970))", title: title, start: when, end: end)
    }
}
