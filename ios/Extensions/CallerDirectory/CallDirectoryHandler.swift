import CallKit
import Foundation

/// The Call Directory extension: labels for numbers Lucina has screened ("Telemarketing 86%"), shown on the
/// incoming-call screen. The app saves the list (`CallerDirectoryStore`) and asks iOS to reload this extension;
/// iOS runs it only then, never during a call, and keeps what it adds.
///
/// Extensions have a tight memory limit, so the list is read once (a few hundred KB at most) and handed to
/// CallKit one entry at a time. CallKit wants the numbers strictly ascending; `CallerDirectoryList.decode`
/// guarantees that even if the file is damaged.
final class CallDirectoryHandler: CXCallDirectoryProvider, CXCallDirectoryExtensionContextDelegate {
    override func beginRequest(with context: CXCallDirectoryExtensionContext) {
        context.delegate = self
        // Full reload: whatever was added before is replaced, whether iOS asks incrementally or not.
        if context.isIncremental { context.removeAllIdentificationEntries() }
        // No list yet (or an unreadable one) is not an error: there is just nothing to show.
        if let snapshot = CallerDirectoryStore.read() {
            for entry in snapshot.entries {
                context.addIdentificationEntry(withNextSequentialPhoneNumber: CXCallDirectoryPhoneNumber(entry.number), label: entry.label)
            }
        }
        context.completeRequest()
    }

    func requestFailed(for extensionContext: CXCallDirectoryExtensionContext, withError error: Error) {
        // Nothing to clean up: the next reload starts from scratch.
    }
}
