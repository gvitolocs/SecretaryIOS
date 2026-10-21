import SwiftUI
import UniformTypeIdentifiers

struct TextExport: FileDocument {
    static var readableContentTypes: [UTType] { [.json, .commaSeparatedText, .plainText] }
    var text: String
    init(text: String) { self.text = text }
    init(configuration: ReadConfiguration) throws {
        text = String(decoding: configuration.file.regularFileContents ?? Data(), as: UTF8.self)
    }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: Data(text.utf8))
    }
}

/// The original LucinaGym settings, plus where the training lives and server sync.
struct GymSettingsScreen: View {
    @Environment(GymStore.self) private var store
    @Environment(AppModel.self) private var app
    @State private var unit = "kg"
    @State private var rest = 90
    @State private var goal = 3
    @State private var exportDocument = TextExport(text: "")
    @State private var exportType = UTType.json
    @State private var exportName = "Lucina-Gym-backup"
    @State private var showExport = false
    @State private var showImport = false
    @State private var confirmRestore = false
    @State private var pendingBackup = ""
    @State private var confirmUpload = false
    @State private var confirmDownload = false
    @State private var syncing = false
    @State private var message: String?

    var body: some View {
        Form {
            Section("Training preferences") {
                Picker("Weight unit", selection: $unit) {
                    Text("Kilograms").tag("kg")
                    Text("Pounds").tag("lb")
                }
                Picker("Rest timer", selection: $rest) {
                    ForEach(Array(Set([30, 45, 60, 90, 120, 180, 300, rest])).sorted(), id: \.self) { Text("\($0) seconds").tag($0) }
                }
                Stepper("Weekly goal: \(goal) workouts", value: $goal, in: 1...14)
                Button("Save preferences") {
                    Task { await store.run(.updateSettings(weightUnit: unit, restSeconds: rest, weeklyGoal: goal)) }
                }
            }
            Section("Your data") {
                Button("Export JSON backup", systemImage: "square.and.arrow.up") {
                    Task { await export(.exportJSON, type: .json, name: "Lucina-Gym-backup") }
                }
                Button("Export workout CSV", systemImage: "tablecells") {
                    Task { await export(.exportCSV, type: .commaSeparatedText, name: "Lucina-Gym-workouts") }
                }
                Button("Restore JSON backup", systemImage: "square.and.arrow.down") { showImport = true }
            }
            Section {
                Picker("Training lives", selection: Binding(get: { app.gymLocation }, set: { app.setGymLocation($0) })) {
                    ForEach(GymLocation.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                Button("Upload this iPhone to the server", systemImage: "icloud.and.arrow.up") { confirmUpload = true }
                    .disabled(syncing)
                Button("Download from the server", systemImage: "icloud.and.arrow.down") { confirmDownload = true }
                    .disabled(syncing)
                if syncing { ProgressView() }
            } header: {
                Text("Server sync")
            } footer: {
                Text(app.gymEngineError
                    ?? String(localized: "On this iPhone, training works offline. Upload or download copies the whole history between the iPhone and the server (which the web app uses); each replaces the other side."))
            }
            if let message = message ?? store.actionError {
                Section { Text(message).font(.footnote).foregroundStyle(GymPalette.muted) }
            }
            Section {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Lucina Gym").font(.headline)
                    Text("Your training, on your device. No account required. Export a backup to keep a copy of your workouts.")
                        .font(.footnote).foregroundStyle(GymPalette.muted)
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(GymPalette.background)
        .onAppear(perform: loadPreferences)
        .onChange(of: store.view?.state.settings) { _, _ in loadPreferences() }
        .fileExporter(isPresented: $showExport, document: exportDocument, contentType: exportType, defaultFilename: exportName) { result in
            if case let .failure(error) = result { message = error.localizedDescription }
        }
        .fileImporter(isPresented: $showImport, allowedContentTypes: [.json]) { result in
            do {
                let url = try result.get()
                let scoped = url.startAccessingSecurityScopedResource()
                defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
                guard size <= 8 * 1024 * 1024 else { message = String(localized: "This backup exceeds the 8 MB import limit."); return }
                pendingBackup = try String(contentsOf: url, encoding: .utf8)
                confirmRestore = true
            } catch {
                message = error.localizedDescription
            }
        }
        .confirmationDialog("Replace your current gym data with this backup?", isPresented: $confirmRestore, titleVisibility: .visible) {
            Button("Restore backup", role: .destructive) {
                let backup = pendingBackup
                pendingBackup = ""
                Task { if await store.run(.restoreJSON(backup: backup)) != nil { loadPreferences() } }
            }
        }
        .confirmationDialog("Replace the server's gym data with this iPhone's?", isPresented: $confirmUpload, titleVisibility: .visible) {
            Button("Upload and replace", role: .destructive) { Task { await sync(upload: true) } }
        }
        .confirmationDialog("Replace this iPhone's gym data with the server's?", isPresented: $confirmDownload, titleVisibility: .visible) {
            Button("Download and replace", role: .destructive) { Task { await sync(upload: false) } }
        }
    }

    private func loadPreferences() {
        guard let settings = store.view?.state.settings else { return }
        unit = settings.weightUnit
        rest = settings.restSeconds
        goal = settings.weeklyGoal
    }

    private func export(_ command: GymCommand, type: UTType, name: String) async {
        message = nil
        guard let response = await store.query(command, reportErrors: true), let text = response.text else { return }
        exportDocument = TextExport(text: text)
        exportType = type
        exportName = name
        showExport = true
    }

    private func sync(upload: Bool) async {
        syncing = true
        defer { syncing = false }
        do {
            if upload {
                try await app.uploadGymToServer()
                message = String(localized: "Uploaded: the server now has this iPhone's training.")
            } else {
                try await app.downloadGymFromServer()
                message = String(localized: "Downloaded: this iPhone now has the server's training.")
                loadPreferences()
            }
        } catch {
            message = (error as? APIError)?.userMessage ?? error.localizedDescription
        }
    }
}
