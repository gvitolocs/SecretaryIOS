import CoreLocation
import Foundation

/// One location fix for a new memo, so the server can prefer the calendar event held where you
/// are (a lecture hall, a meeting room). Never asks for permission: only when Secretary already
/// has location access. Gives up after a few seconds.
enum MemoLocation {
    @MainActor
    static func fix(timeout: Duration = .seconds(8)) async -> MemoGeoFix? {
        let status = CLLocationManager().authorizationStatus
        guard status == .authorizedWhenInUse || status == .authorizedAlways else { return nil }
        return await withTaskGroup(of: MemoGeoFix?.self) { group in
            group.addTask {
                do {
                    for try await update in CLLocationUpdate.liveUpdates() {
                        if let l = update.location, l.horizontalAccuracy >= 0, l.horizontalAccuracy <= 200 {
                            return MemoGeoFix(lat: l.coordinate.latitude, lon: l.coordinate.longitude, accuracyM: l.horizontalAccuracy)
                        }
                    }
                } catch {}
                return nil
            }
            group.addTask {
                try? await Task.sleep(for: timeout)
                return nil
            }
            let first = await group.next() ?? nil
            group.cancelAll()
            return first
        }
    }
}
