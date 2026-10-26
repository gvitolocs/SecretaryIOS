import AppIntents
import Foundation

// Where a widget or control tap lands in the app. Widgets link to `secretary://open/<route>`;
// controls and widget buttons run `OpenSecretaryIntent`, which opens the app and goes to the same
// route. Routes are the app's web-style paths (`AppModel.open(route:)`), plus a few actions:
//   chat/compose      the chat with the keyboard up (Ask Lucina)
//   shopping/add      the Shopping tab's add field
//   weight/log        the quick weight entry sheet
//   gym/start         start (or resume) a workout
//   memos/record      start recording a voice memo
//   map/event/<id>    the in-app map for a calendar event
//   films/play/<id>   play a film where it stopped
enum WidgetRoute {
    static let scheme = "secretary"

    static func url(_ route: String) -> URL {
        let path = route.hasPrefix("/") ? String(route.dropFirst()) : route
        return URL(string: "\(scheme)://open/\(path)") ?? URL(string: "\(scheme)://open/chat")!
    }

    /// The route of a widget URL, nil for any other URL.
    static func route(from url: URL) -> String? {
        guard url.scheme == scheme, url.host == "open" else { return nil }
        // Still percent-encoded: an id may hold a `/` (the app decodes each segment itself).
        let path = url.path(percentEncoded: true)
        return path.count <= 1 ? "chat" : String(path.dropFirst())
    }

    static func segment(_ id: String) -> String {
        id.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-_.~"))) ?? id
    }
}

/// Set by the app at launch; nil in the widget extension, which never runs `OpenSecretaryIntent`
/// itself (`openAppWhenRun` runs it in the app).
enum WidgetRouter {
    @MainActor static var open: ((String) -> Void)?
}

/// Opens Secretary at a route. Used by the Control Center / Lock Screen controls and by widget
/// buttons that need the app (the compose field, the weight sheet, the recorder).
struct OpenSecretaryIntent: AppIntent {
    static let title: LocalizedStringResource = "Open Secretary"
    static let isDiscoverable = false
    static let openAppWhenRun = true

    @Parameter(title: "Route")
    var route: String

    init() { route = "chat" }

    init(_ route: String) { self.route = route }

    @MainActor
    func perform() async throws -> some IntentResult {
        WidgetRouter.open?(route)
        return .result()
    }
}
