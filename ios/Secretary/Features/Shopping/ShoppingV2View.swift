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

/// The original Bring! layout (decompiled resources): a 3-column grid of 108 pt tiles with
/// 58 pt icons, red to buy (#f16a6f), teal for recent and the catalog (#6eb8ae), white text;
/// home order: list chooser, to-buy grid, Recently, catalog sections. Emoji icons.
enum Bring {
    static let red = Color("BringRed")
    static let teal = Color("BringTeal")
    static let background = Color("BringBackground")
    static let section = Color("BringSection")
    static let text = Color("BringText")
    static let field = Color("BringField")
    static let tileHeight: CGFloat = 108
    static let iconHeight: CGFloat = 58
    static let corner: CGFloat = 8
    static let gap: CGFloat = 4
    static let margin: CGFloat = 16
    static let columns = Array(repeating: GridItem(.flexible(), spacing: gap), count: 3)
}

struct ShoppingV2View: View {
    @Environment(ShoppingV2Store.self) private var store
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage(ShoppingLanguage.key) private var lang = ShoppingLanguage.preferred
    @AppStorage("secretary.shopping.openSections") private var openSectionsRaw = ""
    @State private var detail: ShopItem?
    @State private var editingList: ListEditor.Mode?
    @State private var showChooser = false
    @State private var showSearch = false
    @State private var tapCount = 0
    @Namespace private var tiles

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                header
                searchButton
                    .padding(.horizontal, Bring.margin)
                    .padding(.bottom, 12)
                if let error = store.error {
                    ErrorBanner(message: error) { store.error = nil }
                        .padding(.horizontal, Bring.margin)
                        .padding(.bottom, 8)
                }
                if let problem = store.syncProblem, !store.outbox.isEmpty {
                    ErrorBanner(message: "\(store.outbox.count) change\(store.outbox.count == 1 ? "" : "s") waiting to sync. \(problem)",
                                systemImage: "icloud.slash")
                        .padding(.horizontal, Bring.margin)
                        .padding(.bottom, 8)
                        .accessibilityIdentifier("shoppingOutbox")
                }
                if store.current == nil {
                    if store.isLoading { ProgressView().frame(maxWidth: .infinity).padding(.top, 60) }
                } else {
                    toBuy
                    recently
                    catalogSections
                }
            }
            .padding(.bottom, 120)
            .animation(.spring(duration: 0.35), value: store.current?.items)
        }
        .background(Bring.background.ignoresSafeArea())
        .toolbar(.hidden, for: .navigationBar)
        .refreshable { await store.load(lang: lang) }
        .overlay(alignment: .bottom) { undoBar }
        .sensoryFeedback(.impact(flexibility: .soft), trigger: tapCount)
        .sheet(item: $detail) { item in ItemDetail(itemId: item.id) }
        .sheet(item: $editingList) { mode in ListEditor(mode: mode) }
        .sheet(isPresented: $showChooser) {
            ListChooser(lang: $lang, onEdit: { mode in
                showChooser = false
                editingList = mode
            })
        }
        .sheet(isPresented: $showSearch) {
            SearchSheet(onAdded: { tapCount += 1 })
        }
        .task { await store.load(lang: lang) }
        .onChange(of: lang) { _, value in Task { await store.loadCatalog(lang: value) } }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { store.resume(); Task { await store.load(lang: lang) } }
        }
    }

    // MARK: Header (list chooser)

    private var header: some View {
        Button { showChooser = true } label: {
            VStack(alignment: .leading, spacing: 6) {
                Text(store.currentList?.theme.emoji ?? "🛒").font(.system(size: 34)).accessibilityHidden(true)
                HStack(spacing: 6) {
                    Text(store.currentList?.name ?? "Shopping")
                        .font(.system(size: 30, weight: .bold, design: .rounded))
                        .lineLimit(1)
                        .minimumScaleFactor(0.6)
                    Image(systemName: "chevron.down").font(.headline.weight(.bold)).accessibilityHidden(true)
                }
                Text(store.needed.isEmpty ? "Nothing to buy" : "\(store.needed.count) to buy")
                    .font(.subheadline.weight(.medium))
                    .opacity(0.8)
            }
            .foregroundStyle(Bring.text)
            .frame(maxWidth: .infinity, minHeight: 148, alignment: .bottomLeading)
            .padding(.horizontal, Bring.margin)
            .padding(.bottom, 14)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("List \(store.currentList?.name ?? ""), \(store.needed.count) to buy")
        .accessibilityHint("Choose another list")
        .accessibilityIdentifier("listChooser")
    }

    private var searchButton: some View {
        Button { showSearch = true } label: {
            HStack(spacing: 10) {
                Image(systemName: "magnifyingglass").accessibilityHidden(true)
                Text("What would you like to buy?")
                Spacer()
            }
            .font(.body.weight(.medium))
            .foregroundStyle(Bring.text.opacity(0.7))
            .padding(.horizontal, 14)
            .frame(height: 44)
            .background(Bring.field, in: RoundedRectangle(cornerRadius: Bring.corner, style: .continuous))
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Add an item")
        .accessibilityIdentifier("shoppingSearch")
    }

    // MARK: Grid areas

    @ViewBuilder private var toBuy: some View {
        if store.needed.isEmpty {
            VStack(spacing: 8) {
                Text("🧺").font(.system(size: 52)).accessibilityHidden(true)
                Text("Your list is empty").font(.system(.title3, design: .rounded).weight(.bold))
                Text("Tap something below, or search to add it.").font(.subheadline).opacity(0.7)
            }
            .foregroundStyle(Bring.text)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 24)
            .accessibilityElement(children: .combine)
        } else {
            LazyVGrid(columns: Bring.columns, spacing: Bring.gap) {
                ForEach(store.needed) { item in
                    BringTile(icon: store.icon(for: item), name: item.name, detail: detailText(item), color: Bring.red)
                        .matchedGeometryEffect(id: item.id, in: tiles)
                        .onTapGesture { tap(item) }
                        .onLongPressGesture(minimumDuration: 0.4) { detail = item }
                        .tileAccessibility(item: item, needed: true) { detail = item }
                }
            }
            .padding(.horizontal, Bring.margin)
        }
    }

    @ViewBuilder private var recently: some View {
        if !store.recent.isEmpty {
            sectionHeader(icon: "🕘", title: "Recently", trailing: AnyView(
                Button("Clear") { store.clearRecent() }.font(.subheadline.weight(.semibold)).foregroundStyle(Bring.text)
            ))
            LazyVGrid(columns: Bring.columns, spacing: Bring.gap) {
                ForEach(store.recent.prefix(18)) { item in
                    BringTile(icon: store.icon(for: item), name: item.name, detail: detailText(item), color: Bring.teal)
                        .matchedGeometryEffect(id: item.id, in: tiles)
                        .onTapGesture { tap(item) }
                        .onLongPressGesture(minimumDuration: 0.4) { detail = item }
                        .tileAccessibility(item: item, needed: false) { detail = item }
                }
            }
            .padding(.horizontal, Bring.margin)
        }
    }

    private var openSections: Set<String> {
        Set(openSectionsRaw.split(separator: ",").map(String.init))
    }

    private func toggleSection(_ id: String) {
        var open = openSections
        if open.contains(id) { open.remove(id) } else { open.insert(id) }
        openSectionsRaw = open.sorted().joined(separator: ",")
    }

    /// Catalog items not on the list right now, by section (tap to add).
    @ViewBuilder private var catalogSections: some View {
        let active = Set((store.current?.items ?? []).filter { $0.status == .needed }.compactMap(\.catalogId))
        ForEach(store.catalog.sections) { section in
            let items = store.catalog.items.filter { $0.section == section.id && !active.contains($0.id) }
            if !items.isEmpty {
                let open = openSections.contains(section.id)
                Button {
                    withAnimation(.easeInOut(duration: 0.2)) { toggleSection(section.id) }
                } label: {
                    HStack(spacing: 10) {
                        Text(section.icon).font(.title3).accessibilityHidden(true)
                        Text(section.name).font(.system(.headline, design: .rounded).weight(.bold))
                        Spacer()
                        Image(systemName: "chevron.down").rotationEffect(.degrees(open ? 180 : 0)).accessibilityHidden(true)
                    }
                    .foregroundStyle(Bring.text)
                    .padding(.horizontal, 14)
                    .frame(height: 52)
                    .background(Bring.section, in: RoundedRectangle(cornerRadius: Bring.corner, style: .continuous))
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .padding(.horizontal, Bring.margin)
                .padding(.top, Bring.gap * 2)
                .accessibilityLabel(section.name)
                .accessibilityValue(open ? "Expanded" : "Collapsed")
                .accessibilityIdentifier("section-\(section.id)")
                if open {
                    LazyVGrid(columns: Bring.columns, spacing: Bring.gap) {
                        ForEach(items) { item in
                            BringTile(icon: item.icon, name: item.displayName(store.search.lang), detail: nil, color: Bring.teal)
                                .onTapGesture {
                                    tapCount += 1
                                    withAnimation(.spring(duration: 0.35)) { store.add(item.displayName(store.search.lang), catalogItem: item) }
                                }
                                .accessibilityElement(children: .ignore)
                                .accessibilityLabel(item.displayName(store.search.lang))
                                .accessibilityHint("Double-tap to add it to the list")
                                .accessibilityAddTraits(.isButton)
                                .accessibilityIdentifier("catalog-\(item.id)")
                        }
                    }
                    .padding(.horizontal, Bring.margin)
                    .padding(.top, Bring.gap)
                }
            }
        }
    }

    private func sectionHeader(icon: String, title: String, trailing: AnyView? = nil) -> some View {
        HStack(spacing: 8) {
            Text(icon).accessibilityHidden(true)
            Text(title).font(.system(.headline, design: .rounded).weight(.bold))
            Spacer()
            trailing
        }
        .foregroundStyle(Bring.text)
        .padding(.horizontal, Bring.margin)
        .padding(.top, 22)
        .padding(.bottom, 8)
        .accessibilityAddTraits(.isHeader)
    }

    private func detailText(_ item: ShopItem) -> String? {
        [item.spec.isEmpty ? nil : item.spec, item.quantityText].compactMap { $0 }.joined(separator: " · ").nilIfEmpty
    }

    private func tap(_ item: ShopItem) {
        tapCount += 1
        withAnimation(.spring(duration: 0.35)) { store.toggle(item) }
    }

    @ViewBuilder private var undoBar: some View {
        if let undo = store.undo {
            HStack {
                Text(undo.message).font(.subheadline).lineLimit(1)
                Spacer()
                Button("Undo") { withAnimation { store.applyUndo() } }
                    .bold()
                    .foregroundStyle(Bring.teal)
                    .accessibilityIdentifier("undo")
            }
            .foregroundStyle(.white)
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            .background(Color(white: 0.15).opacity(0.95), in: RoundedRectangle(cornerRadius: Bring.corner, style: .continuous))
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

/// One Bring! tile: 108 pt high, 58 pt icon 8 pt from the top, name 14 pt bold (shrinks to
/// 12), specification 12 pt, white on the tile colour, 8 pt corners.
struct BringTile: View {
    let icon: String
    let name: String
    let detail: String?
    let color: Color

    var body: some View {
        VStack(spacing: 2) {
            Text(icon)
                .font(.system(size: 46))
                .frame(height: Bring.iconHeight)
                .padding(.top, 8)
                .accessibilityHidden(true)
            Text(name)
                .font(.system(size: 14, weight: .bold, design: .rounded))
                .lineLimit(2)
                .minimumScaleFactor(12.0 / 14.0)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 2)
            if let detail {
                Text(detail)
                    .font(.system(size: 12, weight: .medium, design: .rounded))
                    .lineLimit(1)
                    .padding(.horizontal, 2)
            }
            Spacer(minLength: 4)
        }
        .foregroundStyle(.white)
        .frame(maxWidth: .infinity)
        .frame(height: Bring.tileHeight)
        .background(color, in: RoundedRectangle(cornerRadius: Bring.corner, style: .continuous))
        .contentShape(RoundedRectangle(cornerRadius: Bring.corner, style: .continuous))
        // Fixed geometry like the original: large text sizes don't reflow the grid.
        .dynamicTypeSize(...DynamicTypeSize.xLarge)
    }
}

private extension View {
    func tileAccessibility(item: ShopItem, needed: Bool, details: @escaping () -> Void) -> some View {
        accessibilityElement(children: .ignore)
            .accessibilityLabel([item.name, item.spec.isEmpty ? nil : item.spec, item.quantityText].compactMap { $0 }.joined(separator: ", "))
            .accessibilityValue(needed ? "To buy" : "Bought")
            .accessibilityHint(needed ? "Double-tap to mark it bought" : "Double-tap to put it back on the list")
            .accessibilityAddTraits(.isButton)
            .accessibilityAction(named: "Details", details)
            .accessibilityIdentifier("tile-\(item.name)")
    }
}

/// The search sheet: type, see catalog tiles (red when already on the list), tap to add.
private struct SearchSheet: View {
    @Environment(ShoppingV2Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    let onAdded: () -> Void
    @State private var query = ""
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: Bring.gap) {
                    let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
                    let needed = Set(store.needed.compactMap(\.catalogId))
                    LazyVGrid(columns: Bring.columns, spacing: Bring.gap) {
                        if !trimmed.isEmpty, store.search.exactMatch(trimmed) == nil {
                            BringTile(icon: "✏️", name: trimmed, detail: "Add", color: Bring.teal)
                                .onTapGesture { add(trimmed, nil) }
                                .accessibilityElement(children: .ignore)
                                .accessibilityLabel("Add \(trimmed)")
                                .accessibilityAddTraits(.isButton)
                                .accessibilityIdentifier("suggestion-free")
                        }
                        ForEach(results) { item in
                            let onList = needed.contains(item.id)
                            BringTile(icon: item.icon, name: item.displayName(store.search.lang), detail: nil, color: onList ? Bring.red : Bring.teal)
                                .onTapGesture { add(item.displayName(store.search.lang), item) }
                                .accessibilityElement(children: .ignore)
                                .accessibilityLabel(item.displayName(store.search.lang))
                                .accessibilityValue(onList ? "On the list" : "")
                                .accessibilityAddTraits(.isButton)
                                .accessibilityIdentifier("suggestion-\(item.id)")
                        }
                    }
                    if trimmed.isEmpty {
                        Text("Type a product: \u{201C}latte\u{201D}, \u{201C}bananas\u{201D}, \u{201C}mælk\u{201D}…")
                            .font(.subheadline)
                            .foregroundStyle(Bring.text.opacity(0.6))
                            .padding(.top, 20)
                    }
                }
                .padding(Bring.margin)
            }
            .background(Bring.background.ignoresSafeArea())
            .safeAreaInset(edge: .top) {
                HStack(spacing: 10) {
                    Image(systemName: "magnifyingglass").foregroundStyle(Bring.text.opacity(0.6)).accessibilityHidden(true)
                    TextField("What would you like to buy?", text: $query)
                        .focused($focused)
                        .submitLabel(.done)
                        .autocorrectionDisabled()
                        .onSubmit { addTyped() }
                        .accessibilityIdentifier("shoppingSearchField")
                    if !query.isEmpty {
                        Button { query = "" } label: { Image(systemName: "xmark.circle.fill") }
                            .foregroundStyle(Bring.text.opacity(0.5))
                            .accessibilityLabel("Clear")
                    }
                }
                .padding(.horizontal, 14)
                .frame(height: 44)
                .background(Bring.field, in: RoundedRectangle(cornerRadius: Bring.corner, style: .continuous))
                .padding(.horizontal, Bring.margin)
                .padding(.vertical, 8)
                .background(Bring.background)
            }
            .navigationTitle("Add to \(store.currentList?.name ?? "the list")")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() }.accessibilityIdentifier("searchDone") }
            }
            .onAppear { focused = true }
        }
        .presentationDragIndicator(.visible)
    }

    private var results: [CatalogItem] {
        let used = Set((store.current?.items ?? []).compactMap(\.catalogId))
        return store.search.search(query, limit: 24) { used.contains($0) ? 1 : 0 }
    }

    private func add(_ name: String, _ item: CatalogItem?) {
        store.add(name, catalogItem: item)
        onAdded()
        query = ""
    }

    private func addTyped() {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        add(trimmed, nil)
    }
}

/// The list chooser sheet: lists with their theme, counts, a check on the current one.
private struct ListChooser: View {
    @Environment(ShoppingV2Store.self) private var store
    @Environment(\.dismiss) private var dismiss
    @Binding var lang: String
    let onEdit: (ListEditor.Mode) -> Void

    var body: some View {
        NavigationStack {
            List {
                Section {
                    ForEach(store.lists) { list in
                        Button {
                            Task { await store.select(list.id) }
                            dismiss()
                        } label: {
                            HStack(spacing: 14) {
                                Text(list.theme.emoji)
                                    .font(.title2)
                                    .frame(width: 48, height: 48)
                                    .background(Bring.teal.opacity(0.25), in: RoundedRectangle(cornerRadius: Bring.corner, style: .continuous))
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(list.name).font(.system(.body, design: .rounded).weight(.bold))
                                    Text(list.needed == 1 ? "1 item to buy" : "\(list.needed) items to buy").font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                if list.id == store.currentListId {
                                    Image(systemName: "checkmark").foregroundStyle(Bring.teal).accessibilityLabel("Current list")
                                }
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .swipeActions {
                            Button("Edit") { onEdit(.edit(list)) }.tint(Bring.teal)
                        }
                        .accessibilityIdentifier("list-\(list.id)")
                    }
                }
                Section {
                    Button { onEdit(.create) } label: { Label("Create a new list", systemImage: "plus") }
                    if let list = store.currentList {
                        Button { onEdit(.edit(list)) } label: { Label("Edit \u{201C}\(list.name)\u{201D}", systemImage: "pencil") }
                    }
                }
                Section("Catalog language") {
                    Picker("Language", selection: $lang) {
                        ForEach(ShoppingLanguage.all, id: \.self) { Text(ShoppingLanguage.title($0)).tag($0) }
                    }
                    .pickerStyle(.segmented)
                }
            }
            .navigationTitle("Lists")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
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
        .presentationDragIndicator(.visible)
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
