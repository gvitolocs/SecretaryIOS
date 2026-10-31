import SwiftUI

/// The bell in the navigation bar, with the unread count; opens the notification center.
struct NotificationBell: View {
    @Environment(NotificationCenterStore.self) private var store
    @State private var showing = false

    var body: some View {
        Button {
            showing = true
        } label: {
            // The red dot of `bell.badge` (a number would be clipped by the bar button).
            Image(systemName: store.unread > 0 ? "bell.badge" : "bell")
                .symbolRenderingMode(store.unread > 0 ? .palette : .monochrome)
                .foregroundStyle(store.unread > 0 ? Color.red : Color.primary, Color.primary)
        }
        .accessibilityLabel(store.unread > 0 ? Text("Notifications, \(store.unread) unread") : Text("Notifications"))
        .accessibilityIdentifier("notificationBell")
        .sheet(isPresented: $showing) {
            NotificationCenterView()
        }
    }
}

/// The in-app notification center: the server inbox, newest first, grouped by day.
struct NotificationCenterView: View {
    @Environment(NotificationCenterStore.self) private var store
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    private var today: [AppNotification] { store.items.filter { Calendar.current.isDateInToday($0.date) } }
    private var earlier: [AppNotification] { store.items.filter { !Calendar.current.isDateInToday($0.date) } }

    var body: some View {
        NavigationStack {
            List {
                if store.authorization == .notDetermined {
                    Section {
                        PermissionPrompt()
                    }
                }
                if !today.isEmpty {
                    Section("Today") { rows(today) }
                }
                if !earlier.isEmpty {
                    Section("Earlier") { rows(earlier) }
                }
            }
            .overlay {
                if store.loaded && store.items.isEmpty {
                    ContentUnavailableView {
                        Label("Nothing new", systemImage: "bell")
                    } description: {
                        Text("Lucina's messages, reminders and upcoming events will show up here.")
                    }
                } else if !store.loaded {
                    ProgressView()
                }
            }
            .refreshable { await store.refresh(alert: false) }
            .navigationTitle("Notifications")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
                ToolbarItem(placement: .secondaryAction) {
                    Button("Decisions", systemImage: "checklist") {
                        dismiss()
                        app.openDecision(nil)
                    }
                }
                ToolbarItem(placement: .primaryAction) {
                    Button("Mark all read") { Task { await store.markAllRead() } }
                        .disabled(store.unread == 0)
                        .accessibilityIdentifier("markAllRead")
                }
            }
            .task {
                await store.refreshAuthorization()
                await store.refresh(alert: false)
            }
        }
    }

    @ViewBuilder
    private func rows(_ list: [AppNotification]) -> some View {
        ForEach(list) { n in
            Button {
                Task {
                    let tab = await store.open(n)
                    app.selectedTab = tab
                    if Place.fromRoute(n.route) != nil { app.open(route: n.route) }
                    if NotificationRoute.opensDealsMap(n.route) { app.showDealsMap = true }
                    if let memo = NotificationRoute.memo(n.route) { app.openMemo(memo.isEmpty ? nil : memo) }
                    if let server = NotificationRoute.server(n.route) { app.openServer(server) }
                    if let calendar = NotificationRoute.calendar(n.route) { app.openCalendar(calendar) }
                    dismiss()
                    if let decision = n.decisionId ?? NotificationRoute.decision(n.route) { app.openDecision(decision) }
                }
            } label: {
                NotificationRow(notification: n)
            }
            .buttonStyle(.plain)
            .swipeActions {
                if !n.read {
                    Button("Mark read", systemImage: "envelope.open") { Task { await store.markRead([n.id]) } }
                        .tint(.accentColor)
                }
            }
        }
    }
}

struct NotificationRow: View {
    let notification: AppNotification

    private var channel: String? {
        notification.channel.map { TimelineChannel(raw: $0).label }
    }

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: notification.category.symbol)
                .font(.body.weight(.semibold))
                .foregroundStyle(tint)
                .frame(width: 36, height: 36)
                .background(RoundedRectangle(cornerRadius: 10).fill(tint.opacity(0.14)))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(notification.displayTitle).font(.subheadline.weight(.semibold))
                if !notification.body.isEmpty {
                    Text(notification.body).font(.subheadline).foregroundStyle(.secondary).lineLimit(3)
                }
                HStack(spacing: 4) {
                    Text(notification.date, format: .relative(presentation: .named))
                    if let channel { Text("· via \(channel)") }
                }
                .font(.caption)
                .foregroundStyle(.tertiary)
            }
            Spacer(minLength: 0)
            if !notification.read {
                Circle().fill(Color.accentColor).frame(width: 9, height: 9).padding(.top, 6)
                    .accessibilityLabel(Text("Unread"))
            }
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
    }

    private var tint: Color {
        switch notification.category {
        case .lucinaMessage: return .pink
        case .reminder: return .orange
        case .calendar: return .teal
        case .deviceConfirm: return .purple
        case .shopping: return .green
        case .presence, .system: return .gray
        }
    }
}

/// Asks iOS for notification permission, only when tapped.
struct PermissionPrompt: View {
    @Environment(NotificationCenterStore.self) private var store

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Never miss Lucina", systemImage: "bell.badge").font(.headline)
            Text("Turn on notifications for reminders, upcoming events and Lucina's messages.")
                .font(.subheadline).foregroundStyle(.secondary)
            Button("Enable notifications") { Task { await store.requestAuthorization() } }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("enableNotifications")
        }
        .padding(.vertical, 4)
    }
}
