import Foundation

/// "Show adult plugins" (Settings → Plugins), per device like the web's switch (web/src/lib/adult.ts): NClient's card
/// and Pixiv's R-18 modes (`adult=1`). Adult content is owner-only on the server; this only hides it on this iPhone.
/// Default on: the owner is the only account.
enum AdultPlugins {
    static let key = "secretary.showAdultPlugins"

    static var isShown: Bool { UserDefaults.standard.object(forKey: key) as? Bool ?? true }
}
