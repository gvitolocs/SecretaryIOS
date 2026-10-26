import Foundation
import Intents
import UIKit

enum LucinaDonations {
    static var avatar: UIImage? { UIImage(named: "LucinaShareIcon") ?? UIImage(named: "AppIcon") }

    /// `completion` gets the donation's outcome (the share self-test logs it); it is not called when a daily donation is skipped.
    static func donate(daily: Bool = false, completion: (@Sendable (Error?) -> Void)? = nil) {
        let defaults = UserDefaults(suiteName: ShareRequestBuilder.group)
        if daily, let date = defaults?.object(forKey: "share.lastDonation") as? Date,
           Date().timeIntervalSince(date) < 24 * 60 * 60 { return }
        let image = avatar?.pngData().map { INImage(imageData: $0) }
        let person = INPerson(personHandle: INPersonHandle(value: "lucina", type: .unknown),
                              nameComponents: nil, displayName: "Lucina", image: image,
                              contactIdentifier: nil, customIdentifier: "lucina")
        let intent = INSendMessageIntent(recipients: [person], outgoingMessageType: .outgoingMessageText,
                                         content: nil, speakableGroupName: INSpeakableString(spokenPhrase: "Lucina"),
                                         conversationIdentifier: "lucina", serviceName: nil, sender: nil, attachments: nil)
        let interaction = INInteraction(intent: intent, response: nil)
        interaction.direction = .outgoing
        interaction.donate { error in
            if error == nil { defaults?.set(Date(), forKey: "share.lastDonation") }
            completion?(error)
        }
    }
}
