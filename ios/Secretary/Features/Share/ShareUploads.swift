import Foundation
import UserNotifications

/// Upload progress for `sendNow`.
private final class UploadProgress: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    let report: @Sendable (Double) -> Void
    init(_ report: @escaping @Sendable (Double) -> Void) { self.report = report }
    func urlSession(_ session: URLSession, task: URLSessionTask, didSendBodyData bytesSent: Int64,
                    totalBytesSent: Int64, totalBytesExpectedToSend: Int64) {
        guard totalBytesExpectedToSend > 0 else { return }
        report(Double(totalBytesSent) / Double(totalBytesExpectedToSend))
    }
}

/// Shared by Shortcuts, the share extension and the app's background-session delegate.
@MainActor
final class ShareUploads: NSObject, URLSessionTaskDelegate {
    static let shared = ShareUploads()
    static let completed = Notification.Name("LucinaShareCompleted")
    private var sessions: [String: URLSession] = [:]
    private var completions: [String: () -> Void] = [:]
    private var retrying = Set<String>()

    private func session(_ identifier: String) -> URLSession {
        if let existing = sessions[identifier] { return existing }
        let config = URLSessionConfiguration.background(withIdentifier: identifier)
        // The app can hand its own files to nsurlsessiond; an extension needs the App Group (see sendNow).
        if ShareRequestBuilder.hasAppGroup { config.sharedContainerIdentifier = ShareRequestBuilder.group }
        config.sessionSendsLaunchEvents = true
        config.isDiscretionary = false
        let session = URLSession(configuration: config, delegate: self, delegateQueue: .main)
        sessions[identifier] = session
        return session
    }

    func enqueue(_ record: ShareRecord, credentials: BridgeCredentials) throws {
        guard let endpoint = record.endpoints.first, let request = request(record, endpoint: endpoint, token: credentials.token),
              let directory = try? ShareRequestBuilder.directory() else { throw ShareFailure.preparation }
        let task = session(ShareRequestBuilder.prefix + record.shareId)
            .uploadTask(with: request, fromFile: directory.appendingPathComponent(record.shareId + ".body"))
        task.resume()
    }

    /// Upload in this process now, trying every endpoint, reporting progress (0...1). The share extension uses it
    /// while the App Group is not provisioned; the body file then lives in the extension's own container.
    func sendNow(_ record: ShareRecord, credentials: BridgeCredentials,
                 progress: @escaping @Sendable (Double) -> Void) async throws {
        let directory = try ShareRequestBuilder.directory()
        let body = directory.appendingPathComponent(record.shareId + ".body")
        var lastStatus: Int?
        var lastError: Error?
        for endpoint in record.endpoints {
            guard let request = request(record, endpoint: endpoint, token: credentials.token) else { continue }
            do {
                let (_, response) = try await URLSession.shared.upload(for: request, fromFile: body,
                                                                       delegate: UploadProgress(progress))
                lastStatus = (response as? HTTPURLResponse)?.statusCode
                let decision = ShareRetryPolicy.decision(status: lastStatus, attempts: 0)
                if decision == .success || decision == .discard { break }
            } catch let error as URLError where [.cannotFindHost, .dnsLookupFailed, .cannotConnectToHost].contains(error.code) {
                lastError = error
                continue // the next endpoint (e.g. the Tailscale name)
            }
        }
        let decision = ShareRetryPolicy.decision(status: lastStatus, attempts: record.attempts)
        if decision == .success {
            finish(record, status: lastStatus)
            return
        }
        if let lastError, lastStatus == nil { throw lastError }
        throw ShareFailure.server(lastStatus ?? 0)
    }

    func handleEvents(_ identifier: String, completion: @escaping () -> Void) {
        completions[identifier] = completion
        _ = session(identifier)
    }

    private func request(_ record: ShareRecord, endpoint: String, token: String) -> URLRequest? {
        guard let base = URL(string: endpoint), ["http", "https"].contains(base.scheme ?? "") else { return nil }
        var request = URLRequest(url: base.appendingPathComponent("v1/share"))
        request.httpMethod = "POST"
        request.setValue("Bearer " + token, forHTTPHeaderField: "Authorization")
        request.setValue("multipart/form-data; boundary=" + record.boundary, forHTTPHeaderField: "Content-Type")
        return request
    }

    nonisolated func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        let identifier = session.configuration.identifier ?? ""
        let status = (task.response as? HTTPURLResponse)?.statusCode
        MainActor.assumeIsolated {
            guard let record = self.record(identifier) else { return }
            let result = ShareRetryPolicy.decision(status: error == nil ? status : nil, attempts: 0)
            if !self.retrying.contains(record.shareId) || result == .success || result == .discard {
                self.finish(record, status: error == nil ? status : nil)
            }
            // The app gets didFinishEvents; an extension can release its session here.
            if self.completions[identifier] == nil {
                self.sessions.removeValue(forKey: identifier)?.finishTasksAndInvalidate()
            }
        }
    }

    nonisolated func urlSessionDidFinishEvents(forBackgroundURLSession session: URLSession) {
        let identifier = session.configuration.identifier ?? ""
        MainActor.assumeIsolated {
            self.sessions.removeValue(forKey: identifier)?.finishTasksAndInvalidate()
            self.completions.removeValue(forKey: identifier)?()
        }
    }

    private func record(_ identifier: String) -> ShareRecord? {
        guard identifier.hasPrefix(ShareRequestBuilder.prefix), let directory = try? ShareRequestBuilder.directory(),
              let data = try? Data(contentsOf: directory.appendingPathComponent(String(identifier.dropFirst(ShareRequestBuilder.prefix.count)) + ".json")) else { return nil }
        return try? JSONDecoder().decode(ShareRecord.self, from: data)
    }

    private func finish(_ record: ShareRecord, status: Int?) {
        let decision = ShareRetryPolicy.decision(status: status, attempts: record.attempts)
        guard decision != .retry else { return }
        let defaults = UserDefaults(suiteName: ShareRequestBuilder.group)
        defaults?.set(["shareId": record.shareId, "status": status ?? 0, "success": decision == .success,
                       "date": Date()] as [String: Any], forKey: "share.lastResult")
        defaults?.set(decision == .success ? String(localized: "Sent to Lucina") : String(localized: "Couldn't send to Lucina"), forKey: "share.lastResultMessage")
        if let directory = try? ShareRequestBuilder.directory() {
            for suffix in ["body", "json"] { try? FileManager.default.removeItem(at: directory.appendingPathComponent(record.shareId + "." + suffix)) }
        }
        if decision == .success {
            NotificationCenter.default.post(name: Self.completed, object: nil)
        } else {
            let content = UNMutableNotificationContent()
            content.title = String(localized: "Couldn't send to Lucina")
            content.sound = .default
            UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: "share-" + record.shareId, content: content, trigger: nil))
        }
    }

    /// Reattach and cancel overdue background work before retrying the same idempotent body.
    func retryPending() async {
        guard let credentials = BridgeCredentials.read(), let directory = try? ShareRequestBuilder.directory(),
              let entries = try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil) else { return }
        for entry in entries where entry.pathExtension == "json" {
            if Task.isCancelled { return }
            guard let data = try? Data(contentsOf: entry), var record = try? JSONDecoder().decode(ShareRecord.self, from: data),
                  Date().timeIntervalSince(record.createdAt) > 120, !retrying.contains(record.shareId),
                  FileManager.default.fileExists(atPath: directory.appendingPathComponent(record.shareId + ".body").path) else { continue }
            retrying.insert(record.shareId)
            let identifier = ShareRequestBuilder.prefix + record.shareId
            let background = session(identifier)
            let tasks = await background.allTasks
            for task in tasks { task.cancel() }
            sessions.removeValue(forKey: identifier)?.invalidateAndCancel()
            // A background completion may have removed the record while allTasks suspended.
            guard FileManager.default.fileExists(atPath: entry.path) else { retrying.remove(record.shareId); continue }
            if record.attempts >= 5 { finish(record, status: nil); retrying.remove(record.shareId); continue }
            // One pass is one attempt; each endpoint sees the same idempotency key and body.
            record.attempts += 1
            do {
                try ShareRequestBuilder.save(record, in: directory)
                var finalStatus: Int?
                for endpoint in record.endpoints {
                    guard let request = request(record, endpoint: endpoint, token: credentials.token) else { continue }
                    do {
                        let (_, response) = try await URLSession.shared.upload(for: request, fromFile: directory.appendingPathComponent(record.shareId + ".body"))
                        finalStatus = (response as? HTTPURLResponse)?.statusCode
                        let result = ShareRetryPolicy.decision(status: finalStatus, attempts: 0)
                        if result == .success || result == .discard { break }
                    } catch {
                        if Task.isCancelled {
                            record.attempts -= 1
                            try? ShareRequestBuilder.save(record, in: directory)
                            retrying.remove(record.shareId)
                            return
                        }
                        finalStatus = nil
                    }
                }
                if FileManager.default.fileExists(atPath: entry.path) { finish(record, status: finalStatus) }
            } catch { /* Keep the existing body and record when persistence is unavailable. */ }
            retrying.remove(record.shareId)
        }
    }
}
