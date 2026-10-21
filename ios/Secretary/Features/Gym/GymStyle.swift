import SwiftUI

// The original LucinaGym look (LucinaGym/Components.swift), used only inside the Gym tab.
// The Gym tab renders in light mode, as the original did.

enum GymPalette {
    /// #F5F4EE
    static let background = Color(red: 0.96, green: 0.955, blue: 0.935)
    /// #214D3D
    static let forest = Color(red: 0.13, green: 0.30, blue: 0.24)
    /// #1F2621
    static let ink = Color(red: 0.12, green: 0.15, blue: 0.13)
    /// #737A70
    static let muted = Color(red: 0.45, green: 0.48, blue: 0.44)
    /// #D4ED91
    static let lime = Color(red: 0.83, green: 0.93, blue: 0.57)
}

struct GymCard<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 16) { content }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(20)
            .background(.white, in: RoundedRectangle(cornerRadius: 22))
            .overlay(RoundedRectangle(cornerRadius: 22).stroke(GymPalette.ink.opacity(0.04)))
    }
}

struct GymEyebrow: View {
    let text: String

    var body: some View {
        Text(text.uppercased())
            .font(.system(size: 10, weight: .bold, design: .rounded))
            .tracking(2)
            .foregroundStyle(GymPalette.muted)
    }
}

struct GymMetric: View {
    let value: String
    let label: String

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(value)
                .font(.system(size: 23, weight: .semibold, design: .rounded))
                .lineLimit(1)
                .minimumScaleFactor(0.6)
            GymEyebrow(text: label)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

struct GymPrimaryButton: View {
    let title: String
    var icon: String = "plus"
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: icon)
                .font(.headline)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 15)
        }
        .buttonStyle(.plain)
        .foregroundStyle(.white)
        .background(GymPalette.forest, in: RoundedRectangle(cornerRadius: 16))
    }
}

struct GymEmptyCard: View {
    let icon: String
    let title: String
    let detail: String

    var body: some View {
        GymCard {
            Image(systemName: icon).font(.title).foregroundStyle(GymPalette.forest).accessibilityHidden(true)
            Text(title).font(.title3.bold())
            Text(detail).font(.subheadline).foregroundStyle(GymPalette.muted)
        }
        .accessibilityElement(children: .combine)
    }
}

extension View {
    /// The Gym tab's environment: light mode, ink text, forest tint (as the original app root).
    func gymStyle() -> some View {
        environment(\.colorScheme, .light)
            .foregroundStyle(GymPalette.ink)
            .tint(GymPalette.forest)
    }

    /// The original's rounded, monospaced-digit field font.
    func gymFieldFont() -> some View {
        font(.system(.body, design: .rounded).monospacedDigit())
    }
}
