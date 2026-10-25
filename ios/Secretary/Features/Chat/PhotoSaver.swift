import Photos
import UIKit

/// Saves a chat photo to the owner's library (add-only access): the original file when we have it,
/// else the picture on screen.
enum PhotoSaver {
    static func save(file: URL?, image: UIImage?) async -> Bool {
        let status = await PHPhotoLibrary.requestAuthorization(for: .addOnly)
        guard status == .authorized || status == .limited else { return false }
        if let file {
            do {
                try await PHPhotoLibrary.shared().performChanges {
                    PHAssetCreationRequest.forAsset().addResource(with: .photo, fileURL: file, options: nil)
                }
                return true
            } catch {
                // Not a format Photos takes as an original: fall through to the decoded picture.
            }
        }
        guard let image else { return false }
        do {
            try await PHPhotoLibrary.shared().performChanges {
                PHAssetChangeRequest.creationRequestForAsset(from: image)
            }
            return true
        } catch {
            return false
        }
    }
}
