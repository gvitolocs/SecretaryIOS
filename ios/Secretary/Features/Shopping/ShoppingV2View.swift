import SwiftUI

/// The Shopping tab: v2 tiles when the server has them, else the legacy list.
struct ShoppingTab: View {
    @Environment(ShoppingV2Store.self) private var store

    var body: some View {
        switch store.availability {
        case .unavailable: ShoppingView()
        default: ShoppingV2View()
        }
    }
}

/// Catalog language for names and search: the device language when the catalog has it.
enum ShoppingLanguage {
    static let key = "secretary.shopping.lang"
    static let all = ["it", "en", "da"]

    static var preferred: String {
        let code = Locale.current.language.languageCode?.identifier ?? "it"
        return all.contains(code) ? code : "it"
    }

    static func title(_ code: String) -> String {
        switch code {
        case "en": return "English"
        case "da": return "Dansk"
        default: return "Italiano"
        }
    }
}

struct ShoppingV2View: View {
    @Environment(ShoppingV2Store.self) private var store
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage(ShoppingLanguage.key) private var lang = ShoppingLanguage.preferred
    @State private var query = ""
    @State private var detail: ShopItem?
    @State private var editingList: ListEditor.Mode?
    @State private var tapCount = 0
    @Namespace private var tiles

    private let columns = [GridItem(.adaptive(minimum: 96, maximum: 140), spacing: 10)]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                if let error = store.error {
                    ErrorBanner(message: error) { store.error = nil }
                }
                if let problem = store.syncProblem, !store.outbox.isEmpty {
                    ErrorBanner(message: "\(store.outbox.count) change\(store.outbox.count == 1 ? "" : "s") waiting to sync. \(problem)",
                                systemImage: "icloud.slash")
                        .accessibilityIdentifier("shoppingOutbox")
                }
                if store.current == nil {
                    if store.isLoading {
                        ProgressView("Loading the list…").frame(maxWidth: .infinity).padding(.top, 60)
                    }
                } else {
                    neededSections
                    recentSection
                }
            }
            .padding(16)
            .animation(.spring(duration: 0.35), value: store.current?.items)
        }
        .background(Theme.page)
        .refreshable { await store.load(lang: lang) }
        .navigationTitle(store.currentList.map { "\($0.theme.emoji) \($0.name)" } ?? "Shopping")
        .toolbar { ToolbarItem(placement: .topBarTrailing) { listMenu } }
        .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always),
                    prompt: "Add to \(store.currentList?.name ?? "the list")")
        .searchSuggestions { suggestions }
        .onSubmit(of: .search) { addFromQuery() }
        .overlay(alignment: .bottom) { undoBar }
        .sensoryFeedback(.impact(flexibility: .soft), trigger: tapCount)
        .sheet(item: $detail) { item in ItemDetail(itemId: item.id) }
        .sheet(item: $editingList) { mode in ListEditor(mode: mode) }
        .task { await store.load(lang: lang) }
        .onChange(of: lang) { _, value in Task { await store.loadCatalog(lang: value) } }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { store.resume(); Task { await store.load(lang: lang) } }
        }
    }

    // MARK: Sections

    private var groupedNeeded: [(section: CatalogSection, items: [ShopItem])] {
        let groups = Dictionary(grouping: store.needed) { $0.section ?? ShoppingCatalog.otherSection }
        let other = CatalogSection(id: ShoppingCatalog.otherSection, name: "Other", icon: "🛒", order: Int.max)
        return groups.map { key, items in (store.catalog.section(key) ?? (key == other.id ? other : CatalogSection(id: key, name: key.capitalized, icon: "🛒", order: Int.max - 1)), items) }
            .sorted { $0.section.order < $1.section.order }
    }

    @ViewBuilder private var neededSections: some View {
        if store.needed.isEmpty {
            VStack(spacing: 10) {
                Text("🧺").font(.system(size: 52)).accessibilityHidden(true)
                Text("Nothing to buy").font(.title3.bold())
                Text("Search above to add something, or tap a recent item to put it back.")
                    .font(.subheadline).foregroundStyle(.secondary).multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 28)
            .accessibilityElement(children: .combine)
        }
        ForEach(groupedNeeded, id: \.section.id) { group in
            VStack(alignment: .leading, spacing: 10) {
                Label {
                    Text(group.section.name)
                } icon: {
                    Text(group.section.icon)
                }
                .font(.headline)
                .accessibilityAddTraits(.isHeader)
                LazyVGrid(columns: columns, spacing: 10) {
                    ForEach(group.items) { item in tile(item) }
                }
            }
        }
    }

    @ViewBuilder private var recentSection: some View {
        if !store.recent.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("Recently bought").font(.headline).accessibilityAddTraits(.isHeader)
                    Spacer()
                    Button("Clear") { store.clearRecent() }.font(.subheadline)
                }
                LazyVGrid(columns: columns, spacing: 10) {
                    ForEach(store.recent) { item in tile(item) }
                }
            }
            .padding(.top, 8)
        }
    }

    private func tile(_ item: ShopItem) -> some View {
        let needed = item.status == .needed
        return VStack(spacing: 6) {
            Text(store.icon(for: item))
                .font(.system(size: 40))
                .frame(height: 46)
                .accessibilityHidden(true)
            Text(item.name)
                .font(.footnote.weight(.semibold))
                .lineLimit(2)
                .multilineTextAlignment(.center)
                .minimumScaleFactor(0.8)
            if let detail = [item.spec.isEmpty ? nil : item.spec, item.quantityText].compactMap({ $0 }).joined(separator: " · ").nilIfEmpty {
                Text(detail)
                    .font(.caption2)
                    .lineLimit(1)
                    .opacity(0.85)
            }
        }
        .foregroundStyle(needed ? Color.white : Color.primary)
        .padding(.horizontal, 6)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, minHeight: 108)
        .background(needed ? AnyShapeStyle(Theme.accentFill.gradient) : AnyShapeStyle(Theme.card),
                    in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .matchedGeometryEffect(id: item.id, in: tiles)
        .contentShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
        .onTapGesture {
            tapCount += 1
            withAnimation(.spring(duration: 0.35)) { store.toggle(item) }
        }
        .onLongPressGesture(minimumDuration: 0.4) { detail = item }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel([item.name, item.spec.isEmpty ? nil : item.spec, item.quantityText].compactMap { $0 }.joined(separator: ", "))
        .accessibilityValue(needed ? "To buy" : "Bought")
        .accessibilityHint(needed ? "Double-tap to mark it bought" : "Double-tap to put it back on the list")
        .accessibilityAddTraits(.isButton)
        .accessibilityAction(named: "Details") { detail = item }
        .accessibilityIdentifier("tile-\(item.name)")
    }

    // MARK: Search

    private var results: [CatalogItem] {
        let used = Set((store.current?.items ?? []).compactMap(\.catalogId))
        return store.search.search(query, limit: 12) { used.contains($0) ? 1 : 0 }
    }

    @ViewBuilder private var suggestions: some View {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        ForEach(results) { item in
            Button {
                store.add(item.displayName(store.search.lang), catalogItem: item)
                tapCount += 1
                query = ""
            } label: {
                HStack {
                    Text(item.icon)
                    Text(item.displayName(store.search.lang))
                    Spacer()
                    if let section = store.catalog.section(item.section) {
                        Text(section.name).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
            .accessibilityIdentifier("suggestion-\(item.id)")
        }
        if !trimmed.isEmpty, store.search.exactMatch(trimmed) == nil {
            Button {
                addFromQuery()
            } label: {
                Label("Add \u{201C}\(trimmed)\u{201D}", systemImage: "plus.circle")
            }
            .accessibilityIdentifier("suggestion-free")
        }
    }

    private func addFromQuery() {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        store.add(trimmed)
        tapCount += 1
        query = ""
    }

    // MARK: Lists, undo

    private var listMenu: some View {
        Menu {
            Section("Lists") {
                ForEach(store.lists) { list in
                    Button {
                        Task { await store.select(list.id) }
                    } label: {
                        Label("\(list.theme.emoji) \(list.name)\(list.needed > 0 ? " · \(list.needed)" : "")",
                              systemImage: list.id == store.currentListId ? "checkmark" : "")
                    }
                }
            }
            Button("New list…", systemImage: "plus") { editingList = .create }
            if let list = store.currentList {
                Button("Edit \u{201C}\(list.name)\u{201D}…", systemImage: "pencil") { editingList = .edit(list) }
            }
            Picker("Catalog language", selection: $lang) {
                ForEach(ShoppingLanguage.all, id: \.self) { Text(ShoppingLanguage.title($0)).tag($0) }
            }
        } label: {
            Image(systemName: "list.bullet.rectangle")
        }
        .accessibilityLabel("Lists")
        .accessibilityIdentifier("listMenu")
    }

    @ViewBuilder private var undoBar: some View {
        if let undo = store.undo {
            HStack {
                Text(undo.message).font(.subheadline).lineLimit(1)
                Spacer()
                Button("Undo") { withAnimation { store.applyUndo() } }
                    .bold()
                    .accessibilityIdentifier("undo")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .background(.regularMaterial, in: Capsule())
            .padding(.horizontal, 16)
            .padding(.bottom, 8)
            .transition(.move(edge: .bottom).combined(with: .opacity))
            .task(id: undo.id) {
                try? await Task.sleep(for: .seconds(5))
                if store.undo?.id == undo.id { withAnimation { store.undo = nil } }
            }
        }
    }
}

/// Long-press details: spec chips, quantity, section, move, delete, who changed it last.
private struct ItemDetail: View {
    @Environment(ShoppingV2Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let itemId: String
    @State private var name = ""
    @State private var spec = ""
    @State private var quantity: Double = 0
    @State private var unit = "pz"
    @State private var confirmDelete = false

    private static let chips = ["x2", "x6", "bio", "1 kg", "500 g", "1 l", "grande", "piccolo", "senza lattosio"]
    private static let units = ["pz", "g", "kg", "ml", "l", "conf"]

    private var item: ShopItem? { store.current?.items.first { $0.id == itemId && $0.isActive } }

    var body: some View {
        NavigationStack {
            Form {
                if let item {
                    Section {
                        HStack(spacing: 12) {
                            Text(store.icon(for: item)).font(.system(size: 44)).accessibilityHidden(true)
                            TextField("Name", text: $name).font(.title3.weight(.semibold))
                                .onSubmit { store.rename(item, name) }
                        }
                    }
                    Section("Details") {
                        TextField("e.g. 6, ripe", text: $spec)
                            .onSubmit { store.setSpec(item, spec) }
                            .accessibilityIdentifier("specField")
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack {
                                ForEach(Self.chips, id: \.self) { chip in
                                    let on = specParts.contains(chip)
                                    Button(chip) { toggleChip(chip) }
                                        .buttonStyle(.bordered)
                                        .tint(on ? Theme.accent : .secondary)
                                        .accessibilityAddTraits(on ? .isSelected : [])
                                }
                            }
                        }
                    }
                    Section("Quantity") {
                        Stepper(value: $quantity, in: 0...100_000, step: 1) {
                            Text(quantity > 0 ? "\(quantity.formatted()) \(unit)" : "No quantity")
                        }
                        Picker("Unit", selection: $unit) {
                            ForEach(Self.units, id: \.self) { Text($0).tag($0) }
                        }
                        .disabled(quantity == 0)
                    }
                    Section {
                        Picker("Section", selection: Binding(get: { item.section ?? ShoppingCatalog.otherSection },
                                                             set: { store.setSection(item, $0 == ShoppingCatalog.otherSection ? nil : $0) })) {
                            ForEach(store.catalog.sections) { Text("\($0.icon) \($0.name)").tag($0.id) }
                            Text("🛒 Other").tag(ShoppingCatalog.otherSection)
                        }
                        if store.lists.count > 1 {
                            Menu {
                                ForEach(store.lists.filter { $0.id != store.currentListId }) { list in
                                    Button("\(list.theme.emoji) \(list.name)") {
                                        store.move(item, to: list.id)
                                        dismiss()
                                    }
                                }
                            } label: {
                                Label("Move to another list", systemImage: "arrow.right.square")
                            }
                        }
                        Button("Delete", systemImage: "trash", role: .destructive) { confirmDelete = true }
                    } footer: {
                        Text(lastChanged(item))
                    }
                } else {
                    Text("This item is no longer on the list.").foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Item")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { save(); dismiss() }
                }
            }
            .confirmationDialog("Delete this item?", isPresented: $confirmDelete, titleVisibility: .visible) {
                Button("Delete", role: .destructive) {
                    if let item { store.remove(item) }
                    dismiss()
                }
            }
            .onAppear(perform: load)
            .onDisappear(perform: save)
        }
        .presentationDetents([.medium, .large])
    }

    private var specParts: [String] {
        spec.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
    }

    private func toggleChip(_ chip: String) {
        var parts = specParts
        if let index = parts.firstIndex(of: chip) { parts.remove(at: index) } else { parts.append(chip) }
        spec = parts.joined(separator: ", ")
    }

    private func load() {
        guard let item else { return }
        name = item.name
        spec = item.spec
        quantity = item.quantity ?? 0
        unit = item.unit ?? "pz"
    }

    private func save() {
        guard let item else { return }
        store.rename(item, name)
        store.setSpec(item, spec)
        store.setQuantity(item, quantity > 0 ? quantity : nil, unit: unit)
    }

    private func lastChanged(_ item: ShopItem) -> String {
        let via = TimelineChannel(rawValue: item.updatedBy)?.label ?? (item.updatedBy == "lucina" ? "Lucina" : item.updatedBy.capitalized)
        let when = item.updatedDate.map { $0.formatted(.relative(presentation: .named)) }
        switch (via.isEmpty, when) {
        case (false, let when?): return "Last changed via \(via), \(when)."
        case (false, nil): return "Last changed via \(via)."
        case (true, let when?): return "Last changed \(when)."
        default: return ""
        }
    }
}

/// Create or edit a list: name and theme.
private struct ListEditor: View {
    enum Mode: Identifiable {
        case create
        case edit(ShopList)
        var id: String {
            switch self {
            case .create: return "create"
            case let .edit(list): return list.id
            }
        }
    }

    @Environment(ShoppingV2Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let mode: Mode
    @State private var name = ""
    @State private var theme: ListTheme = .grocery
    @State private var confirmDelete = false

    var body: some View {
        NavigationStack {
            Form {
                TextField("Name", text: $name)
                Section("Theme") {
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 64))], spacing: 10) {
                        ForEach(ListTheme.allCases, id: \.self) { option in
                            Button {
                                theme = option
                            } label: {
                                VStack(spacing: 4) {
                                    Text(option.emoji).font(.system(size: 28))
                                    Text(option.title).font(.caption2)
                                }
                                .frame(maxWidth: .infinity, minHeight: 64)
                                .background(theme == option ? Theme.accent.opacity(0.2) : Theme.field,
                                            in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel(option.title)
                            .accessibilityAddTraits(theme == option ? .isSelected : [])
                        }
                    }
                    .padding(.vertical, 4)
                }
                if case let .edit(list) = mode, list.id != ShopList.defaultId {
                    Section {
                        Button("Delete list", role: .destructive) { confirmDelete = true }
                    }
                }
            }
            .navigationTitle(isCreate ? "New list" : "Edit list")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") { Task { await save() } }
                        .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
            .confirmationDialog("Delete this list and its items?", isPresented: $confirmDelete, titleVisibility: .visible) {
                Button("Delete list", role: .destructive) {
                    if case let .edit(list) = mode { Task { await store.deleteList(list.id); dismiss() } }
                }
            }
            .onAppear {
                if case let .edit(list) = mode {
                    name = list.name
                    theme = list.theme
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    private var isCreate: Bool {
        if case .create = mode { return true }
        return false
    }

    private func save() async {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        let ok: Bool
        switch mode {
        case .create: ok = await store.createList(name: trimmed, theme: theme)
        case let .edit(list): ok = await store.updateList(list.id, name: trimmed, theme: theme)
        }
        if ok { dismiss() }
    }
}
