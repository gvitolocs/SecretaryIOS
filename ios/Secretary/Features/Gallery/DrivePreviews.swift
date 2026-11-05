import QuickLook
import SwiftUI
import UIKit

// Drive previews (DriveOpen decides which): photos and videos in the Gallery's viewer, audio in the
// media player, PDFs and Office files in Quick Look (downloaded under their real names; Quick Look
// renders PDF, docx, xlsx and pptx and has its own Share button), text and code in DriveTextViewer,
// archives in DriveArchiveViewer, .xlsx workbooks in SheetScreen, anything else in DriveFileInfo. Every sheet has Share and Download
// (a copy to Files) through DriveFileActions.

/// One Drive level's open previews.
@Observable
@MainActor
final class DrivePreviewer {
    var quickLook: URL?
    var text: GalleryItem?
    var archive: GalleryItem?
    var info: GalleryItem?
    /// An .xlsx in the sheet editor.
    var sheet: GalleryItem?
    /// A photo or video in the Gallery's viewer, paging through `viewerStore`'s items.
    var viewing: GalleryItem?
    @ObservationIgnored var viewerStore: GalleryStore?
    /// The file being downloaded for Quick Look.
    private(set) var opening: String?
    var error: String?

    /// `items`: the list the file was tapped in (the viewer pages through its photos and videos,
    /// the player queues its audio).
    func open(_ item: GalleryItem, among items: [GalleryItem], api: @escaping () -> GalleryAPI, media: MediaPlayer) {
        switch DriveOpen.decide(item) {
        case .gallery:
            let visual = items.filter { $0.kind == "photo" || $0.isVideo }
            viewerStore = GalleryStore(api: api, scope: "drive", items: visual.contains { $0.id == item.id } ? visual : [item])
            viewing = item
        case .audio:
            var playable = items.filter { $0.isVideo || $0.isAudio }
            if !playable.contains(where: { $0.id == item.id }) { playable = [item] }
            media.play(playable.map(\.mediaItem), startAt: playable.firstIndex { $0.id == item.id } ?? 0)
            media.presented = true
        case let .quickLook(pdf):
            guard opening == nil else { return }
            opening = item.id
            let client = api()
            Task {
                defer { opening = nil }
                do {
                    quickLook = try await Self.fetch(item, pdf: pdf, api: client)
                } catch is CancellationError {
                } catch {
                    self.error = error.localizedDescription
                }
            }
        case .text:
            text = item
        case .archive:
            archive = item
        case .info:
            info = item
        case .sheet:
            sheet = item
        }
    }

    /// The server's PDF when asked for (falling back to the original when it cannot convert), else the original.
    private static func fetch(_ item: GalleryItem, pdf: Bool, api: GalleryAPI) async throws -> URL {
        if pdf {
            do {
                return try await api.download(item: item, pdf: true)
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                // 503 "no converter" or no `urls.pdf`: Quick Look on the original.
            }
        }
        return try await api.download(item: item)
    }
}

extension View {
    /// Presents `previewer`'s previews. `playsMedia`: this level is on top, so it presents the media
    /// player (one level does, not every level of the stack).
    func drivePreviews(_ previewer: DrivePreviewer, playsMedia: Bool = true) -> some View {
        modifier(DrivePreviewHost(previewer: previewer, playsMedia: playsMedia))
    }
}

private struct DrivePreviewHost: ViewModifier {
    @Environment(GalleryStore.self) private var store
    @Environment(MediaPlayer.self) private var media
    @Bindable var previewer: DrivePreviewer
    let playsMedia: Bool

    func body(content: Content) -> some View {
        content
            .fullScreenCover(item: $previewer.viewing) { item in
                GalleryViewer(start: item)
                    .environment(previewer.viewerStore ?? store)
                    .presentationBackground(.clear)
            }
            .quickLookPreview($previewer.quickLook)
            .sheet(item: $previewer.text) { item in
                NavigationStack { DriveTextViewer(item: item) }
                    .environment(store)
                    .lucinaSheet()
            }
            .sheet(item: $previewer.archive) { item in
                NavigationStack { DriveArchiveViewer(item: item) }
                    .environment(store)
                    .lucinaSheet()
            }
            .fullScreenCover(item: $previewer.sheet) { item in
                NavigationStack { SheetScreen(item: item) }
                    .environment(store)
            }
            .sheet(item: $previewer.info) { item in
                NavigationStack { DriveFileInfo(item: item) }
                    .environment(store)
                    .lucinaSheet()
                    .presentationDetents([.medium, .large])
            }
            .fullScreenCover(isPresented: Binding(get: { playsMedia && media.presented }, set: { media.presented = $0 })) {
                MediaPlayerScreen()
            }
            .alert("Couldn't open the file", isPresented: Binding(get: { previewer.error != nil },
                                                                   set: { if !$0 { previewer.error = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(verbatim: previewer.error ?? "")
            }
    }
}

// MARK: - Share and Download

/// A downloaded file, for `sheet(item:)`.
struct DriveLocalFile: Identifiable, Hashable {
    let url: URL
    var id: String { url.path }
}

/// Share (the share sheet) and Download (a copy saved to Files), each downloading the file first.
struct DriveFileActions: View {
    @Environment(GalleryStore.self) private var store
    let item: GalleryItem
    /// Buttons with titles (in sheets) instead of toolbar icons.
    var labeled = false
    @State private var busy: Action?
    @State private var sharing: DriveLocalFile?
    @State private var exporting: DriveLocalFile?
    @State private var error: String?

    enum Action { case share, save }

    var body: some View {
        HStack(spacing: labeled ? LucinaTokens.Space.md : LucinaTokens.Space.lg) {
            button(.share)
            button(.save)
        }
        .disabled(busy != nil)
        .sheet(item: $sharing) { file in
            ActivityView(items: [file.url])
                .presentationDetents([.medium, .large])
                .ignoresSafeArea()
        }
        .sheet(item: $exporting) { file in
            DocumentExporter(url: file.url) { exporting = nil }
                .ignoresSafeArea()
        }
        .alert("Couldn't download the file", isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(verbatim: error ?? "")
        }
    }

    @ViewBuilder
    private func button(_ action: Action) -> some View {
        let symbol = action == .share ? "square.and.arrow.up" : "arrow.down.circle"
        let title: LocalizedStringKey = action == .share ? "Share" : "Download"
        Button { run(action) } label: {
            if busy == action {
                ProgressView()
            } else if labeled {
                Label(title, systemImage: symbol)
            } else {
                Image(systemName: symbol)
            }
        }
        .modifier(LabeledButtonStyle(labeled: labeled))
        .accessibilityLabel(Text(title))
        .accessibilityIdentifier(action == .share ? "drive-share" : "drive-download")
    }

    private func run(_ action: Action) {
        guard busy == nil else { return }
        busy = action
        let api = store.api()
        let target = self.item
        Task {
            defer { busy = nil }
            do {
                let url = try await api.download(item: target)
                let file = DriveLocalFile(url: url)
                switch action {
                case .share: sharing = file
                case .save: exporting = file
                }
            } catch is CancellationError {
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

private struct LabeledButtonStyle: ViewModifier {
    let labeled: Bool

    @ViewBuilder
    func body(content: Content) -> some View {
        if labeled {
            content.buttonStyle(.bordered).tint(Theme.accent)
        } else {
            content.buttonStyle(.borderless)
        }
    }
}

/// The system share sheet for files.
struct ActivityView: UIViewControllerRepresentable {
    let items: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}

/// "Save to Files": the document picker exporting a copy.
struct DocumentExporter: UIViewControllerRepresentable {
    let url: URL
    let done: () -> Void

    func makeUIViewController(context: Context) -> UIDocumentPickerViewController {
        let picker = UIDocumentPickerViewController(forExporting: [url], asCopy: true)
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ controller: UIDocumentPickerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(done: done) }

    final class Coordinator: NSObject, UIDocumentPickerDelegate {
        let done: () -> Void

        init(done: @escaping () -> Void) { self.done = done }

        func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) { done() }
        func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) { done() }
    }
}

// MARK: - Text and code

/// A text or code file: line numbers, colours per token class, horizontal scrolling, lazily drawn
/// lines (big files), a note when the server sent only the beginning, and Copy.
struct DriveTextViewer: View {
    @Environment(GalleryStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let item: GalleryItem
    @State private var phase: Phase = .loading
    @State private var copied = false

    struct Loaded {
        let text: DriveText
        let lines: [[DriveToken]]
        /// The width of the line-number column.
        var gutter: CGFloat { CGFloat(String(max(lines.count, 1)).count) * 8 + 4 }
    }

    enum Phase {
        case loading
        /// 202: the server has not read the file yet.
        case pending
        /// 404: a binary file.
        case noText
        case failed(String)
        case ready(Loaded)
    }

    private var loaded: Loaded? {
        if case let .ready(loaded) = phase { return loaded }
        return nil
    }

    var body: some View {
        content
            .navigationTitle(item.driveName)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                ToolbarItemGroup(placement: .primaryAction) {
                    if let loaded {
                        Button { copy(loaded.text.text) } label: {
                            Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        }
                        .accessibilityLabel("Copy")
                        .accessibilityIdentifier("drive-copy")
                    }
                    DriveFileActions(item: item)
                }
            }
            .task { await load() }
    }

    @ViewBuilder
    private var content: some View {
        switch phase {
        case .loading:
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        case .pending:
            ContentUnavailableView {
                Label("Still reading this file…", systemImage: "hourglass")
            } description: {
                Text("The server hasn't read this file yet. Try again in a moment.")
            } actions: {
                Button("Retry") { Task { await load() } }
                    .accessibilityIdentifier("drive-text-retry")
            }
        case .noText:
            ContentUnavailableView {
                Label("This file has no text", systemImage: "doc")
            } description: {
                Text("Download it to open it in another app.")
            } actions: {
                DriveFileActions(item: item, labeled: true)
            }
        case let .failed(message):
            ContentUnavailableView {
                Label("Couldn't load the file", systemImage: "exclamationmark.triangle")
            } description: {
                Text(verbatim: message)
            } actions: {
                Button("Retry") { Task { await load() } }
            }
        case let .ready(loaded):
            VStack(spacing: 0) {
                if loaded.text.truncated {
                    DriveNote(text: "Only the beginning of this file is shown.")
                }
                if loaded.lines.isEmpty {
                    ContentUnavailableView("This file is empty", systemImage: "doc")
                } else {
                    DriveCodeView(loaded: loaded)
                }
            }
        }
    }

    private func load() async {
        phase = .loading
        do {
            switch try await store.api().text(item: item) {
            case .pending:
                phase = .pending
            case let .ready(text):
                phase = .ready(Loaded(text: text, lines: text.displayLines))
            }
        } catch is CancellationError {
        } catch {
            phase = DriveFiles.isMissing(error) ? .noText : .failed(error.localizedDescription)
        }
    }

    private func copy(_ text: String) {
        UIPasteboard.general.string = text
        copied = true
        Task {
            try? await Task.sleep(for: .seconds(1.5))
            copied = false
        }
    }
}

/// The lines, drawn as they scroll into view.
private struct DriveCodeView: View {
    let loaded: DriveTextViewer.Loaded

    var body: some View {
        ScrollView([.vertical, .horizontal]) {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(loaded.lines.indices, id: \.self) { index in
                    DriveCodeLine(number: index + 1, tokens: loaded.lines[index], gutter: loaded.gutter)
                }
            }
            .padding(.vertical, LucinaTokens.Space.sm)
            .padding(.trailing, LucinaTokens.Space.lg)
        }
        .background(Theme.card)
        .textSelection(.enabled)
    }
}

private struct DriveCodeLine: View {
    let number: Int
    let tokens: [DriveToken]
    let gutter: CGFloat

    /// A minified file's one huge line would stall layout: lines stop here (Copy has everything).
    static let maxColumns = 2000

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: LucinaTokens.Space.md) {
            Text(verbatim: String(number))
                .foregroundStyle(Theme.textTertiary)
                .frame(width: gutter, alignment: .trailing)
                .accessibilityHidden(true)
            Text(Self.attributed(tokens))
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
        }
        .font(.system(.footnote, design: .monospaced))
        .padding(.horizontal, LucinaTokens.Space.sm)
    }

    static func attributed(_ tokens: [DriveToken]) -> AttributedString {
        var out = AttributedString()
        var columns = 0
        for token in tokens {
            var raw = token.text.replacingOccurrences(of: "\t", with: "    ")
            let cut = columns + raw.count > maxColumns
            if cut { raw = String(raw.prefix(max(0, maxColumns - columns))) + "…" }
            columns += raw.count
            var piece = AttributedString(raw)
            let role = DriveSyntaxRole(tokenClass: token.kind)
            if role != .plain { piece.foregroundColor = role.color }
            if role.isBold {
                piece.inlinePresentationIntent = .stronglyEmphasized
            } else if role.isItalic {
                piece.inlinePresentationIntent = .emphasized
            }
            out.append(piece)
            if cut { break }
        }
        // An empty line keeps its height.
        return out.characters.isEmpty ? AttributedString(" ") : out
    }
}

/// A one-line note above a preview ("only the beginning…").
private struct DriveNote: View {
    let text: LocalizedStringKey

    var body: some View {
        Label(text, systemImage: "info.circle")
            .font(LucinaTokens.Fonts.caption)
            .foregroundStyle(Theme.textSecondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, LucinaTokens.Space.gutter)
            .padding(.vertical, LucinaTokens.Space.sm)
            .background(Theme.field)
    }
}

// MARK: - Archives

/// The entries of a zip/tar/7z as a tree, with sizes, the total and a note when the list is cut.
struct DriveArchiveViewer: View {
    @Environment(GalleryStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    let item: GalleryItem
    @State private var phase: Phase = .loading

    enum Phase {
        case loading
        /// 415: a format the server cannot list (rar…).
        case unsupported
        case failed(String)
        case ready(DriveArchive, [DriveArchiveTree.Row])
    }

    var body: some View {
        content
            .navigationTitle(item.driveName)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
                ToolbarItem(placement: .primaryAction) { DriveFileActions(item: item) }
            }
            .task { await load() }
    }

    @ViewBuilder
    private var content: some View {
        switch phase {
        case .loading:
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        case .unsupported:
            ContentUnavailableView {
                Label("This archive can't be listed", systemImage: "archivebox")
            } description: {
                Text("Download it to open it in Files.")
            } actions: {
                DriveFileActions(item: item, labeled: true)
            }
        case let .failed(message):
            ContentUnavailableView {
                Label("Couldn't load the file", systemImage: "exclamationmark.triangle")
            } description: {
                Text(verbatim: message)
            } actions: {
                Button("Retry") { Task { await load() } }
            }
        case let .ready(archive, rows):
            List {
                Section {
                    ForEach(rows) { row in
                        HStack(spacing: LucinaTokens.Space.sm) {
                            Image(systemName: row.isDir ? "folder.fill" : "doc")
                                .foregroundStyle(row.isDir ? Theme.accent : Theme.textSecondary)
                                .frame(width: 20)
                                .accessibilityHidden(true)
                            Text(verbatim: row.name)
                                .font(LucinaTokens.Fonts.subheadline)
                                .foregroundStyle(Theme.text)
                                .lineLimit(1)
                                .truncationMode(.middle)
                            Spacer(minLength: 0)
                            if let size = row.size {
                                Text(verbatim: DriveFormat.bytes(size))
                                    .font(LucinaTokens.Fonts.caption)
                                    .foregroundStyle(Theme.textSecondary)
                            }
                        }
                        .padding(.leading, CGFloat(min(row.depth, 12)) * 14)
                    }
                } header: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(summary(archive))
                        if archive.truncated {
                            Text("Only the first \(archive.entries.count) entries are listed.")
                        }
                    }
                    .textCase(nil)
                }
            }
            .lucinaList()
        }
    }

    private func summary(_ archive: DriveArchive) -> String {
        let count = archive.total ?? archive.entries.count
        var parts = [String(localized: "\(count) entries")]
        if let format = archive.format, !format.isEmpty { parts.append(format.uppercased()) }
        if let size = item.size { parts.append(DriveFormat.bytes(Int64(size))) }
        return parts.joined(separator: " · ")
    }

    private func load() async {
        phase = .loading
        do {
            let archive = try await store.api().archive(item: item)
            phase = .ready(archive, DriveArchiveTree.rows(archive.entries))
        } catch is CancellationError {
        } catch {
            phase = DriveFiles.isUnsupported(error) ? .unsupported : .failed(error.localizedDescription)
        }
    }
}

// MARK: - Other files

/// Name, type, size, modified, folder; Download and Share.
struct DriveFileInfo: View {
    @Environment(\.dismiss) private var dismiss
    let item: GalleryItem

    var body: some View {
        Form {
            Section {
                HStack(spacing: LucinaTokens.Space.md) {
                    DriveFileIcon(item: item, size: 56)
                    Text(verbatim: item.driveName)
                        .font(LucinaTokens.Fonts.headline)
                        .foregroundStyle(Theme.text)
                        .lineLimit(3)
                }
            }
            Section {
                LabeledContent("Type", value: typeLabel)
                if let size = item.size { LabeledContent("Size", value: DriveFormat.bytes(Int64(size))) }
                if let date = item.drive?.modifiedDate ?? item.date {
                    LabeledContent("Modified", value: date.formatted(date: .abbreviated, time: .shortened))
                }
                if let location = DriveFormat.location(item) { LabeledContent("Folder", value: location) }
            }
            Section {
                DriveFileActions(item: item, labeled: true)
                    .frame(maxWidth: .infinity)
            }
        }
        .lucinaList()
        .navigationTitle("File")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
    }

    private var typeLabel: String {
        let ext = item.drive?.ext ?? DriveFiles.ext(of: item.driveName)
        if !ext.isEmpty { return ext.uppercased() }
        return item.mime ?? "—"
    }
}
