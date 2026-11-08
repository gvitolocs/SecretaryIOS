import Foundation
import SafariServices

/// Native side of the Secretary Safari extension: receives `{type: "visit", domain, title,
/// ts}` from background.js and reports a `web_visit` event (domain and title only, never the
/// full URL) when the owner turned "Safari websites" on in Lucina's access.
final class SafariWebExtensionHandler: NSObject, NSExtensionRequestHandling {
    func beginRequest(with context: NSExtensionContext) {
        let item = context.inputItems.first as? NSExtensionItem
        let message = item?.userInfo?[SFExtensionMessageKey] as? [String: Any] ?? [:]
        Task {
            var ok = false
            if message["type"] as? String == "visit", let domain = message["domain"] as? String, !domain.isEmpty {
                let title = (message["title"] as? String).map { String($0.prefix(200)) } ?? ""
                let ts = (message["ts"] as? Double).map { Date(timeIntervalSince1970: $0 / 1000) } ?? Date()
                ok = await ExtensionReporter.send(type: "web_visit", source: "safari-ext", requires: "safari",
                                                  data: ["domain": String(domain.prefix(253)), "title": title], at: ts)
            } else if message["type"] as? String == "status" {
                ok = BridgeCredentials.read()?.sources.contains("safari") == true
            }
            let response = NSExtensionItem()
            response.userInfo = [SFExtensionMessageKey: ["ok": ok]]
            context.completeRequest(returningItems: [response], completionHandler: nil)
        }
    }
}
