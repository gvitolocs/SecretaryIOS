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

struct GymSettingsScreen: View {
    @Environment(GymStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var unit = "kg"
    @State private var rest = 90
    @State private var goal = 3
    @State private var exportDocument = TextExport(text: "")
    @State private var exportType = UTType.json
    @State private var exportName = "Secretary-Gym-backup"
    @State private var showExport = false
    @State private var showImport = false
    @State private var confirmRestore = false
    @State private var pendingBackup = ""
    @State private var message: String?

    var body: some View {
        NavigationStack {
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
                }
                Section("Your data") {
                    Button("Export JSON backup", systemImage: "square.and.arrow.up") {
                        Task { await export(.exportJSON, type: .json, name: "Secretary-Gym-backup") }
                    }
                    Button("Export workout CSV", systemImage: "tablecells") {
                        Task { await export(.exportCSV, type: .commaSeparatedText, name: "Secretary-Gym-workouts") }
                    }
                    Button("Restore JSON backup", systemImage: "square.and.arrow.down") { showImport = true }
                }
                if let message = message ?? store.actionError {
                    Section { Text(message).font(.footnote).foregroundStyle(.secondary) }
                }
            }
            .navigationTitle("Gym settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        Task {
                            if await store.run(.updateSettings(weightUnit: unit, restSeconds: rest, weeklyGoal: goal)) != nil { dismiss() }
                        }
                    }
                }
            }
            .onAppear(perform: loadPreferences)
            .fileExporter(isPresented: $showExport, document: exportDocument, contentType: exportType, defaultFilename: exportName) { result in
                if case let .failure(error) = result { message = error.localizedDescription }
            }
            .fileImporter(isPresented: $showImport, allowedContentTypes: [.json]) { result in
                do {
                    let url = try result.get()
                    let scoped = url.startAccessingSecurityScopedResource()
                    defer { if scoped { url.stopAccessingSecurityScopedResource() } }
                    let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
                    guard size <= 15 * 1024 * 1024 else { message = "This backup is larger than the 15 MB the server accepts."; return }
                    pendingBackup = try String(contentsOf: url, encoding: .utf8)
                    confirmRestore = true
                } catch {
                    message = error.localizedDescription
                }
            }
            .confirmationDialog("Replace your gym data with this backup?", isPresented: $confirmRestore, titleVisibility: .visible) {
                Button("Restore backup", role: .destructive) {
                    let backup = pendingBackup
                    pendingBackup = ""
                    Task {
                        if await store.run(.restoreJSON(backup: backup)) != nil {
                            loadPreferences()
                            message = "Backup restored."
                        }
                    }
                }
            }
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
}
