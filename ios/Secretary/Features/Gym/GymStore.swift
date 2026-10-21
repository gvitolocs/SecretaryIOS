import Foundation
import Observation

/// The gym feature's state. The engine view is the source of truth: the store sends
/// commands through a `GymTransport` and renders whatever view comes back. It holds no
/// training rules of its own.
@Observable
@MainActor
final class GymStore {
    private(set) var view: GymView?
    /// The last clock reading and when it arrived (for the local tick between polls).
    private(set) var clock: GymClock?
    private(set) var clockReceivedAt = Date()
    private(set) var isLoading = false
    /// Number of commands queued or in flight.
    private(set) var pending = 0
    var loadError: String?
    var actionError: String?
    /// The workout sheet shows its own error alert while it is up.
    var isPresentingWorkout = false

    @ObservationIgnored private let transport: GymTransport
    @ObservationIgnored private let queue = SerialQueue()
    @ObservationIgnored private let makeOpId: () -> String
    @ObservationIgnored private let timezone: () -> String
    @ObservationIgnored private var generation = 0
    /// Called with transport failures (401 → sign out, offline → banner).
    @ObservationIgnored var onTransportError: ((APIError) -> Void)?
    @ObservationIgnored var onTransportSuccess: (() -> Void)?

    init(transport: GymTransport,
         makeOpId: @escaping () -> String = { UUID().uuidString.lowercased() },
         timezone: @escaping () -> String = { EngineClock.timezone }) {
        self.transport = transport
        self.makeOpId = makeOpId
        self.timezone = timezone
    }

    var revision: Int? { view?.state.revision }

    // MARK: - Loading

    func refresh() async {
        await queue.run { [weak self] in
            guard let self else { return }
            _ = await self.fetchState()
        }
    }

    /// Loads the snapshot and returns its revision (nil on failure).
    private func fetchState() async -> Int? {
        let mine = generation
        isLoading = view == nil
        defer { if mine == generation { isLoading = false } }
        do {
            let response = try await transport.state(timezone: timezone())
            guard mine == generation else { return nil }
            guard let view = response.view else {
                loadError = response.error ?? "The gym engine sent no data."
                return nil
            }
            apply(view)
            loadError = nil
            onTransportSuccess?()
            return view.state.revision
        } catch {
            guard mine == generation else { return nil }
            let apiError = APIError.wrap(error)
            loadError = apiError.userMessage
            onTransportError?(apiError)
            return nil
        }
    }

    // MARK: - Commands

    /// Runs one user action. Returns the response when the engine accepted it, nil when
    /// it was rejected (the message is in `actionError` unless `quiet`).
    @discardableResult
    func run(_ command: GymCommand, quiet: Bool = false) async -> GymResponse? {
        if command.isQuery { return await query(command) }
        pending += 1
        defer { pending -= 1 }
        if !quiet { actionError = nil }
        let opId = makeOpId()
        return await queue.run { [weak self] () -> GymResponse? in
            guard let self else { return nil }
            return await self.execute(command, opId: opId, quiet: quiet)
        }
    }

    private func execute(_ command: GymCommand, opId: String, quiet: Bool) async -> GymResponse? {
        let mine = generation
        var revision = view?.state.revision
        if revision == nil { revision = await fetchState() }
        guard let startRevision = revision, mine == generation else { return nil }
        let tz = timezone()
        do {
            let outcome = try await EngineRetry.run(
                opId: opId,
                nextOpId: makeOpId,
                revision: startRevision,
                send: { [transport] opId, expected in
                    try await transport.send(EngineRequest(opId: opId, expectedRevision: expected, timezone: tz, command: command))
                },
                refetch: { [weak self] in
                    guard let self, let fresh = await self.fetchState() else { throw CancellationError() }
                    return fresh
                }
            )
            guard mine == generation else { return nil }
            onTransportSuccess?()
            switch outcome {
            case let .success(response):
                if let view = response.view { apply(view) }
                if let clock = response.clock { setClock(clock) }
                return response
            case let .rejected(response):
                if let view = response.view { apply(view) }
                if !quiet { actionError = response.error ?? "The change could not be saved." }
                return nil
            case .conflict:
                if !quiet { actionError = EngineRetry.conflictMessage }
                return nil
            }
        } catch is CancellationError {
            return nil
        } catch {
            guard mine == generation else { return nil }
            let apiError = APIError.wrap(error)
            onTransportError?(apiError)
            if !quiet {
                let unreachable = apiError.isConnectivity || apiError.status == 502 || apiError.status == 504
                actionError = unreachable && apiError.code != "host_not_found"
                    ? "Secretary's server isn't reachable. That change wasn't saved."
                    : apiError.userMessage
            }
            return nil
        }
    }

    /// Queries (`clock`, exports) never change state; failures stay quiet unless asked.
    @discardableResult
    func query(_ command: GymCommand, reportErrors: Bool = false) async -> GymResponse? {
        let mine = generation
        do {
            let response = try await transport.send(
                EngineRequest(opId: makeOpId(), expectedRevision: view?.state.revision ?? 0, timezone: timezone(), command: command))
            guard mine == generation else { return nil }
            if let clock = response.clock { setClock(clock) }
            if let view = response.view { apply(view) }
            if !response.ok {
                if reportErrors { actionError = response.error ?? "That didn't work." }
                return nil
            }
            return response
        } catch {
            if reportErrors, mine == generation { actionError = APIError.wrap(error).userMessage }
            return nil
        }
    }

    func tickClock() async {
        guard view?.state.active != nil else { return }
        await query(.clock)
    }

    func reset() {
        generation += 1
        queue.reset()
        view = nil
        clock = nil
        loadError = nil
        actionError = nil
        isLoading = false
    }

    // MARK: - View helpers (presentation only)

    private func apply(_ view: GymView) {
        self.view = view
        setClock(GymClock(restRemaining: view.restRemaining, activeDuration: view.summary.activeDuration,
                          activeElapsedSeconds: view.summary.activeElapsedSeconds))
    }

    private func setClock(_ clock: GymClock) {
        self.clock = clock
        clockReceivedAt = Date()
    }

    func exercise(_ id: String) -> Exercise? { view?.exercise(id) }

    /// Filters the catalog the engine sent (name, muscle or equipment). This is a list
    /// filter for the picker, not a rule; the engine's catalog order is kept.
    func search(_ query: String) -> [Exercise] {
        let all = view?.state.exercises ?? []
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return all }
        return all.filter {
            $0.name.localizedCaseInsensitiveContains(needle)
                || $0.muscle.localizedCaseInsensitiveContains(needle)
                || $0.equipment.localizedCaseInsensitiveContains(needle)
        }
    }

    func setIsEditable(_ id: String) -> Bool {
        view?.state.active?.exercises.flatMap(\.sets).first { $0.id == id }?.completed == false
    }

    /// Seconds of rest left at `date`, counting down locally from the last clock reading.
    func restRemaining(at date: Date) -> Int {
        guard let clock else { return 0 }
        let elapsed = Int(date.timeIntervalSince(clockReceivedAt).rounded(.down))
        return max(0, clock.restRemaining - max(0, elapsed))
    }

    /// The active workout's duration text at `date`. With `active_elapsed_seconds` it
    /// ticks locally; with an older engine it shows the engine's last label.
    func activeDuration(at date: Date) -> String {
        guard let clock else { return view?.summary.activeDuration ?? "" }
        guard let base = clock.activeElapsedSeconds else { return clock.activeDuration }
        let elapsed = Int(date.timeIntervalSince(clockReceivedAt).rounded(.down))
        return Self.durationText(base + max(0, elapsed))
    }

    /// Same shape as the engine's duration label ("12m 05s", "1h 02m").
    static func durationText(_ seconds: Int) -> String {
        let secs = max(0, seconds)
        if secs >= 3600 {
            return "\(secs / 3600)h \(String(format: "%02d", (secs / 60) % 60))m"
        }
        return "\(secs / 60)m \(String(format: "%02d", secs % 60))s"
    }
}
