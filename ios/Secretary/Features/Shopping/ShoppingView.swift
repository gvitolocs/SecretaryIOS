import SwiftUI

struct ShoppingView: View {
    @Environment(ShoppingStore.self) private var store
    @State private var newItem = ""
    @AppStorage("secretary.shopping.boughtExpanded") private var boughtExpanded = true
    @FocusState private var addFocused: Bool

    var body: some View {
        List {
            Section {
                HStack(spacing: 10) {
                    Image(systemName: "plus.circle.fill")
                        .foregroundStyle(Theme.accent)
                        .font(.title3)
                        .accessibilityHidden(true)
                    TextField("Add an item", text: $newItem)
                        .submitLabel(.done)
                        .focused($addFocused)
                        .onSubmit(add)
                        .accessibilityIdentifier("shoppingAddField")
                    if store.isAdding {
                        ProgressView()
                    } else if !newItem.trimmingCharacters(in: .whitespaces).isEmpty {
                        Button("Add", action: add)
                            .bold()
                            .accessibilityIdentifier("shoppingAddButton")
                    }
                }
            }

            if let error = store.error {
                Section {
                    ErrorBanner(message: error) { store.error = nil }
                        .listRowInsets(EdgeInsets())
                        .listRowBackground(Color.clear)
                }
            }

            if store.loaded && store.items.isEmpty {
                Section {
                    ContentUnavailableView("Nothing to buy",
                                           systemImage: "cart",
                                           description: Text("Add something above, or ask Lucina to put it on the list."))
                        .listRowBackground(Color.clear)
                }
            } else {
                if !store.needed.isEmpty {
                    Section("To buy · \(store.needed.count)") {
                        ForEach(store.needed) { item in row(item) }
                    }
                }
                if !store.bought.isEmpty {
                    Section {
                        if boughtExpanded {
                            ForEach(store.bought) { item in row(item) }
                        }
                    } header: {
                        Button {
                            withAnimation { boughtExpanded.toggle() }
                        } label: {
                            HStack {
                                Text("Bought · \(store.bought.count)")
                                Spacer()
                                Image(systemName: "chevron.right")
                                    .rotationEffect(.degrees(boughtExpanded ? 90 : 0))
                                    .accessibilityHidden(true)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Bought, \(store.bought.count) items")
                        .accessibilityValue(boughtExpanded ? "Expanded" : "Collapsed")
                        .accessibilityHint("Shows or hides bought items")
                        .accessibilityIdentifier("boughtHeader")
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .overlay {
            if !store.loaded && store.isLoading {
                ProgressView("Loading the list…")
            }
        }
        .refreshable { await store.refresh() }
        .navigationTitle("Shopping")
        .task { if !store.loaded { await store.refresh() } }
        .animation(.default, value: store.items)
    }

    private func row(_ item: ShoppingItem) -> some View {
        Button {
            Task { await store.toggle(item) }
        } label: {
            HStack(spacing: 12) {
                Image(systemName: item.purchased ? "checkmark.circle.fill" : "circle")
                    .font(.title3)
                    .foregroundStyle(item.purchased ? Theme.accent : .secondary)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(item.name)
                        .strikethrough(item.purchased)
                        .foregroundStyle(item.purchased ? .secondary : .primary)
                    if let detail = [item.quantity, item.category].compactMap({ $0 }).joined(separator: " · ").nilIfEmpty {
                        Text(detail).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                if store.toggling.contains(item.id) { ProgressView().controlSize(.small) }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!item.hasId)
        .accessibilityLabel(item.name)
        .accessibilityValue(item.purchased ? "Bought" : "To buy")
        .accessibilityHint(item.purchased ? "Double-tap to put it back on the list" : "Double-tap to mark it bought")
        .accessibilityIdentifier("shopping-\(item.name)")
    }

    private func add() {
        let text = newItem
        guard !text.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        Task {
            if await store.add(text) {
                newItem = ""
                addFocused = true
            }
        }
    }
}

extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
