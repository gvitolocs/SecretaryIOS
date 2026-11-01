#if canImport(FamilyControls) && canImport(DeviceActivity)
import DeviceActivity
import FamilyControls
import SwiftUI

/// Screen Time in Secretary: today's report (drawn by the report extension; the numbers stay
/// on the phone) and one limit that, when passed, tells Lucina its name.
struct ScreenTimeView: View {
    @AppStorage("secretary.screentime.label") private var label = "Social"
    @AppStorage("secretary.screentime.minutes") private var minutes = 60
    @AppStorage("secretary.screentime.selection") private var selectionData = Data()
    @State private var selection = FamilyActivitySelection()
    @State private var picking = false
    @State private var message: String?

    private var today: DateInterval {
        let start = Calendar.current.startOfDay(for: Date())
        return DateInterval(start: start, end: Date())
    }

    var body: some View {
        List {
            Section("Today") {
                DeviceActivityReport(.init("Total Activity"),
                                     filter: DeviceActivityFilter(segment: .daily(during: today), users: .all, devices: .init([.iPhone])))
                    .frame(minHeight: 260)
            }
            Section {
                TextField("Name for Lucina", text: $label)
                Stepper(value: $minutes, in: 5...600, step: 5) {
                    Text("Limit: \(minutes) min a day")
                }
                Button {
                    picking = true
                } label: {
                    LabeledContent("Apps and websites") {
                        Text("\(selection.applicationTokens.count + selection.categoryTokens.count + selection.webDomainTokens.count)")
                    }
                }
                Button("Start watching") { startMonitoring() }
                    .disabled(selection.applicationTokens.isEmpty && selection.categoryTokens.isEmpty && selection.webDomainTokens.isEmpty)
                Button("Stop watching", role: .destructive) {
                    DeviceActivityCenter().stopMonitoring()
                    message = String(localized: "Lucina no longer gets Screen Time notes.")
                }
                if let message { Text(message).font(.footnote).foregroundStyle(.secondary) }
            } header: {
                Text("A limit for Lucina")
            } footer: {
                Text("When the chosen apps pass the limit today, Lucina gets a note with the name above. iOS never shares which apps or how long.")
            }
        }
        .navigationTitle("Screen Time")
        .familyActivityPicker(isPresented: $picking, selection: $selection)
        .onAppear {
            if let saved = try? JSONDecoder().decode(FamilyActivitySelection.self, from: selectionData) { selection = saved }
        }
        .onChange(of: selection) { _, value in
            selectionData = (try? JSONEncoder().encode(value)) ?? Data()
        }
    }

    private func startMonitoring() {
        let name = "\(label.trimmingCharacters(in: .whitespaces).isEmpty ? "Apps" : label) over \(minutes) min"
        let event = DeviceActivityEvent(applications: selection.applicationTokens, categories: selection.categoryTokens,
                                        webDomains: selection.webDomainTokens, threshold: DateComponents(minute: minutes))
        let schedule = DeviceActivitySchedule(intervalStart: DateComponents(hour: 0, minute: 0),
                                              intervalEnd: DateComponents(hour: 23, minute: 59), repeats: true)
        do {
            let center = DeviceActivityCenter()
            center.stopMonitoring()
            try center.startMonitoring(DeviceActivityName("secretary.daily"), during: schedule,
                                       events: [DeviceActivityEvent.Name(name): event])
            message = String(localized: "Watching. Lucina hears \u{201C}\(name)\u{201D} when it happens.")
        } catch {
            message = error.localizedDescription
        }
    }
}
#endif
