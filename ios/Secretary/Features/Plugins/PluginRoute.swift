import Foundation

/// The plugins' web paths (`/instagram/<username>`, `/pixiv/<id>`) for `AppModel.open(route:)`.
enum PluginRoute {
    /// The path segment at `index` (0 is the plugin), percent-decoded; the query is ignored.
    static func segment(_ path: String, _ index: Int) -> String? {
        let bare = path.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first.map(String.init) ?? ""
        let parts = bare.split(separator: "/").map { String($0).removingPercentEncoding ?? String($0) }
        return parts.indices.contains(index) && !parts[index].isEmpty ? parts[index] : nil
    }

    static func instagramUser(_ path: String) -> String? {
        guard let name = segment(path, 1), case let .profile(username) = InstagramText.parse(name) else { return nil }
        return username
    }

    static func pixivWork(_ path: String) -> String? {
        guard let id = segment(path, 1), (1...12).contains(id.count), id.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        return id
    }
}

/// Launch arguments for the device screenshots of the plugin screens (`ios/scripts/plugin-shots.sh`). Read only in
/// DEBUG and SCREENSHOTS builds; nil in the App Store build.
enum PluginScreenshotHooks {
    static func value(_ key: String) -> String? {
        #if DEBUG || SCREENSHOTS
        UserDefaults.standard.string(forKey: key)
        #else
        nil
        #endif
    }

    /// `-SecretaryInstagramTab highlights`: the profile opens on that tab.
    static var instagramTab: IgTab? { value("SecretaryInstagramTab").flatMap(IgTab.init(rawValue:)) }
    /// `-SecretaryInstagramHighlight first`: the first highlight opens once the list is in; `story` also opens its first
    /// item in the full-screen story viewer.
    static var opensFirstHighlight: Bool { ["first", "story"].contains(value("SecretaryInstagramHighlight")) }
    static var opensHighlightStory: Bool { value("SecretaryInstagramHighlight") == "story" }
    /// `-SecretaryPixivOpen first`: the first work of the list opens once it is in.
    static var opensFirstPixivWork: Bool { value("SecretaryPixivOpen") == "first" }
    /// `-SecretaryComputerFullScreen page`: once the stream is live, click Cua's own full-screen button (the path the
    /// owner taps); `page-twice` clicks it again 20 s later (back out). Watch-only: nothing reaches her desktop.
    static var computerFullScreen: String? { value("SecretaryComputerFullScreen") }
}
