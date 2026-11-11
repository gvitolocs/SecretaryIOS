import Foundation
import Observation

/// The owner's tracked parcels (`/v1/parcels`): the list, adding, renaming, "check now" and stop tracking.
/// Lucina does the tracking; this keeps what she last said.
@Observable
@MainActor
final class ParcelsStore {
    /// Active parcels first, each group newest change first (`ParcelText.sorted`).
    private(set) var parcels: [Parcel] = []
    /// The local hour of Lucina's daily check and digest.
    private(set) var digestHour: Int?
    private(set) var isLoading = false
    /// The list was fetched at least once since the app started or the last sign-out.
    private(set) var hasLoaded = false
    /// Lucina is unreachable or tracking is off (503 `parcels_unavailable`): a state of the screen, not a failure.
    private(set) var isUnavailable = false
    private(set) var loadError: String?
    var actionError: String?
    /// The server's message on the add sheet (a number it calls invalid, too short or too long).
    var addError: String?
    private(set) var isAdding = false
    /// Parcels a check, rename or removal is in flight for.
    private(set) var busy: Set<String> = []

    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored private let api: () -> ParcelsAPI?
    @ObservationIgnored private var generation = 0

    init(api: @escaping () -> ParcelsAPI?) {
        self.api = api
    }

    nonisolated static let unavailableCode = "parcels_unavailable"

    nonisolated static func isUnavailable(_ error: APIError) -> Bool {
        if case let .server(status, code, _) = error { return status == 503 && code == unavailableCode }
        return false
    }

    /// What the unavailable state says.
    static var unavailableMessage: String { String(localized: "Parcel tracking is not reachable right now.") }

    var active: [Parcel] { parcels.filter(ParcelText.isActive) }
    var finished: [Parcel] { parcels.filter { !ParcelText.isActive($0) } }

    func parcel(id: String) -> Parcel? { parcels.first { $0.id == id } }

    // MARK: Reading

    /// Fetches the list. A newer load wins over an older one that is still in flight.
    func load() async {
        guard let api = api() else { return }
        generation += 1
        let mine = generation
        isLoading = true
        defer { if mine == generation { isLoading = false } }
        do {
            let response = try await api.list()
            guard mine == generation else { return }
            parcels = ParcelText.sorted(response.parcels)
            digestHour = response.digestHour
            hasLoaded = true
            isUnavailable = false
            loadError = nil
        } catch is CancellationError {
            return
        } catch {
            guard mine == generation else { return }
            fail(APIError.wrap(error), loading: true)
        }
    }

    /// One parcel again (the detail screen), without the 10-minute check rule: this only reads what Lucina stored.
    func reload(id: String) async {
        guard let api = api() else { return }
        do {
            let parcel = try await api.get(id)
            put(parcel)
            isUnavailable = false
        } catch is CancellationError {
            return
        } catch {
            let wrapped = APIError.wrap(error)
            if case let .server(404, _, _) = wrapped {
                parcels.removeAll { $0.id == id }
            } else {
                fail(wrapped, loading: false)
            }
        }
    }

    // MARK: Changing

    /// Starts tracking a number. The carrier is detected when `carrier` is empty. True when the parcel is in the list.
    @discardableResult
    func add(number: String, carrier: String = "", label: String = "") async -> Bool {
        guard let api = api(), !isAdding else { return false }
        isAdding = true
        addError = nil
        defer { isAdding = false }
        let request = ParcelAddRequest(number: number.trimmingCharacters(in: .whitespacesAndNewlines),
                                       carrier: Self.nonEmpty(carrier), label: Self.nonEmpty(label))
        do {
            put(try await api.add(request))
            isUnavailable = false
            return true
        } catch is CancellationError {
            return false
        } catch {
            let wrapped = APIError.wrap(error)
            addError = Self.isUnavailable(wrapped) ? Self.unavailableMessage : wrapped.userMessage
            if Self.isUnavailable(wrapped) { isUnavailable = true } else { onTransportError?(wrapped) }
            return false
        }
    }

    /// Checks the carrier now (Lucina answers with the stored parcel when the last check is under 10 minutes old).
    func checkNow(id: String) async {
        guard let api = api(), !busy.contains(id) else { return }
        busy.insert(id)
        defer { busy.remove(id) }
        actionError = nil
        do {
            put(try await api.refresh(id))
            isUnavailable = false
        } catch is CancellationError {
            return
        } catch {
            fail(APIError.wrap(error), loading: false)
        }
    }

    /// Sets the label; an empty one clears it.
    @discardableResult
    func rename(id: String, label: String) async -> Bool {
        guard let api = api(), !busy.contains(id) else { return false }
        busy.insert(id)
        defer { busy.remove(id) }
        actionError = nil
        do {
            put(try await api.rename(id, label: Self.nonEmpty(label)))
            isUnavailable = false
            return true
        } catch is CancellationError {
            return false
        } catch {
            fail(APIError.wrap(error), loading: false)
            return false
        }
    }

    /// Stops tracking and removes the parcel. True when it is gone (a parcel Lucina no longer has counts).
    @discardableResult
    func remove(id: String) async -> Bool {
        guard let api = api(), !busy.contains(id) else { return false }
        busy.insert(id)
        defer { busy.remove(id) }
        actionError = nil
        do {
            try await api.remove(id)
            parcels.removeAll { $0.id == id }
            isUnavailable = false
            return true
        } catch is CancellationError {
            return false
        } catch {
            let wrapped = APIError.wrap(error)
            if case let .server(404, _, _) = wrapped {
                parcels.removeAll { $0.id == id }
                return true
            }
            fail(wrapped, loading: false)
            return false
        }
    }

    /// Sign-out: the list leaves this iPhone.
    func reset() {
        generation += 1
        parcels = []
        digestHour = nil
        isLoading = false
        hasLoaded = false
        isUnavailable = false
        loadError = nil
        actionError = nil
        addError = nil
        isAdding = false
        busy = []
    }

    // MARK: Helpers

    private func put(_ parcel: Parcel) {
        var list = parcels
        if let index = list.firstIndex(where: { $0.id == parcel.id }) {
            list[index] = parcel
        } else {
            list.append(parcel)
        }
        parcels = ParcelText.sorted(list)
    }

    private func fail(_ error: APIError, loading: Bool) {
        let message: String
        if Self.isUnavailable(error) {
            isUnavailable = true
            message = Self.unavailableMessage
        } else {
            message = error.userMessage
            onTransportError?(error)
        }
        if loading { loadError = message } else if !Self.isUnavailable(error) { actionError = message }
    }

    private static func nonEmpty(_ text: String) -> String? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}
