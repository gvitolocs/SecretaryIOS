import Foundation
import Observation

@Observable
@MainActor
final class ShoppingStore {
    private(set) var items: [ShoppingItem] = []
    private(set) var loaded = false
    private(set) var isLoading = false
    private(set) var isAdding = false
    /// Items with a toggle in flight.
    private(set) var toggling: Set<String> = []
    var error: String?

    @ObservationIgnored private let api: () -> SecretaryAPI
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored var onTransportSuccess: (() -> Void)?

    init(api: @escaping () -> SecretaryAPI) {
        self.api = api
    }

    var needed: [ShoppingItem] { items.filter { !$0.purchased } }
    var bought: [ShoppingItem] { items.filter(\.purchased) }

    func refresh() async {
        isLoading = true
        defer { isLoading = false }
        do {
            items = try await api().shoppingList()
            loaded = true
            error = nil
            onTransportSuccess?()
        } catch {
            report(error)
        }
    }

    @discardableResult
    func add(_ rawName: String) async -> Bool {
        let name = rawName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name.count <= 200, !isAdding else { return false }
        isAdding = true
        defer { isAdding = false }
        do {
            if let item = try await api().addShopping(rawName: name) {
                items.removeAll { $0.id == item.id }
                items.insert(item, at: 0)
            } else {
                await refresh()
            }
            error = nil
            onTransportSuccess?()
            return true
        } catch {
            report(error)
            return false
        }
    }

    /// Optimistic toggle with rollback on failure.
    func toggle(_ item: ShoppingItem) async {
        guard item.hasId, !toggling.contains(item.id), let index = items.firstIndex(where: { $0.id == item.id }) else { return }
        let original = items[index]
        let target = !original.purchased
        items[index].purchased = target
        items[index].status = target ? "purchased" : "needed"
        toggling.insert(item.id)
        defer { toggling.remove(item.id) }
        do {
            let updated = try await api().setShoppingStatus(id: item.id, status: target ? "purchased" : "needed")
            if let i = items.firstIndex(where: { $0.id == item.id }) {
                items[i] = items[i].merged(with: updated)
            }
            error = nil
            onTransportSuccess?()
        } catch {
            if let i = items.firstIndex(where: { $0.id == item.id }) { items[i] = original }
            report(error, prefix: "Couldn't update \u{201C}\(original.name)\u{201D}.")
        }
    }

    func reset() {
        items = []
        loaded = false
        error = nil
    }

    private func report(_ error: Error, prefix: String? = nil) {
        let apiError = APIError.wrap(error)
        onTransportError?(apiError)
        let message = apiError.code == "grocery_unavailable" ? "The shopping list is turned off on Lucina's side right now." : apiError.userMessage
        self.error = [prefix, message].compactMap { $0 }.joined(separator: " ")
    }
}
