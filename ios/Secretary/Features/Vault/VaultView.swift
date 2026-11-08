import SwiftUI

// The vault's screens: folders and notes, search, tags, a note with its backlinks, its source to edit, and the graph.
// Links inside a note are `vault-link:` URLs on the attributed text, caught by openURL: they never leave the app.

struct VaultView: View {
    @State private var store = VaultStore()

    var body: some View {
        Group {
            if let error = store.openError {
                EmptyCard(icon: "exclamationmark.triangle", title: "The vault did not open", detail: "\(error)")
                    .padding()
            } else if !store.opened {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if store.status == nil {
                EmptyCard(icon: "folder",
                          title: "No vault on this phone yet",
                          detail: "A vault is a folder of Markdown files, the same one Obsidian opens. It goes in this app's Documents, in a folder called Vault.")
                    .padding()
            } else {
                VaultFolderView(folder: "")
            }
        }
        .environment(store)
        .navigationTitle("Vault")
        .navigationDestination(for: VaultRoute.self) { route in
            Group {
                switch route {
                case let .folder(f): VaultFolderView(folder: f)
                case let .note(p): VaultNoteView(path: p)
                case .tags: VaultTagsView()
                case let .graph(around): VaultGraphView(around: around)
                case let .search(q): VaultSearchResults(query: q)
                }
            }
            .environment(store)
        }
        .task { await store.open() }
    }
}

struct VaultFolderView: View {
    let folder: String
    @Environment(VaultStore.self) private var store
    @State private var listing: VaultListing?
    @State private var query = ""
    @State private var hits: [VaultHit] = []
    @State private var error: String?

    var body: some View {
        List {
            if let error {
                Text(error).foregroundStyle(.secondary)
            }
            if !query.isEmpty {
                Section("Results") {
                    ForEach(hits) { hit in
                        NavigationLink(value: VaultRoute.note(hit.path)) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(verbatim: hit.name).lineLimit(2)
                                if let line = hit.lines.first {
                                    Text(verbatim: line.text).font(.footnote).foregroundStyle(.secondary).lineLimit(2)
                                }
                            }
                        }
                    }
                }
            } else if let listing {
                if folder.isEmpty, let c = store.status?.counts {
                    Section {
                        Text("\(c.notes) notes, \(c.links) links, \(c.unresolved) unresolved")
                            .font(.footnote).foregroundStyle(.secondary)
                        NavigationLink("Tags", value: VaultRoute.tags)
                        NavigationLink("Graph", value: VaultRoute.graph(around: nil))
                    }
                }
                if !listing.folders.isEmpty {
                    Section("Folders") {
                        ForEach(listing.folders, id: \.self) { f in
                            NavigationLink(value: VaultRoute.folder(f)) {
                                Label(VaultCore.name(of: f), systemImage: "folder")
                            }
                        }
                    }
                }
                if !listing.notes.isEmpty {
                    Section("Notes") {
                        ForEach(listing.notes) { n in
                            NavigationLink(n.name, value: VaultRoute.note(n.path))
                        }
                    }
                }
                if !listing.files.isEmpty {
                    Section("Files") {
                        ForEach(listing.files, id: \.self) { f in
                            Text(verbatim: VaultCore.name(of: f)).foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
        .lucinaList()
        .navigationTitle(folder.isEmpty ? String(localized: "Vault") : VaultCore.name(of: folder))
        .searchable(text: $query, prompt: Text("Search the vault"))
        .task(id: query) {
            guard !query.isEmpty else {
                hits = []
                return
            }
            // A pause, so each key stroke does not read every note.
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            struct Hits: Decodable { var hits: [VaultHit] }
            hits = (try? await store.core.ask(["op": "search", "query": query, "limit": 100], as: Hits.self))?.hits ?? []
        }
        .task {
            do {
                listing = try await store.core.ask(["op": "list", "folder": folder])
            } catch {
                self.error = error.localizedDescription
            }
        }
    }
}

struct VaultSearchResults: View {
    let query: String
    @Environment(VaultStore.self) private var store
    @State private var hits: [VaultHit] = []

    var body: some View {
        List(hits) { hit in
            NavigationLink(hit.name, value: VaultRoute.note(hit.path))
        }
        .lucinaList()
        .navigationTitle(query)
        .task {
            struct Hits: Decodable { var hits: [VaultHit] }
            hits = (try? await store.core.ask(["op": "search", "query": query, "limit": 200], as: Hits.self))?.hits ?? []
        }
    }
}

struct VaultTagsView: View {
    @Environment(VaultStore.self) private var store
    @State private var tags: [VaultTag] = []

    var body: some View {
        List(tags) { t in
            NavigationLink(value: VaultRoute.search("tag:\(t.tag)")) {
                HStack {
                    Text(verbatim: "#\(t.tag)")
                    Spacer()
                    Text(verbatim: "\(t.count)").foregroundStyle(.secondary).monospacedDigit()
                }
            }
        }
        .lucinaList()
        .navigationTitle("Tags")
        .task {
            struct Tags: Decodable { var tags: [VaultTag] }
            tags = (try? await store.core.ask(["op": "tags"], as: Tags.self))?.tags ?? []
        }
    }
}

struct VaultNoteView: View {
    let path: String
    @Environment(VaultStore.self) private var store
    @State private var note: VaultNote?
    @State private var error: String?
    @State private var editing = false
    @State private var draft = ""
    @State private var conflict = false
    @State private var askEditing = false
    @State private var pushed: VaultRoute?
    @State private var notice: String?

    var body: some View {
        Group {
            if editing {
                TextEditor(text: $draft)
                    .font(.system(.body, design: .monospaced))
                    .padding(.horizontal, 8)
            } else if let note {
                ScrollView {
                    VStack(alignment: .leading, spacing: 10) {
                        if !note.tags.isEmpty {
                            Text(verbatim: uniqueTags(note).map { "#\($0)" }.joined(separator: "  "))
                                .font(.footnote).foregroundStyle(Theme.accent)
                        }
                        VaultBlocks(blocks: note.blocks, from: note.path, core: store.core)
                        if !note.backlinks.isEmpty {
                            Divider().padding(.top, 12)
                            Text("Backlinks (\(note.backlinks.count))").font(.headline)
                            ForEach(note.backlinks) { b in
                                NavigationLink(value: VaultRoute.note(b.from)) {
                                    Text(verbatim: b.name).frame(maxWidth: .infinity, alignment: .leading)
                                }
                            }
                        }
                    }
                    .padding()
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .environment(\.openURL, OpenURLAction { url in follow(url) })
            } else if let error {
                Text(error).foregroundStyle(.secondary).padding()
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Theme.page)
        .navigationTitle(note?.name ?? VaultCore.name(of: path))
        .navigationBarTitleDisplayMode(.inline)
        .navigationDestination(item: $pushed) { route in
            switch route {
            case let .note(p): VaultNoteView(path: p).environment(store)
            default: EmptyView()
            }
        }
        .toolbar {
            if editing {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { editing = false } }
                ToolbarItem(placement: .confirmationAction) { Button("Save") { Task { await save() } }.bold() }
            } else if note != nil {
                ToolbarItem(placement: .primaryAction) {
                    Button("Edit") {
                        if store.status?.writable == true {
                            draft = note?.text ?? ""
                            editing = true
                        } else {
                            askEditing = true
                        }
                    }
                }
                ToolbarItem(placement: .secondaryAction) {
                    NavigationLink("Graph around this note", value: VaultRoute.graph(around: path))
                }
            }
        }
        .alert("This vault is read-only here", isPresented: $askEditing) {
            Button("Allow editing") { Task { await store.allowEditing() } }
            Button("Stay read-only", role: .cancel) {}
        } message: {
            Text("Saves never overwrite a change they have not seen, and a conflict keeps both as notes.")
        }
        .alert("This note changed on disk", isPresented: $conflict) {
            Button("Keep mine as a new note") { Task { await keepCopy() } }
            Button("Drop mine, read theirs", role: .destructive) {
                editing = false
                Task { await load() }
            }
        } message: {
            Text("Something else wrote it after you opened it. Nothing was overwritten.")
        }
        .overlay(alignment: .bottom) {
            if let notice {
                Text(verbatim: notice).font(.footnote).padding(8)
                    .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 8)).padding()
            }
        }
        .task { await load() }
    }

    private func uniqueTags(_ note: VaultNote) -> [String] {
        var seen = Set<String>()
        return note.tags.filter { seen.insert($0.lowercased()).inserted }
    }

    private func load() async {
        do {
            note = try await store.core.ask(["op": "note", "path": path])
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func save() async {
        guard let note else { return }
        struct Saved: Decodable { var revision: String }
        do {
            let _: Saved = try await store.core.ask(["op": "save", "path": note.path, "text": draft, "revision": note.revision])
            editing = false
            await load()
        } catch VaultError.conflict {
            conflict = true
        } catch {
            notice = error.localizedDescription
        }
    }

    private func keepCopy() async {
        struct Copy: Decodable { var path: String }
        let date = Date().formatted(.iso8601.year().month().day())
        if let c: Copy = try? await store.core.ask(["op": "conflict_copy", "path": path, "text": draft, "date": date, "device": "iphone"]) {
            editing = false
            notice = String(localized: "Your text is in \(c.path)")
            await load()
        }
    }

    private func follow(_ url: URL) -> OpenURLAction.Result {
        guard url.scheme == VaultLink.scheme, let link = VaultLink.decode(url) else {
            // A web link goes to the browser only because it was tapped; nothing here fetches it by itself.
            return .systemAction
        }
        Task {
            struct Resolved: Decodable {
                var dest: String?
                var isNote: Bool
            }
            guard let r: Resolved = try? await store.core.ask(["op": "resolve", "from": path, "link": link]) else { return }
            if let d = r.dest, r.isNote {
                if d != path { pushed = .note(d) }
            } else {
                notice = String(localized: "No note called \(link) yet.")
            }
        }
        return .handled
    }
}

enum VaultLink {
    static let scheme = "vault-link"

    static func url(_ link: String) -> URL? {
        var c = URLComponents()
        c.scheme = scheme
        c.host = "note"
        c.queryItems = [URLQueryItem(name: "l", value: link)]
        return c.url
    }

    static func decode(_ url: URL) -> String? {
        URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "l" }?.value
    }
}

/// A note's blocks, drawn natively. Remote pictures are never fetched, so a note cannot tell a server it was opened.
struct VaultBlocks: View {
    let blocks: [VaultBlock]
    let from: String
    let core: VaultCore

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                view(block)
            }
        }
    }

    private func text(_ spans: [VaultSpan]) -> AttributedString {
        var out = AttributedString()
        for s in spans {
            var a = AttributedString(s.text)
            if s.code == true { a.font = .system(.body, design: .monospaced) }
            var traits = InlinePresentationIntent()
            if s.bold == true { traits.insert(.stronglyEmphasized) }
            if s.italic == true { traits.insert(.emphasized) }
            if !traits.isEmpty { a.inlinePresentationIntent = traits }
            if s.strike == true { a.strikethroughStyle = .single }
            if s.mark == true { a.backgroundColor = Theme.accentSoft }
            if s.tag != nil { a.foregroundColor = Theme.accent }
            if let link = s.link {
                if s.external == true {
                    a.link = URL(string: link)
                } else {
                    a.link = VaultLink.url(link)
                }
            }
            out.append(a)
        }
        return out
    }

    @ViewBuilder
    private func view(_ block: VaultBlock) -> some View {
        switch block {
        case let .heading(level, spans):
            Text(text(spans))
                .font(level <= 1 ? .title2.bold() : level == 2 ? .title3.bold() : .headline)
                .padding(.top, 6)
                .accessibilityAddTraits(.isHeader)
        case let .paragraph(spans):
            Text(text(spans)).fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
        case let .code(_, code):
            ScrollView(.horizontal) {
                Text(verbatim: code).font(.system(.footnote, design: .monospaced)).padding(10)
            }
            .background(Theme.field, in: RoundedRectangle(cornerRadius: 8))
        case let .math(m):
            Text(verbatim: m).font(.system(.footnote, design: .monospaced)).foregroundStyle(.secondary)
        case let .html(h):
            Text(verbatim: h).font(.system(.footnote, design: .monospaced)).foregroundStyle(.secondary)
        case let .quote(inner):
            AnyView(VaultBlocks(blocks: inner, from: from, core: core))
                .padding(.leading, 10)
                .overlay(alignment: .leading) { Rectangle().fill(Theme.separator).frame(width: 3) }
        case let .callout(kind, title, inner):
            AnyView(VStack(alignment: .leading, spacing: 6) {
                Text(title.isEmpty ? AttributedString(kind.capitalized) : text(title)).font(.headline)
                VaultBlocks(blocks: inner, from: from, core: core)
            })
            .padding(10)
            .background(Theme.accentSoft, in: RoundedRectangle(cornerRadius: 8))
        case let .list(ordered, start, items):
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        if let done = item.task {
                            Image(systemName: done ? "checkmark.square.fill" : "square")
                                .foregroundStyle(done ? Theme.accent : .secondary)
                        } else if ordered {
                            Text(verbatim: "\(start + i).").monospacedDigit().foregroundStyle(Theme.accent)
                        } else {
                            Text(verbatim: "•").foregroundStyle(Theme.accent)
                        }
                        AnyView(VaultBlocks(blocks: item.blocks, from: from, core: core))
                    }
                }
            }
        case let .table(head, rows):
            ScrollView(.horizontal) {
                Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                    GridRow {
                        ForEach(Array(head.enumerated()), id: \.offset) { _, c in Text(text(c)).bold() }
                    }
                    Divider()
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                        GridRow {
                            ForEach(Array(r.enumerated()), id: \.offset) { _, c in Text(text(c)) }
                        }
                    }
                }
            }
        case .rule:
            Divider()
        case let .embed(target, alt, external):
            VaultEmbed(target: target, alt: alt, external: external, from: from, core: core)
        case let .footnote(label, inner):
            HStack(alignment: .firstTextBaseline) {
                Text(verbatim: "[\(label)]").font(.footnote).foregroundStyle(.secondary)
                AnyView(VaultBlocks(blocks: inner, from: from, core: core))
            }
        }
    }
}

struct VaultEmbed: View {
    let target: String
    let alt: String
    let external: Bool
    let from: String
    let core: VaultCore
    @State private var image: UIImage?

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().scaledToFit().clipShape(RoundedRectangle(cornerRadius: 8))
                    .accessibilityLabel(alt.isEmpty ? target : alt)
            } else if external {
                Label("Remote picture not loaded", systemImage: "photo").font(.footnote).foregroundStyle(.secondary)
            } else if let url = VaultLink.url(target) {
                Link(destination: url) { Label(target, systemImage: "doc") }.font(.footnote)
            }
        }
        .task(id: target) {
            guard !external else { return }
            struct Resolved: Decodable { var dest: String? }
            struct File: Decodable { var absolute: String }
            guard let r: Resolved = try? await core.ask(["op": "resolve", "from": from, "link": target]), let d = r.dest,
                  ["png", "jpg", "jpeg", "gif", "webp", "heic"].contains((d as NSString).pathExtension.lowercased()),
                  let f: File = try? await core.ask(["op": "file_path", "path": d]) else { return }
            image = await Task.detached { UIImage(contentsOfFile: f.absolute)?.preparingThumbnail(of: CGSize(width: 1200, height: 1200)) }.value
        }
    }
}

/// The note graph on a Canvas: one picture, dragged and pinched. A tap opens the nearest note.
struct VaultGraphView: View {
    let around: String?
    @Environment(VaultStore.self) private var store
    @State private var graph: VaultGraph?
    @State private var scale: CGFloat = 1
    @State private var offset: CGSize = .zero
    @State private var base: CGFloat = 1
    @State private var baseOffset: CGSize = .zero
    @State private var pushed: VaultRoute?

    var body: some View {
        GeometryReader { geo in
            if let graph {
                let fit = fitScale(graph, geo.size)
                Canvas { ctx, size in
                    let s = fit * scale
                    func at(_ n: VaultNode) -> CGPoint {
                        CGPoint(x: n.x * s + size.width / 2 + offset.width, y: n.y * s + size.height / 2 + offset.height)
                    }
                    var lines = Path()
                    for e in graph.edges where e.count == 2 {
                        lines.move(to: at(graph.nodes[e[0]]))
                        lines.addLine(to: at(graph.nodes[e[1]]))
                    }
                    ctx.stroke(lines, with: .color(Theme.separator), lineWidth: 0.5)
                    for n in graph.nodes {
                        let r = max(1.5, (1 + log2(Double(n.degree) + 1) * 0.8) * min(1.6, scale))
                        let p = at(n)
                        ctx.fill(Path(ellipseIn: CGRect(x: p.x - r, y: p.y - r, width: 2 * r, height: 2 * r)),
                                 with: .color(n.path == around ? Theme.accentFill : Theme.accent))
                    }
                    if s > 0.35 || graph.nodes.count < 60 {
                        for n in graph.nodes.prefix(400) where n.degree > 3 || scale > 2 || graph.nodes.count < 60 {
                            let p = at(n)
                            guard p.x > -40, p.y > -20, p.x < size.width, p.y < size.height else { continue }
                            ctx.draw(Text(verbatim: n.name).font(.caption2).foregroundColor(.secondary), at: CGPoint(x: p.x + 6, y: p.y), anchor: .leading)
                        }
                    }
                }
                .contentShape(Rectangle())
                .gesture(SimultaneousGesture(
                    DragGesture().onChanged { v in
                        offset = CGSize(width: baseOffset.width + v.translation.width, height: baseOffset.height + v.translation.height)
                    }.onEnded { _ in baseOffset = offset },
                    MagnifyGesture().onChanged { v in scale = max(0.3, min(12, base * v.magnification)) }.onEnded { _ in base = scale }
                ))
                .onTapGesture { at in
                    let s = fit * scale
                    let hit = graph.nodes.min { a, b in distance(a, at, s, geo.size) < distance(b, at, s, geo.size) }
                    if let hit, distance(hit, at, s, geo.size) < 30 { pushed = .note(hit.path) }
                }
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(Theme.page)
        .navigationTitle("Graph")
        .navigationBarTitleDisplayMode(.inline)
        .navigationDestination(item: $pushed) { route in
            switch route {
            case let .note(p): VaultNoteView(path: p).environment(store)
            default: EmptyView()
            }
        }
        .task {
            var q: [String: Any] = ["op": "graph"]
            if let around {
                q["around"] = around
                q["depth"] = 2
            }
            graph = try? await store.core.ask(q)
        }
    }

    private func fitScale(_ g: VaultGraph, _ size: CGSize) -> CGFloat {
        let far = g.nodes.reduce(1.0) { max($0, max(abs($1.x), abs($1.y))) }
        return max(0.05, min(size.width, size.height) / 2 / far * 0.9)
    }

    private func distance(_ n: VaultNode, _ p: CGPoint, _ s: CGFloat, _ size: CGSize) -> CGFloat {
        let x = n.x * s + size.width / 2 + offset.width
        let y = n.y * s + size.height / 2 + offset.height
        return hypot(x - p.x, y - p.y)
    }
}
