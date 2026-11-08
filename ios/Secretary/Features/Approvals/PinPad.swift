import SwiftUI

/// Six dots and a number pad. Calls `onComplete` with the 6 digits as soon as the sixth is typed, then clears.
struct PinPad: View {
    var title: String
    var message: String?
    var busy = false
    var onComplete: (String) -> Void

    @State private var digits = ""

    var body: some View {
        VStack(spacing: 14) {
            Text(title).font(.headline)
            HStack(spacing: 14) {
                ForEach(0 ..< 6, id: \.self) { index in
                    Circle()
                        .fill(index < digits.count ? Theme.accent : Color.clear)
                        .overlay(Circle().stroke(Theme.accent.opacity(0.6), lineWidth: 1.5))
                        .frame(width: 14, height: 14)
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(digits.count) of 6 digits")
            Text(message ?? " ")
                .font(.footnote)
                .foregroundStyle(Theme.negative)
                .multilineTextAlignment(.center)
                .frame(minHeight: 18)
                .accessibilityIdentifier("pinMessage")
            VStack(spacing: 10) {
                ForEach([["1", "2", "3"], ["4", "5", "6"], ["7", "8", "9"], ["", "0", "⌫"]], id: \.self) { row in
                    HStack(spacing: 18) {
                        ForEach(row, id: \.self) { key in keyView(key) }
                    }
                }
            }
            .opacity(busy ? 0.4 : 1)
            .disabled(busy)
        }
    }

    @ViewBuilder private func keyView(_ key: String) -> some View {
        if key.isEmpty {
            Color.clear.frame(width: 64, height: 46)
        } else {
            Button {
                press(key)
            } label: {
                Group {
                    if key == "⌫" { Image(systemName: "delete.left") } else { Text(key).font(.title2.weight(.medium)) }
                }
                .frame(width: 64, height: 46)
                .background(Theme.field, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            }
            .buttonStyle(.plain)
            .accessibilityLabel(key == "⌫" ? "Delete" : key)
            .accessibilityIdentifier(key == "⌫" ? "pinDelete" : "pinKey\(key)")
        }
    }

    private func press(_ key: String) {
        if key == "⌫" {
            if !digits.isEmpty { digits.removeLast() }
            return
        }
        guard digits.count < 6 else { return }
        digits.append(key)
        if digits.count == 6 {
            let entered = digits
            digits = ""
            onComplete(entered)
        }
    }
}
