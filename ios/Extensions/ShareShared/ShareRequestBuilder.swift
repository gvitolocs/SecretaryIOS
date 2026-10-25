import Foundation

struct ShareFile: Sendable {
    var url: URL
    var name: String
    var mime: String
}

struct SharePayload: Sendable {
    var title = ""
    var texts: [String] = []
    var urls: [String] = []
    var files: [ShareFile] = []
}

enum ShareFailure: LocalizedError {
    case signIn, empty, tooLarge, tooManyParts, preparation
    var errorDescription: String? {
        switch self {
        case .signIn: return String(localized: "Open Secretary and sign in first")
        case .empty: return String(localized: "There is nothing to share.")
        case .tooLarge: return String(localized: "This share is larger than 2 GB.")
        case .tooManyParts: return String(localized: "This share has more than 32 parts.")
        case .preparation: return String(localized: "Couldn't prepare this share. Try again.")
        }
    }
}

struct ShareRecord: Codable {
    var shareId: String
    var createdAt: Date
    var endpoints: [String]
    var intent: String
    var attempts: Int
    var boundary: String
}

enum ShareRequestBuilder {
    static let group = "group.tech.getsecretary.shared"
    static let prefix = "tech.getsecretary.share."
    static let maxBytes: Int64 = 2_000_000_000

    static func directory() throws -> URL {
        guard let root = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group) else {
            throw ShareFailure.preparation
        }
        let url = root.appendingPathComponent("Shares", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    static func save(_ record: ShareRecord, in directory: URL) throws {
        try JSONEncoder().encode(record).write(to: directory.appendingPathComponent(record.shareId + ".json"),
                                                options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    /// Bounded chunks, including for multi-gigabyte movie/document providers.
    static func build(payload: SharePayload, intent: String, note: String, source: String,
                      credentials: BridgeCredentials, directory: URL, shareId: String = UUID().uuidString,
                      boundary: String = "Lucina-" + UUID().uuidString, language: String = String((Locale.preferredLanguages.first ?? "en").prefix(2))) throws -> ShareRecord {
        guard !payload.files.isEmpty || !payload.texts.isEmpty || !payload.urls.isEmpty else { throw ShareFailure.empty }
        var fields = [("shareId", shareId), ("intent", intent), ("note", String(note.prefix(4000))),
                      ("lang", language), ("channel", "ios"), ("source", source)]
        if !payload.title.isEmpty { fields.append(("title", payload.title)) }
        fields += payload.texts.map { ("text", $0) } + payload.urls.map { ("url", $0) }
        guard fields.count + payload.files.count <= 32 else { throw ShareFailure.tooManyParts }
        var estimated = Int64(fields.reduce(0) { $0 + $1.1.utf8.count })
        for file in payload.files {
            estimated += Int64(try file.url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0)
            guard estimated <= maxBytes else { throw ShareFailure.tooLarge }
        }
        let body = directory.appendingPathComponent(shareId + ".body")
        FileManager.default.createFile(atPath: body.path, contents: nil,
                                      attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
        let output = try FileHandle(forWritingTo: body)
        var succeeded = false
        defer { try? output.close(); if !succeeded { try? FileManager.default.removeItem(at: body) } }
        var bytes: Int64 = 0
        func write(_ data: Data) throws {
            bytes += Int64(data.count)
            guard bytes <= maxBytes else { throw ShareFailure.tooLarge }
            try output.write(contentsOf: data)
        }
        func line(_ value: String) throws { try write(Data(value.utf8)) }
        for (name, value) in fields {
            try line("--\(boundary)\r\nContent-Disposition: form-data; name=\"\(name)\"\r\n\r\n\(value)\r\n")
        }
        for file in payload.files {
            let name = file.name.replacingOccurrences(of: "\r", with: "_").replacingOccurrences(of: "\n", with: "_")
                .replacingOccurrences(of: "\"", with: "_").replacingOccurrences(of: "\\", with: "_")
            let mime = file.mime.contains("\r") || file.mime.contains("\n") ? "application/octet-stream" : file.mime
            try line("--\(boundary)\r\nContent-Disposition: form-data; name=\"file\"; filename=\"\(name)\"\r\nContent-Type: \(mime)\r\n\r\n")
            let input = try FileHandle(forReadingFrom: file.url)
            defer { try? input.close() }
            while let chunk = try input.read(upToCount: 256 * 1024), !chunk.isEmpty { try write(chunk) }
            try line("\r\n")
        }
        try line("--\(boundary)--\r\n")
        try output.synchronize()
        let record = ShareRecord(shareId: shareId, createdAt: Date(), endpoints: credentials.endpoints,
                                 intent: intent, attempts: 1, boundary: boundary)
        try save(record, in: directory)
        succeeded = true
        return record
    }
}

enum ShareRetryPolicy {
    enum Decision: Equatable { case success, discard, retry, exhausted }
    static func decision(status: Int?, attempts: Int) -> Decision {
        if let status, (200..<300).contains(status) { return .success }
        if let status, (400..<500).contains(status), ![401, 408, 429].contains(status) { return .discard }
        return attempts >= 5 ? .exhausted : .retry
    }
}
