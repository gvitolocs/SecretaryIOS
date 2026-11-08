import AVFoundation
import SwiftUI

// QR sign-in, Discord-style (server: crates/secretary-server/src/qr_login.rs): a signed-out
// screen (the web app, Secretary Code) shows a QR code with `https://getsecretary.tech/approve?c=…`
// and a 6-digit code. This app scans it (or the code is typed), shows who is asking
// (`GET /v1/session/qr/info`) and approves or denies (`POST /v1/session/qr/approve`) with its own
// device token. The phone's Camera app would open the link in Safari, which is not signed in.

/// What a scanned or typed value means.
enum QrSignInCode {
    /// Hosts whose approve links this app answers (besides the server it talks to).
    static let approveHosts: Set<String> = ["getsecretary.tech"]

    /// The sign-in code in `https://getsecretary.tech/approve?c=…`, or a typed 6-digit code
    /// ("482 913" and "482-913" too); nil for anything else.
    static func code(from text: String, serverHost: String? = nil) -> String? {
        let raw = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let digits = raw.filter { !$0.isWhitespace && $0 != "-" }
        if digits.count == 6, digits.allSatisfy(\.isASCII), digits.allSatisfy(\.isNumber) { return digits }
        guard let url = URLComponents(string: raw), let host = url.host?.lowercased() else { return nil }
        let own = serverHost.map { $0.lowercased() == host } ?? false
        guard own || (approveHosts.contains(host) && url.scheme == "https") else { return nil }
        guard url.path == "/approve",
              let c = url.queryItems?.first(where: { $0.name == "c" })?.value,
              isCode(c) else { return nil }
        return c
    }

    private static func isCode(_ c: String) -> Bool {
        (6...64).contains(c.count)
            && c.unicodeScalars.allSatisfy { $0.isASCII && (CharacterSet.alphanumerics.contains($0) || $0 == "-" || $0 == "_") }
    }
}

/// The requesting device, for the approve sheet.
struct QrSignInInfo: Decodable, Equatable, Sendable {
    let ip: String
    let userAgent: String
    let createdAt: Date?
    let expiresIn: Int?

    init(ip: String, userAgent: String, createdAt: Date? = nil, expiresIn: Int? = nil) {
        self.ip = ip
        self.userAgent = userAgent
        self.createdAt = createdAt
        self.expiresIn = expiresIn
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        ip = (try? c.decode(String.self, forKey: .ip)) ?? ""
        userAgent = (try? c.decode(String.self, forKey: .userAgent)) ?? ""
        createdAt = (try? c.decodeIfPresent(String.self, forKey: .createdAt)).flatMap { $0 }.flatMap(Self.date)
        expiresIn = try? c.decodeIfPresent(Int.self, forKey: .expiresIn)
    }

    private enum CodingKeys: String, CodingKey { case ip, userAgent, createdAt, expiresIn }

    /// RFC 3339 from the server, which writes nanoseconds ("…:47.499728123Z"): the fraction is dropped.
    static func date(_ text: String) -> Date? {
        let whole = text.replacingOccurrences(of: #"\.\d+"#, with: "", options: .regularExpression)
        return ISO8601DateFormatter().date(from: whole)
    }

    /// "Safari on macOS", "Chrome", or nil when the agent is not recognised.
    var deviceDescription: String? {
        let (browser, os) = UserAgentSummary.describe(userAgent)
        switch (browser, os) {
        case let (b?, o?): return String(localized: "\(b) on \(o)")
        case let (b?, nil): return b
        case let (nil, o?): return o
        default: return nil
        }
    }
}

/// A User-Agent string as a person reads it (same rules as the web's lib/userAgent.ts).
enum UserAgentSummary {
    static func describe(_ ua: String) -> (browser: String?, os: String?) {
        (browser(ua), os(ua))
    }

    private static func has(_ ua: String, _ pattern: String) -> Bool {
        ua.range(of: pattern, options: .regularExpression) != nil
    }

    private static func browser(_ ua: String) -> String? {
        if has(ua, #"\bEdg(A|iOS)?/"#) { return "Edge" }
        if has(ua, #"\b(OPR|OPiOS)/"#) { return "Opera" }
        if has(ua, #"\b(Firefox|FxiOS)/"#) { return "Firefox" }
        if has(ua, #"\bSamsungBrowser/"#) { return "Samsung Internet" }
        if has(ua, #"\b(HeadlessChrome|Chrome|CriOS|Chromium)/"#) { return "Chrome" }
        if has(ua, #"\bVersion/[\d.]+.*\bSafari/"#) { return "Safari" }
        // WKWebView (Secretary Code, the apps' embedded views): AppleWebKit without the Safari token.
        if has(ua, #"\bAppleWebKit/"#) && !has(ua, #"\bSafari/"#) { return "Secretary app" }
        return nil
    }

    private static func os(_ ua: String) -> String? {
        if has(ua, #"\biPhone\b"#) { return "iOS" }
        if has(ua, #"\biPad\b"#) { return "iPadOS" }
        if has(ua, #"\bAndroid\b"#) { return "Android" }
        if has(ua, #"\bCrOS\b"#) { return "ChromeOS" }
        if has(ua, #"\bMac OS X\b|\bMacintosh\b"#) { return "macOS" }
        if has(ua, #"\bWindows\b"#) { return "Windows" }
        if has(ua, #"\bLinux\b"#) { return "Linux" }
        return nil
    }
}

/// `/v1/session/qr/info` and `/approve`, with this device's token (Bearer: no CSRF header needed).
struct QrSignInAPI: Sendable {
    let client: APIClient

    func info(_ code: String) async throws -> QrSignInInfo {
        try await client.get("/v1/session/qr/info", query: [URLQueryItem(name: "c", value: code)])
    }

    private struct Answer: Encodable { let code: String; let approve: Bool }
    private struct Status: Decodable { let status: String? }

    func answer(_ code: String, approve: Bool) async throws {
        let _: Status = try await client.send("POST", "/v1/session/qr/approve", body: Answer(code: code, approve: approve))
    }
}

// MARK: - The sheet

/// Scan QR code → who is asking → Approve / Deny. From Settings and the Plugins tab.
struct QrSignInSheet: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    private enum Phase: Equatable {
        case scanning
        case loading(String)
        case asking(String, QrSignInInfo)
        case sending(String, QrSignInInfo, approve: Bool)
        case done(approved: Bool)
        case gone
        case failed(String)
    }

    @State private var phase: Phase = .scanning
    @State private var typed = ""
    @State private var typedInvalid = false
    @State private var notOurs = false

    private var api: QrSignInAPI { QrSignInAPI(client: app.client) }

    var body: some View {
        NavigationStack {
            content
                .navigationTitle("Sign in another device")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button(isFinished ? "Done" : "Cancel") { dismiss() }
                    }
                }
        }
    }

    private var isFinished: Bool {
        if case .done = phase { return true }
        return false
    }

    @ViewBuilder private var content: some View {
        switch phase {
        case .scanning:
            scanning
        case .loading:
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        case let .asking(code, info), let .sending(code, info, _):
            asking(code: code, info: info)
        case let .done(approved):
            outcome(systemImage: approved ? "checkmark.circle.fill" : "xmark.circle",
                    tint: approved ? .green : .secondary,
                    text: approved ? String(localized: "Done. The other device is signed in.")
                                   : String(localized: "Denied. The other device stays signed out."))
        case .gone:
            retry(String(localized: "This code expired or was already used. Show a new code on the other device and scan it again."))
        case let .failed(message):
            retry(message)
        }
    }

    private var scanning: some View {
        Form {
            Section {
                QrCameraView { value in found(value) }
                    .frame(height: 320)
                    .listRowInsets(EdgeInsets())
                    .accessibilityLabel("Camera")
                if notOurs {
                    Text("That isn't a Secretary sign-in code.").font(.footnote).foregroundStyle(.secondary)
                }
            } footer: {
                Text("Point the camera at the QR code on the screen you want to sign in.")
            }
            Section {
                HStack {
                    TextField("123 456", text: $typed)
                        .keyboardType(.numberPad)
                        .textContentType(.oneTimeCode)
                        .font(.title3.monospacedDigit())
                        .accessibilityIdentifier("qrTypedCode")
                    Button("Continue") { submitTyped() }
                        .buttonStyle(.borderedProminent)
                        .disabled(typed.isEmpty)
                }
                if typedInvalid {
                    Text("Type the 6 digits shown under the QR code.").font(.footnote).foregroundStyle(.red)
                }
            } header: {
                Text("Or type the 6-digit code")
            }
        }
    }

    private func asking(code: String, info: QrSignInInfo) -> some View {
        let sending: Bool
        let approving: Bool
        if case let .sending(_, _, approve) = phase { sending = true; approving = approve } else { sending = false; approving = false }
        return Form {
            Section {
                Text("A device is asking to sign in to your Secretary. Approve only if it's yours and in front of you.")
                    .foregroundStyle(.secondary)
            }
            Section {
                LabeledContent("Device", value: info.deviceDescription ?? String(localized: "Unknown device"))
                LabeledContent("IP address") { Text(info.ip).monospaced() }
                if let at = info.createdAt {
                    LabeledContent("Asked", value: at.formatted(date: .omitted, time: .standard))
                }
            }
            Section {
                Button {
                    Task { await answer(code, info: info, approve: true) }
                } label: {
                    Label(approving ? String(localized: "Approving…") : String(localized: "Approve"), systemImage: "checkmark")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .disabled(sending)
                .accessibilityIdentifier("qrApprove")
                Button(role: .destructive) {
                    Task { await answer(code, info: info, approve: false) }
                } label: {
                    Label("Deny", systemImage: "xmark").frame(maxWidth: .infinity)
                }
                .disabled(sending)
            }
            .listRowBackground(Color.clear)
        }
        .navigationTitle("Sign in on another device?")
    }

    private func outcome(systemImage: String, tint: Color, text: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: systemImage).font(.system(size: 56)).foregroundStyle(tint)
            Text(text).multilineTextAlignment(.center)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func retry(_ message: String) -> some View {
        VStack(spacing: 16) {
            Image(systemName: "exclamationmark.triangle").font(.system(size: 44)).foregroundStyle(.orange)
            Text(message).multilineTextAlignment(.center)
            Button("Scan again") { notOurs = false; typed = ""; phase = .scanning }
                .buttonStyle(.borderedProminent)
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: Actions

    private func found(_ value: String) {
        guard phase == .scanning else { return }
        if let code = QrSignInCode.code(from: value, serverHost: app.client.baseURL.host) {
            Task { await load(code) }
        } else {
            notOurs = true
        }
    }

    private func submitTyped() {
        guard let code = QrSignInCode.code(from: typed) else {
            typedInvalid = true
            return
        }
        typedInvalid = false
        Task { await load(code) }
    }

    private func load(_ code: String) async {
        phase = .loading(code)
        do {
            phase = .asking(code, try await api.info(code))
        } catch {
            fail(error)
        }
    }

    private func answer(_ code: String, info: QrSignInInfo, approve: Bool) async {
        phase = .sending(code, info, approve: approve)
        do {
            try await api.answer(code, approve: approve)
            phase = .done(approved: approve)
        } catch {
            fail(error)
        }
    }

    private func fail(_ error: Error) {
        guard let error = error as? APIError else {
            phase = .failed(error.localizedDescription)
            return
        }
        if error.status == 404 || error.status == 409 {
            phase = .gone
        } else {
            phase = .failed(error.userMessage)
        }
    }
}

// MARK: - Camera

/// The back camera reading QR codes (AVCaptureMetadataOutput). Reports each new value once.
struct QrCameraView: UIViewControllerRepresentable {
    let onCode: (String) -> Void

    func makeUIViewController(context: Context) -> QrCameraController {
        let controller = QrCameraController()
        controller.onCode = onCode
        return controller
    }

    func updateUIViewController(_ controller: QrCameraController, context: Context) {
        controller.onCode = onCode
    }
}

final class QrCameraController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onCode: ((String) -> Void)?
    private let session = AVCaptureSession()
    private let queue = DispatchQueue(label: "secretary.qr-camera")
    private var preview: AVCaptureVideoPreviewLayer?
    private var lastValue: String?
    private let message = UILabel()

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        message.textColor = .white
        message.numberOfLines = 0
        message.textAlignment = .center
        message.font = .preferredFont(forTextStyle: .callout)
        message.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(message)
        NSLayoutConstraint.activate([
            message.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            message.centerYAnchor.constraint(equalTo: view.centerYAnchor),
            message.widthAnchor.constraint(lessThanOrEqualTo: view.widthAnchor, constant: -40),
        ])
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            configure()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                DispatchQueue.main.async { granted ? self.configure() : self.show(denied: true) }
            }
        default:
            show(denied: true)
        }
    }

    private func show(denied: Bool) {
        message.text = denied
            ? String(localized: "Camera access is off for Secretary. Allow it in Settings, or type the code below.")
            : String(localized: "No camera here. Type the code shown under the QR code instead.")
    }

    private func configure() {
        guard let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back),
              let input = try? AVCaptureDeviceInput(device: device),
              session.canAddInput(input) else {
            show(denied: false)
            return
        }
        session.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else {
            show(denied: false)
            return
        }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]
        let layer = AVCaptureVideoPreviewLayer(session: session)
        layer.videoGravity = .resizeAspectFill
        layer.frame = view.bounds
        view.layer.insertSublayer(layer, at: 0)
        preview = layer
        queue.async { self.session.startRunning() }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        preview?.frame = view.bounds
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        queue.async { self.session.stopRunning() }
    }

    func metadataOutput(_ output: AVCaptureMetadataOutput, didOutput objects: [AVMetadataObject], from connection: AVCaptureConnection) {
        guard let value = objects.compactMap({ ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }).first,
              value != lastValue else { return }
        lastValue = value
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        onCode?(value)
    }
}
