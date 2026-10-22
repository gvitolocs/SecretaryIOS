import SwiftUI

/// Lucina's status in the navigation bar: a faint green dot when online, orange when Lucina is down,
/// red when the server is unreachable. It is the app's only connection indicator.
struct StatusDot: View {
    @Environment(HealthStore.self) private var health

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: size, height: size)
            .opacity(health.status == .online ? 0.45 : 1)
            .padding(8)
            .contentShape(Rectangle())
            .animation(.easeInOut(duration: 0.3), value: health.status)
            .accessibilityElement()
            .accessibilityLabel("Lucina status")
            .accessibilityValue(health.summary)
            .accessibilityIdentifier("lucinaStatus")
    }

    /// Small and quiet while everything works; a little larger when something needs attention.
    private var size: CGFloat { health.status == .online || health.status == .unknown ? 6 : 8 }

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
            ToolbarItem(placement: .topBarTrailing) { NotificationBell() }
        }
    }
}
