import SwiftUI
import UIKit
import CoreLocation
import Speech
import UniformTypeIdentifiers

// MARK: - The text view that takes pastes of anything

/// SwiftUI's TextField can't receive pasted images or files, so the composer is a UITextView: a paste with
/// images or files goes to the attachment tray; plain text and links paste as text. Return sends.
struct ComposerTextView: UIViewRepresentable {
    @Binding var text: String
    @Binding var focused: Bool
    var placeholder: String
    var maxLines = 6
    var onSubmit: () -> Void
    var onPaste: ([NSItemProvider]) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> PastingTextView {
        let view = PastingTextView()
        view.delegate = context.coordinator
        view.font = .preferredFont(forTextStyle: .body)
        view.adjustsFontForContentSizeCategory = true
        view.backgroundColor = .clear
        view.textColor = .label
        view.isScrollEnabled = false
        view.returnKeyType = .send
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.accessibilityIdentifier = "chatComposer"
        view.placeholder.text = placeholder
        view.onPaste = onPaste
        view.text = text
        view.updatePlaceholder()
        return view
    }

    func updateUIView(_ view: PastingTextView, context: Context) {
        context.coordinator.parent = self
        view.onPaste = onPaste
        if view.text != text {
            view.text = text
            view.updatePlaceholder()
            view.invalidateIntrinsicContentSize()
        }
        view.placeholder.text = placeholder
        if focused, !view.isFirstResponder {
            DispatchQueue.main.async { view.becomeFirstResponder() }
        } else if !focused, view.isFirstResponder {
            DispatchQueue.main.async { view.resignFirstResponder() }
        }
    }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: PastingTextView, context: Context) -> CGSize? {
        let width = max(40, proposal.width ?? 240)
        let fitting = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude))
        let lineHeight = uiView.font?.lineHeight ?? 20
        let maxHeight = ceil(lineHeight * CGFloat(maxLines))
        let scrolls = fitting.height > maxHeight
        if uiView.isScrollEnabled != scrolls { uiView.isScrollEnabled = scrolls }
        return CGSize(width: width, height: max(ceil(lineHeight), min(fitting.height, maxHeight)))
    }

    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: ComposerTextView
        init(_ parent: ComposerTextView) { self.parent = parent }

        func textViewDidChange(_ textView: UITextView) {
            parent.text = textView.text
            (textView as? PastingTextView)?.updatePlaceholder()
            textView.invalidateIntrinsicContentSize()
        }

        func textView(_ textView: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool {
            // Return sends (as the old single-field composer did); pasted multi-line text still pastes.
            if text == "\n" {
                parent.onSubmit()
                return false
            }
            return true
        }

        func textViewDidBeginEditing(_ textView: UITextView) {
            if !parent.focused { parent.focused = true }
        }

        func textViewDidEndEditing(_ textView: UITextView) {
            if parent.focused { parent.focused = false }
        }
    }
}

final class PastingTextView: UITextView {
    var onPaste: (([NSItemProvider]) -> Void)?
    let placeholder = UILabel()

    override init(frame: CGRect, textContainer: NSTextContainer?) {
        super.init(frame: frame, textContainer: textContainer)
        placeholder.font = .preferredFont(forTextStyle: .body)
        placeholder.adjustsFontForContentSizeCategory = true
        placeholder.textColor = .placeholderText
        placeholder.isAccessibilityElement = false
        placeholder.translatesAutoresizingMaskIntoConstraints = false
        addSubview(placeholder)
        NSLayoutConstraint.activate([placeholder.leadingAnchor.constraint(equalTo: leadingAnchor),
                                     placeholder.topAnchor.constraint(equalTo: topAnchor)])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func updatePlaceholder() {
        placeholder.isHidden = !text.isEmpty
        accessibilityLabel = placeholder.text
    }

    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        if action == #selector(paste(_:)) {
            let board = UIPasteboard.general
            return board.hasStrings || board.hasImages || board.hasURLs || board.numberOfItems > 0
        }
        return super.canPerformAction(action, withSender: sender)
    }

    override func paste(_ sender: Any?) {
        let providers = UIPasteboard.general.itemProviders
        if AttachmentLoader.shouldAttach(providers), let onPaste {
            onPaste(providers)
        } else {
            super.paste(sender)
        }
    }
}

// MARK: - Camera

/// UIImagePickerController for a photo or a video from the camera.
struct CameraPicker: UIViewControllerRepresentable {
    var onImage: (UIImage) -> Void
    var onMovie: (URL) -> Void
    @Environment(\.dismiss) private var dismiss

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.mediaTypes = [UTType.image.identifier, UTType.movie.identifier]
        picker.videoQuality = .typeHigh
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIImagePickerController, context: Context) {}

    final class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let parent: CameraPicker
        init(_ parent: CameraPicker) { self.parent = parent }

        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            if let url = info[.mediaURL] as? URL {
                parent.onMovie(url)
            } else if let image = (info[.editedImage] ?? info[.originalImage]) as? UIImage {
                parent.onImage(image)
            }
            parent.dismiss()
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            parent.dismiss()
        }
    }
}

// MARK: - Current location, once

@MainActor
final class OneShotLocation: NSObject, CLLocationManagerDelegate {
    private let manager = CLLocationManager()
    private var authorization: CheckedContinuation<Void, Never>?
    private var fix: CheckedContinuation<CLLocation?, Never>?

    override init() {
        super.init()
        manager.delegate = self
        manager.desiredAccuracy = kCLLocationAccuracyHundredMeters
    }

    /// The current location with a place name when one is known; nil without permission or a fix in 15 s.
    func current() async -> ShareMeta.Location? {
        if manager.authorizationStatus == .notDetermined {
            await withCheckedContinuation { continuation in
                authorization = continuation
                manager.requestWhenInUseAuthorization()
            }
        }
        guard [.authorizedWhenInUse, .authorizedAlways].contains(manager.authorizationStatus) else { return nil }
        let location: CLLocation? = await withCheckedContinuation { continuation in
            fix = continuation
            manager.requestLocation()
            Task { @MainActor [weak self] in
                try? await Task.sleep(for: .seconds(15))
                self?.resolveFix(nil)
            }
        }
        guard let location else { return nil }
        let placemark = try? await CLGeocoder().reverseGeocodeLocation(location).first
        let name = [placemark?.name, placemark?.locality].compactMap { $0 }.reduce(into: [String]()) { names, part in
            if !names.contains(part) { names.append(part) }
        }.joined(separator: ", ")
        return ShareMeta.Location(lat: location.coordinate.latitude, lon: location.coordinate.longitude, name: name.isEmpty ? nil : name)
    }

    private func resolveFix(_ location: CLLocation?) {
        fix?.resume(returning: location)
        fix = nil
    }

    nonisolated func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        let status = manager.authorizationStatus
        MainActor.assumeIsolated {
            guard status != .notDetermined else { return }
            authorization?.resume()
            authorization = nil
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        let last = locations.last
        MainActor.assumeIsolated { resolveFix(last) }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        MainActor.assumeIsolated { resolveFix(nil) }
    }
}

// MARK: - On-device audio transcripts

/// Transcribes an attached audio file on the iPhone (never on Apple's servers), so Lucina gets the words
/// without running Whisper. Nil when speech recognition isn't allowed, on-device recognition isn't
/// available for the language, or it takes longer than 60 s.
enum AudioTranscriber {
    static let timeout: Duration = .seconds(60)

    static func transcribe(_ url: URL) async -> String? {
        guard await authorized() else { return nil }
        let locale = Locale(identifier: Locale.preferredLanguages.first ?? "it-IT")
        guard let recognizer = SFSpeechRecognizer(locale: locale) ?? SFSpeechRecognizer(), recognizer.isAvailable,
              recognizer.supportsOnDeviceRecognition else { return nil }
        let request = SFSpeechURLRecognitionRequest(url: url)
        request.requiresOnDeviceRecognition = true
        request.shouldReportPartialResults = false
        request.addsPunctuation = true
        let box = OnceBox()
        return await withCheckedContinuation { (continuation: CheckedContinuation<String?, Never>) in
            box.continuation = continuation
            let task = recognizer.recognitionTask(with: request) { result, error in
                if let result, result.isFinal {
                    let text = result.bestTranscription.formattedString.trimmingCharacters(in: .whitespacesAndNewlines)
                    box.finish(text.isEmpty ? nil : text)
                } else if error != nil {
                    box.finish(nil)
                }
            }
            box.task = task
            Task {
                try? await Task.sleep(for: timeout)
                box.finish(nil)
            }
        }
    }

    private static func authorized() async -> Bool {
        switch SFSpeechRecognizer.authorizationStatus() {
        case .authorized: return true
        case .notDetermined:
            return await withCheckedContinuation { continuation in
                SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
            }
        default: return false
        }
    }

    /// Resumes the continuation exactly once (the recognizer can call back several times; the timeout races it).
    private final class OnceBox: @unchecked Sendable {
        private let lock = NSLock()
        var continuation: CheckedContinuation<String?, Never>?
        var task: SFSpeechRecognitionTask?

        func finish(_ value: String?) {
            lock.lock()
            let pending = continuation
            continuation = nil
            let running = task
            task = nil
            lock.unlock()
            guard let pending else { return }
            if value == nil { running?.cancel() }
            pending.resume(returning: value)
        }
    }
}
