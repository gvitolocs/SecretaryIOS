import Foundation

/// Words and numbers for a result.
enum CallerText {
    /// 0.86 -> "86%"; a missing value is a dash.
    static func percent(_ value: Double?) -> String {
        guard let value, value.isFinite else { return "–" }
        return "\(Int((min(max(value, 0), 1) * 100).rounded()))%"
    }

    static func kind(_ kind: String?) -> String? {
        switch kind {
        case "person": return String(localized: "Person")
        case "business": return String(localized: "Business")
        case "telemarketing": return String(localized: "Telemarketing")
        case "scam": return String(localized: "Scam")
        case "unknown": return String(localized: "Unknown")
        default: return nil
        }
    }

    static func source(_ source: String?) -> String? {
        switch source {
        case "jev": return String(localized: "Source: Jev")
        case "rules": return String(localized: "Source: rules (Jev was unavailable)")
        case "contact": return String(localized: "Source: contact")
        default: return nil
        }
    }

    static func numberType(_ type: String?) -> String? {
        switch type {
        case "mobile": return String(localized: "Mobile")
        case "landline": return String(localized: "Landline")
        case "non_geographic": return String(localized: "Non-geographic")
        case "freephone": return String(localized: "Freephone")
        case "premium": return String(localized: "Premium rate")
        default: return nil
        }
    }

    /// "Denmark · Mobile".
    static func place(_ result: CallerResult) -> String? {
        let parts = [result.countryName ?? result.country, numberType(result.type)].compactMap { $0 }.filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}
