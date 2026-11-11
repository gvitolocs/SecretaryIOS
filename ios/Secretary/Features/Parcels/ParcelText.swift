import Foundation
import SwiftUI

/// Words, symbols, colours, dates and the order for the Parcels screens.
enum ParcelText {
    // MARK: Carrier

    /// `postnord` → "PostNord"; an id this app doesn't know is shown with its dashes as spaces and capitalised.
    static func carrierName(_ id: String?) -> String? {
        guard let id = id?.trimmingCharacters(in: .whitespacesAndNewlines), !id.isEmpty else { return nil }
        let key = id.lowercased()
        switch key {
        case "postnord": return "PostNord"
        case "dao": return "DAO"
        case "bring-posten": return "Bring"
        case "ups": return "UPS"
        case "poste-italiane": return "Poste Italiane"
        case "budbee": return "Budbee"
        case "instabox": return "Instabox"
        default: break
        }
        if key.hasPrefix("gls") { return "GLS" }
        if key.hasPrefix("dhl") { return "DHL" }
        if key.hasPrefix("dpd") { return "DPD" }
        return key.replacingOccurrences(of: "-", with: " ").capitalized
    }

    // MARK: Parcel

    /// The label, or "PostNord 0035…" when it has none.
    static func title(_ parcel: Parcel) -> String {
        if let label = parcel.label?.trimmingCharacters(in: .whitespacesAndNewlines), !label.isEmpty { return label }
        if let carrier = carrierName(parcel.carrier) { return "\(carrier) \(parcel.number)" }
        return parcel.number
    }

    /// "PostNord · 0035…": what a labelled row shows under its title.
    static func subtitle(_ parcel: Parcel) -> String? {
        guard let label = parcel.label?.trimmingCharacters(in: .whitespacesAndNewlines), !label.isEmpty else { return nil }
        return [carrierName(parcel.carrier), parcel.number].compactMap { $0 }.joined(separator: " · ")
    }

    static func stage(_ parcel: Parcel) -> ParcelStage { parcel.status?.stage ?? .pending }

    /// Lucina still checks it.
    static func isActive(_ parcel: Parcel) -> Bool { parcel.state == .active }

    /// "Hourly · near you", "Hourly" or "Daily"; nil once checks have stopped.
    static func scheduleBadge(_ parcel: Parcel) -> String? {
        switch parcel.schedule {
        case .hourly: return parcel.near ? String(localized: "Hourly · near you") : String(localized: "Hourly")
        case .daily: return String(localized: "Daily")
        case .stopped, .other: return nil
        }
    }

    /// The newest thing known: the carrier's last update, the newest event, the last check, or when it was added.
    static func changed(_ parcel: Parcel) -> Date? {
        date(parcel.status?.lastUpdate) ?? date(parcel.events.first?.time) ?? date(parcel.lastChecked) ?? date(parcel.addedAt)
    }

    /// Active parcels first, each group newest change first; parcels that never changed keep the server's order.
    static func sorted(_ parcels: [Parcel]) -> [Parcel] {
        parcels.enumerated().sorted { a, b in
            let activeA = isActive(a.element), activeB = isActive(b.element)
            if activeA != activeB { return activeA }
            let dateA = changed(a.element), dateB = changed(b.element)
            switch (dateA, dateB) {
            case let (x?, y?) where x != y: return x > y
            case (.some, nil): return true
            case (nil, .some): return false
            default: return a.offset < b.offset
            }
        }.map(\.element)
    }

    /// The tracked parcels still on their way: "2 on the way" on the plugin card.
    static func onTheWay(_ parcels: [Parcel]) -> Int {
        parcels.filter { isActive($0) && stage($0) != .delivered }.count
    }

    /// The Plugins card's second line.
    static func pluginDetail(parcels: [Parcel]?) -> String {
        guard let parcels else { return String(localized: "Track deliveries with Lucina") }
        let count = onTheWay(parcels)
        return count > 0 ? String(localized: "\(count) on the way") : String(localized: "Track deliveries with Lucina")
    }

    // MARK: Stage

    static func stageName(_ stage: ParcelStage) -> String {
        switch stage {
        case .pending: return String(localized: "Waiting for the carrier")
        case .registered: return String(localized: "Registered")
        case .accepted: return String(localized: "Handed to the carrier")
        case .inTransit: return String(localized: "On its way")
        case .outForDelivery: return String(localized: "Out for delivery")
        case .delivered: return String(localized: "Delivered")
        case .customs: return String(localized: "In customs")
        case .failedAttempt: return String(localized: "Delivery attempt failed")
        case .readyForPickup: return String(localized: "Ready for pickup")
        case .returned: return String(localized: "Returned")
        case .exception: return String(localized: "Problem")
        case .unknown: return String(localized: "Unknown")
        }
    }

    static func stageSymbol(_ stage: ParcelStage) -> String {
        switch stage {
        case .pending: return "clock"
        case .registered: return "doc.text"
        case .accepted: return "shippingbox"
        case .inTransit: return "box.truck"
        case .outForDelivery: return "box.truck.fill"
        case .delivered: return "checkmark.circle.fill"
        case .customs: return "building.columns"
        case .failedAttempt: return "exclamationmark.triangle.fill"
        case .readyForPickup: return "mappin.and.ellipse"
        case .returned: return "arrow.uturn.backward"
        case .exception: return "exclamationmark.octagon.fill"
        case .unknown: return "questionmark.circle"
        }
    }

    static func stageColor(_ stage: ParcelStage) -> Color {
        switch stage {
        case .delivered: return Theme.positive
        case .outForDelivery, .readyForPickup: return Theme.accent
        case .accepted, .inTransit, .registered: return Theme.info
        case .customs, .failedAttempt: return Theme.warning
        case .returned, .exception: return Theme.negative
        case .pending, .unknown: return Theme.textSecondary
        }
    }

    // MARK: Dates

    private static let plain = ISO8601DateFormatter()
    private static let fractional: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    /// An RFC 3339 time from the API; nil for a missing or unreadable one.
    static func date(_ iso: String?) -> Date? {
        guard let iso, !iso.isEmpty else { return nil }
        return plain.date(from: iso) ?? fractional.date(from: iso)
    }

    /// `2026-09-28`, the carrier's expected delivery day, as a date in the phone's calendar.
    static func day(_ iso: String?) -> Date? {
        guard let iso, !iso.isEmpty else { return nil }
        return TaskDates.day(String(iso.prefix(10)))
    }

    /// "Sep 28" for the expected delivery day; the text as sent when it isn't a day.
    static func expected(_ iso: String?, locale: Locale = .current) -> String? {
        guard let iso, !iso.isEmpty else { return nil }
        guard let day = day(iso) else { return iso }
        return day.formatted(.dateTime.weekday(.abbreviated).day().month(.abbreviated).locale(locale))
    }

    /// "2 hours ago", "in 3 hours".
    static func relative(_ date: Date, locale: Locale = .current) -> String {
        date.formatted(.relative(presentation: .named).locale(locale))
    }

    /// "Sep 28, 05:33".
    static func timeline(_ date: Date, locale: Locale = .current) -> String {
        date.formatted(.dateTime.day().month(.abbreviated).hour().minute().locale(locale))
    }

    /// "AARHUS DEPOT · 2 hours ago": where it was last seen and when.
    static func lastSeen(_ parcel: Parcel) -> String? {
        let place = parcel.status?.location ?? parcel.events.first?.location
        let when = (date(parcel.status?.lastUpdate) ?? date(parcel.events.first?.time)).map { relative($0) }
        let parts = [place, when].compactMap { $0 }.filter { !$0.isEmpty }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }
}
