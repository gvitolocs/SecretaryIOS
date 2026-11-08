import SwiftUI
import UserNotifications

/// Settings → Notifications: the same categories, quiet hours and Do Not
/// Disturb as the web app (saved on the server), plus this iPhone's switch
/// and iOS permission.
struct NotificationSettingsScreen: View {
    @Environment(NotificationCenterStore.self) private var store
    @Environment(\.openURL) private var openURL
    @State private var error: String?

    static let leadChoices = [5, 10, 15, 30, 60]

    var body: some View {
        Form {
            Section {
                switch store.authorization {
                case .authorized, .provisional, .ephemeral:
                    Label("Notifications are on for this iPhone.", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                case .denied:
                    Text("Notifications are off for Secretary in iOS Settings.")
                    Button("Open iOS Settings") {
                        if let url = URL(string: UIApplication.openNotificationSettingsURLString) { openURL(url) }
                    }
                default:
                    PermissionPrompt()
                }
                if store.state != nil {
                    Toggle("Notify this iPhone", isOn: Binding(get: { store.thisDeviceEnabled }, set: { on in
                        Task { error = await store.setThisDevice(enabled: on) }
                    }))
                    .accessibilityIdentifier("notifyThisDevice")
                }
            } header: {
                Text("This iPhone")
            } footer: {
                Text("Reminders and calendar events are scheduled on the iPhone, so they arrive even when Secretary is closed. Other notifications arrive when the app is open or refreshes in the background.")
            }

            if let settings = store.state?.settings {
                Section("What to notify") {
                    ForEach(NotificationCategory.allCases) { category in
                        Toggle(isOn: binding(for: category, in: settings)) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(category.title)
                                Text(category.detail).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        .accessibilityIdentifier("category-\(category.rawValue)")
                    }
                    Picker("Calendar: notify before an event", selection: Binding(get: { settings.calendarLeadMinutes }, set: { v in
                        save { $0.calendarLeadMinutes = v }
                    })) {
                        ForEach(Self.leadChoices, id: \.self) { m in Text("\(m) min").tag(m) }
                    }
                    .disabled(!settings.isOn(.calendar))
                }

                Section {
                    Picker("Do not disturb", selection: Binding(get: { dndChoice(settings) }, set: { v in setDnd(v) })) {
                        Text("Off").tag(0)
                        Text("1 hour").tag(1)
                        Text("Until tomorrow 8:00").tag(2)
                        if dndChoice(settings) == 3 { Text("On").tag(3) }
                    }
                    if let until = settings.dndUntil, Double(until) / 1000 > Date().timeIntervalSince1970 {
                        LabeledContent("Until") {
                            Text(Date(timeIntervalSince1970: Double(until) / 1000), format: .dateTime.weekday().hour().minute())
                        }
                    }
                } header: {
                    Text("Do not disturb")
                }

                Section {
                    Toggle("Hold notifications during quiet hours", isOn: Binding(get: { settings.quietHours.enabled }, set: { v in
                        save { $0.quietHours.enabled = v }
                    }))
                    DatePicker("From", selection: timeBinding(settings.quietHours.start) { s, v in s.quietHours.start = v },
                               displayedComponents: .hourAndMinute)
                        .disabled(!settings.quietHours.enabled)
                    DatePicker("To", selection: timeBinding(settings.quietHours.end) { s, v in s.quietHours.end = v },
                               displayedComponents: .hourAndMinute)
                        .disabled(!settings.quietHours.enabled)
                    Toggle("Reminders still come through", isOn: Binding(get: { settings.remindersBypassQuiet }, set: { v in
                        save { $0.remindersBypassQuiet = v }
                    }))
                    Toggle("Send a summary when quiet hours end", isOn: Binding(get: { settings.digest }, set: { v in
                        save { $0.digest = v }
                    }))
                    Toggle("Show message text in notifications", isOn: Binding(get: { settings.previews }, set: { v in
                        save { $0.previews = v }
                    }))
                } header: {
                    Text("Quiet hours")
                }
            } else {
                Section { ProgressView() }
            }

            Section {
                LabeledContent("Remote push (APNs)") {
                    Text(APNsAdapter.isEnabled && store.state?.apns?.enabled == true ? "On" : "Off")
                        .foregroundStyle(.secondary)
                }
            } footer: {
                Text("Remote push needs a paid Apple Developer account. Until then: add the Secretary website to the Home Screen and enable notifications there to get pushes when this app is closed.")
            }

            if let error {
                Section { Text(error).foregroundStyle(.red) }
            }
        }
        .navigationTitle("Notifications")
        .task {
            await store.refreshAuthorization()
            await store.loadSettings()
        }
    }

    private func save(_ change: @escaping (inout NotificationSettings) -> Void) {
        Task { error = await store.update(change) }
    }

    private func binding(for category: NotificationCategory, in settings: NotificationSettings) -> Binding<Bool> {
        Binding(get: { settings.isOn(category) }, set: { on in save { $0.set(category, on) } })
    }

    /// 0 off, 1 one hour, 2 until 8:00 tomorrow, 3 something else (set elsewhere).
    private func dndChoice(_ settings: NotificationSettings) -> Int {
        guard let until = settings.dndUntil, Double(until) / 1000 > Date().timeIntervalSince1970 else { return 0 }
        return 3
    }

    private func setDnd(_ choice: Int) {
        let until: Int64?
        switch choice {
        case 1: until = Int64((Date().timeIntervalSince1970 + 3600) * 1000)
        case 2: until = Int64(Self.tomorrowMorning().timeIntervalSince1970 * 1000)
        case 3: return
        default: until = nil
        }
        save { $0.dndUntil = until }
    }

    static func tomorrowMorning(from now: Date = Date(), calendar: Calendar = .current) -> Date {
        let tomorrow = calendar.date(byAdding: .day, value: 1, to: now) ?? now
        return calendar.date(bySettingHour: 8, minute: 0, second: 0, of: tomorrow) ?? tomorrow
    }

    private func timeBinding(_ hhmm: String, _ apply: @escaping (inout NotificationSettings, String) -> Void) -> Binding<Date> {
        Binding(
            get: {
                let minutes = NotificationRules.minutes(hhmm) ?? 0
                return Calendar.current.date(bySettingHour: minutes / 60, minute: minutes % 60, second: 0, of: Date()) ?? Date()
            },
            set: { date in
                let parts = Calendar.current.dateComponents([.hour, .minute], from: date)
                let value = String(format: "%02d:%02d", parts.hour ?? 0, parts.minute ?? 0)
                save { apply(&$0, value) }
            }
        )
    }
}
