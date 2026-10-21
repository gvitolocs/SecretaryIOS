import Foundation
import Observation

@Observable
@MainActor
final class WeightStore {
    private(set) var view: BodyView?
    private(set) var isLoading = false
    private(set) var pending = 0
    var loadError: String?
    var actionError: String?

    @ObservationIgnored private let client: () -> BodyClient
    @ObservationIgnored private let queue = SerialQueue()
    @ObservationIgnored private let makeOpId: () -> String
    @ObservationIgnored private var generation = 0
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored var onTransportSuccess: (() -> Void)?

    init(client: @escaping () -> BodyClient, makeOpId: @escaping () -> String = { UUID().uuidString.lowercased() }) {
        self.client = client
        self.makeOpId = makeOpId
    }

    var unit: BodyUnit { view?.unit ?? .kg }

    func refresh() async {
        await queue.run { [weak self] in _ = await self?.fetchState() }
    }

    private func fetchState() async -> Int? {
        let mine = generation
        isLoading = view == nil
        defer { if mine == generation { isLoading = false } }
        do {
            let response = try await client().state(timezone: EngineClock.timezone)
            guard mine == generation, let view = response.view else { return nil }
            self.view = view
            loadError = nil
            onTransportSuccess?()
            return view.revision
        } catch {
            guard mine == generation else { return nil }
            let apiError = APIError.wrap(error)
            loadError = apiError.userMessage
            onTransportError?(apiError)
            return nil
        }
    }

    @discardableResult
    func run(_ command: BodyCommand) async -> Bool {
        pending += 1
        defer { pending -= 1 }
        actionError = nil
        let opId = makeOpId()
        return await queue.run { [weak self] () -> Bool in
            guard let self else { return false }
            return await self.execute(command, opId: opId)
        }
    }

    private func execute(_ command: BodyCommand, opId: String) async -> Bool {
        let mine = generation
        var revision = view?.revision
        if revision == nil { revision = await fetchState() }
        guard let start = revision, mine == generation else { return false }
        let tz = EngineClock.timezone
        let body = client()
        do {
            let outcome = try await EngineRetry.run(
                opId: opId, nextOpId: makeOpId, revision: start,
                send: { opId, expected in
                    try await body.send(EngineRequest(opId: opId, expectedRevision: expected, timezone: tz, command: command))
                },
                refetch: { [weak self] in
                    guard let self, let fresh = await self.fetchState() else { throw CancellationError() }
                    return fresh
                })
            guard mine == generation else { return false }
            onTransportSuccess?()
            switch outcome {
            case let .success(response):
                if let view = response.view { self.view = view }
                return true
            case let .rejected(response):
                if let view = response.view { self.view = view }
                actionError = response.error ?? String(localized: "The change could not be saved.")
                return false
            case .conflict:
                actionError = EngineRetry.conflictMessage
                return false
            }
        } catch is CancellationError {
            return false
        } catch {
            guard mine == generation else { return false }
            let apiError = APIError.wrap(error)
            onTransportError?(apiError)
            actionError = apiError.isConnectivity && apiError.code != "host_not_found" ? "Secretary's server isn't reachable. That entry wasn't saved." : apiError.userMessage
            return false
        }
    }

    /// Parses the typed weight in the display unit. Returns nil (and sets the error) when it isn't a number.
    func grams(from text: String) -> Int? {
        guard let grams = unit.parse(text) else {
            actionError = String(localized: "Enter a weight like 80.5 (up to three decimals).")
            return nil
        }
        return grams
    }

    func log(_ text: String, at date: Date = Date(), note: String = "") async -> Bool {
        guard let grams = grams(from: text) else { return false }
        return await run(.logEntry(weightGrams: grams, measuredMs: Int(date.timeIntervalSince1970 * 1000), note: note))
    }

    func reset() {
        generation += 1
        queue.reset()
        view = nil
        loadError = nil
        actionError = nil
    }
}
