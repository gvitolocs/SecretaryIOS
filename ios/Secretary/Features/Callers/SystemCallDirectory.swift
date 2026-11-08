import CallKit
import Foundation
import UIKit

struct SystemCallDirectory: CallDirectoryManaging {
    static let extensionIdentifier = "tech.getsecretary.app.calldirectory"

    func enabledStatus() async -> CallerExtensionStatus {
        await withCheckedContinuation { continuation in
            CXCallDirectoryManager.sharedInstance.getEnabledStatusForExtension(withIdentifier: Self.extensionIdentifier) { status, error in
                if error != nil { continuation.resume(returning: .unavailable); return }
                switch status {
                case .enabled: continuation.resume(returning: .enabled)
                case .disabled: continuation.resume(returning: .disabled)
                default: continuation.resume(returning: .unknown)
                }
            }
        }
    }

    func reload() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            CXCallDirectoryManager.sharedInstance.reloadExtension(withIdentifier: Self.extensionIdentifier) { error in
                if let error { continuation.resume(throwing: error) } else { continuation.resume() }
            }
        }
    }

    func reloadMessage(for error: Error) -> String {
        if let managerError = error as? CXErrorCodeCallDirectoryManagerError, managerError.code == .extensionDisabled {
            return String(localized: "Turn on caller labels in Settings first.")
        }
        return String(localized: "Couldn't update the caller labels: \(error.localizedDescription)")
    }

    /// Straight to Call Blocking & Identification; the app's own Settings page when iOS says no.
    @MainActor func openSettings() async {
        let opened = await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
            CXCallDirectoryManager.sharedInstance.openSettings { error in continuation.resume(returning: error == nil) }
        }
        if !opened, let url = URL(string: UIApplication.openSettingsURLString) { await UIApplication.shared.open(url) }
    }
}
