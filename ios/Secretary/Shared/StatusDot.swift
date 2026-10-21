import SwiftUI

/// Lucina's status in the navigation bar: green online, orange Lucina down, red server down.
struct StatusDot: View {
    @Environment(HealthStore.self) private var health

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 10, height: 10)
            .overlay(Circle().strokeBorder(.background, lineWidth: 1.5))
            .padding(8)
            .contentShape(Rectangle())
            .accessibilityElement()
            .accessibilityLabel("Lucina status")
            .accessibilityValue(health.summary)
            .accessibilityIdentifier("lucinaStatus")
    }

    private var color: Color {
        switch health.status {
        case .unknown: return .gray
        case .online: return .green
        case .lucinaDown: return .orange
        case .serverDown, .hostNotFound: return .red
        }
    }
}

extension View {
    /// Adds the Lucina status dot to the leading side of the navigation bar.
    func lucinaStatusToolbar() -> some View {
        toolbar {
            ToolbarItem(placement: .topBarLeading) { StatusDot() }
        }
    }
}
