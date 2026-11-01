#if DEBUG || SCREENSHOTS
import UIKit

/// `-SecretarySnapshots 9,30,33`: the key window as PNGs in Documents (`snapshot-<seconds>.png`) that many seconds after
/// launch, for device screenshots when no USB developer tunnel is there (the phone on Wi-Fi). Copy them back with
/// `xcrun devicectl device copy from --domain-type appDataContainer --domain-identifier tech.getsecretary.app`.
@MainActor enum WindowSnapshots {
    static func schedule() {
        guard let list = UserDefaults.standard.string(forKey: "SecretarySnapshots") else { return }
        for seconds in list.split(separator: ",").compactMap({ Double($0.trimmingCharacters(in: .whitespaces)) }) {
            Task { @MainActor in
                try? await Task.sleep(for: .seconds(seconds))
                capture(name: "snapshot-\(Int(seconds))")
            }
        }
    }

    private static func capture(name: String) {
        let windows = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.flatMap(\.windows)
        guard let window = windows.first(where: \.isKeyWindow) ?? windows.first,
              let folder = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else { return }
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in
            _ = window.drawHierarchy(in: window.bounds, afterScreenUpdates: false)
        }
        try? image.pngData()?.write(to: folder.appendingPathComponent("\(name).png"))
    }
}
#endif
