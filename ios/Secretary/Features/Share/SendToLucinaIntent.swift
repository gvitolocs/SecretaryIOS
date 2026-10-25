import AppIntents
import UniformTypeIdentifiers
import Foundation

enum ShareAction: String, AppEnum {
    case ask, library, shopping, note, remind
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Share action"
    static let caseDisplayRepresentations: [ShareAction: DisplayRepresentation] = [
        .ask: "Ask Lucina", .library: "Save to library", .shopping: "Add to shopping",
        .note: "Save as note", .remind: "Remind me"
    ]
}

struct SendToLucinaIntent: AppIntent {
    static let title: LocalizedStringResource = "Send to Lucina"
    static let openAppWhenRun = false

    @Parameter(title: "Files", supportedContentTypes: [.item])
    var files: [IntentFile]?
    @Parameter(title: "Text") var text: String?
    @Parameter(title: "Note") var note: String?
    @Parameter(title: "Action", default: .ask) var action: ShareAction

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        AppModel.shared.device.syncCredentials()
        guard let credentials = BridgeCredentials.read() else { throw ShareFailure.signIn }
        let directory = try ShareRequestBuilder.directory()
        let staging = directory.appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: staging, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: staging) }
        var payload = SharePayload()
        if let text { ShareItemReader.addText(text, to: &payload) }
        for file in files ?? [] {
            if let url = file.fileURL {
                payload.files.append(try ShareItemReader.copiedFile(url, name: file.filename, type: file.type, directory: staging))
            } else {
                // Already memory-backed by Shortcuts. Disk-backed files never use `data`.
                let url = staging.appendingPathComponent(UUID().uuidString).appendingPathExtension((file.filename as NSString).pathExtension)
                guard Int64(file.data.count) <= ShareRequestBuilder.maxBytes else { throw ShareFailure.tooLarge }
                try file.data.write(to: url)
                payload.files.append(try ShareItemReader.normalize(ShareFile(url: url, name: file.filename, mime: file.type?.preferredMIMEType ?? "application/octet-stream")))
            }
        }
        let prepared = payload
        let intent = action.rawValue
        let ownerNote = note ?? ""
        let record = try await Task.detached {
            try ShareRequestBuilder.build(payload: prepared, intent: intent, note: ownerNote, source: "ios-intent",
                                          credentials: credentials, directory: directory)
        }.value
        try ShareUploads.shared.enqueue(record, credentials: credentials)
        LucinaDonations.donate()
        return .result(dialog: "Sent to Lucina")
    }
}
