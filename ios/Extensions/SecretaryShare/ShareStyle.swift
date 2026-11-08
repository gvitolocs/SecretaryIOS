import SwiftUI

/// Components.swift contains UIApplication.shared keyboard handling, which is unavailable
/// to app extensions. Keep the extension's small palette on the same generated tokens.
enum ShareStyle {
    static let page = LucinaTokens.Colors.bg
    static let card = LucinaTokens.Colors.card
    static let accent = LucinaTokens.Colors.accent
    static let button = LucinaTokens.Colors.accentFill
    static let text = LucinaTokens.Colors.text
    static let radius = LucinaTokens.Radius.card
}
