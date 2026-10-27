import DeviceActivity
import SwiftUI

// Screen Time inside Secretary. iOS renders this extension's view in the app; the numbers
// never leave it (the extension has no way to send them anywhere).

extension DeviceActivityReport.Context {
    static let totalActivity = Self("Total Activity")
}

@main
struct SecretaryScreenTimeReport: DeviceActivityReportExtension {
    var body: some DeviceActivityReportScene {
        TotalActivityReport { TotalActivityView(summary: $0) }
    }
}

struct ActivitySummary {
    struct App: Identifiable {
        let id: String
        let name: String
        let duration: TimeInterval
    }

    var total: TimeInterval
    var apps: [App]
}

struct TotalActivityReport: DeviceActivityReportScene {
    let context: DeviceActivityReport.Context = .totalActivity
    let content: (ActivitySummary) -> TotalActivityView

    func makeConfiguration(representing data: DeviceActivityResults<DeviceActivityData>) async -> ActivitySummary {
        var total: TimeInterval = 0
        var byApp: [String: (name: String, duration: TimeInterval)] = [:]
        for await device in data {
            for await segment in device.activitySegments {
                total += segment.totalActivityDuration
                for await category in segment.categories {
                    for await app in category.applications {
                        let name = app.application.localizedDisplayName ?? app.application.bundleIdentifier ?? "App"
                        let key = app.application.bundleIdentifier ?? name
                        byApp[key, default: (name, 0)].duration += app.totalActivityDuration
                    }
                }
            }
        }
        let apps = byApp.map { ActivitySummary.App(id: $0.key, name: $0.value.name, duration: $0.value.duration) }
            .sorted { $0.duration > $1.duration }
        return ActivitySummary(total: total, apps: Array(apps.prefix(8)))
    }
}

struct TotalActivityView: View {
    let summary: ActivitySummary

    private func text(_ seconds: TimeInterval) -> String {
        let formatter = DateComponentsFormatter()
        formatter.allowedUnits = seconds >= 3600 ? [.hour, .minute] : [.minute]
        formatter.unitsStyle = .abbreviated
        return formatter.string(from: seconds) ?? "0m"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Today").font(.caption.weight(.semibold)).foregroundStyle(.secondary).textCase(.uppercase)
            Text(text(summary.total)).font(.largeTitle.weight(.semibold).monospacedDigit())
            ForEach(summary.apps) { app in
                HStack {
                    Text(app.name).lineLimit(1)
                    Spacer()
                    Text(text(app.duration)).monospacedDigit().foregroundStyle(.secondary)
                }
                .font(.subheadline)
            }
        }
        .padding()
    }
}
