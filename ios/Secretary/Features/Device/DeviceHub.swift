import CoreLocation
import EventKit
import Foundation
import HealthKit
import Observation
#if canImport(AlarmKit)
import AlarmKit
#endif
#if canImport(FamilyControls)
import FamilyControls
#endif

/// The sources Lucina can see or act on. All off until the owner turns one on.
enum DeviceSource: String, CaseIterable, Identifiable, Sendable {
    case alarms, reminders, calendar, health, location, focus, safari, screenTime

    var id: String { rawValue }

    var title: String {
        switch self {
        case .alarms: return String(localized: "Alarms and timers")
        case .reminders: return String(localized: "Reminders")
        case .calendar: return String(localized: "Calendar")
        case .health: return String(localized: "Health")
        case .location: return String(localized: "Places you visit")
        case .focus: return String(localized: "Focus")
        case .safari: return String(localized: "Safari websites")
        case .screenTime: return String(localized: "Screen Time")
        }
    }

    var symbol: String {
        switch self {
        case .alarms: return "alarm"
        case .reminders: return "checklist"
        case .calendar: return "calendar"
        case .health: return "heart.text.square"
        case .location: return "mappin.and.ellipse"
        case .focus: return "moon"
        case .safari: return "safari"
        case .screenTime: return "hourglass"
        }
    }

    var explanation: String {
        switch self {
        case .alarms: return String(localized: "Lucina can set and delete alarms and timers she made (Clock's own alarms are out of reach). Deleting asks you first.")
        case .reminders: return String(localized: "Lucina can add and complete reminders and sees the upcoming ones. Deleting asks you first.")
        case .calendar: return String(localized: "Lucina sees the next week of events and can add or delete events after you confirm.")
        case .health: return String(localized: "Weight from Apple Health goes to the Weight tab, your gym workouts go to Health, and Lucina gets a daily summary (steps, sleep, workouts).")
        case .location: return String(localized: "When you arrive at or leave a place, Lucina learns where and for how long. Needs \u{201C}Always\u{201D} location access.")
        case .focus: return String(localized: "Add the Lucina filter to a Focus in Settings → Focus, so Lucina knows when you are at work, asleep or driving.")
        case .safari: return String(localized: "With the Secretary Safari extension on, Lucina sees the sites (domain and title) you visit in Safari. Other browsers stay private.")
        case .screenTime: return String(localized: "Your Screen Time report inside the app, and a note to Lucina when an app passes a limit you set. The numbers never leave the phone.")
        }
    }
}

/// Which sources are on, and their system permission state.
@Observable
@MainActor
final class DeviceAccess {
    enum Status: Equatable {
        case off, on, needsPermission, denied
        case unavailable(String)
    }

    private(set) var revision = 0 // bumps when anything changes, so views refresh

    func isOn(_ source: DeviceSource) -> Bool {
        UserDefaults.standard.bool(forKey: "secretary.access.\(source.rawValue)")
    }

    func setOn(_ source: DeviceSource, _ on: Bool) {
        UserDefaults.standard.set(on, forKey: "secretary.access.\(source.rawValue)")
        revision += 1
    }

    func touch() { revision += 1 }

    func status(_ source: DeviceSource) -> Status {
        _ = revision
        guard isOn(source) else { return availability(source).map(Status.unavailable) ?? .off }
        switch source {
        case .alarms:
            #if canImport(AlarmKit)
            if #available(iOS 26.0, *) {
                switch AlarmManager.shared.authorizationState {
                case .authorized: return .on
                case .denied: return .denied
                default: return .needsPermission
                }
            }
            #endif
            return .unavailable(String(localized: "Needs iOS 26."))
        case .reminders:
            return Self.eventStatus(EKEventStore.authorizationStatus(for: .reminder))
        case .calendar:
            return Self.eventStatus(EKEventStore.authorizationStatus(for: .event))
        case .health:
            return HKHealthStore.isHealthDataAvailable() ? .on : .unavailable(String(localized: "Health isn't available on this device."))
        case .location:
            switch CLLocationManager().authorizationStatus {
            case .authorizedAlways: return .on
            case .denied, .restricted: return .denied
            default: return .needsPermission
            }
        case .focus:
            return .on
        case .safari:
            return .on
        case .screenTime:
            #if canImport(FamilyControls)
            switch AuthorizationCenter.shared.authorizationStatus {
            case .approved: return .on
            case .denied: return .denied
            default: return .needsPermission
            }
            #else
            return .unavailable(String(localized: "Not available on this device."))
            #endif
        }
    }

    /// Why a source can't be used at all on this build or device, if it can't.
    func availability(_ source: DeviceSource) -> String? {
        switch source {
        case .alarms:
            #if canImport(AlarmKit)
            if #available(iOS 26.0, *) { return nil }
            #endif
            return String(localized: "Needs iOS 26.")
        case .health:
            return HKHealthStore.isHealthDataAvailable() ? nil : String(localized: "Health isn't available on this device.")
        default:
            return nil
        }
    }

    private static func eventStatus(_ status: EKAuthorizationStatus) -> Status {
        switch status {
        case .fullAccess: return .on
        case .denied, .restricted, .writeOnly: return .denied
        default: return .needsPermission
        }
    }
}

// MARK: - HealthKit

@MainActor
final class HealthCollector {
    let store = HKHealthStore()
    private let bridge: DeviceBridge
    private weak var weight: WeightStore?
    private var started = false

    init(bridge: DeviceBridge, weight: WeightStore) {
        self.bridge = bridge
        self.weight = weight
    }

    private var readTypes: Set<HKObjectType> {
        var types: Set<HKObjectType> = [HKObjectType.workoutType()]
        for id in [HKQuantityTypeIdentifier.bodyMass, .stepCount, .activeEnergyBurned] { types.insert(HKQuantityType(id)) }
        types.insert(HKCategoryType(.sleepAnalysis))
        return types
    }

    private var shareTypes: Set<HKSampleType> { [HKQuantityType(.bodyMass), HKObjectType.workoutType()] }

    func requestAuthorization() async throws {
        guard HKHealthStore.isHealthDataAvailable() else { return }
        try await store.requestAuthorization(toShare: shareTypes, read: readTypes)
    }

    func start() async {
        guard HKHealthStore.isHealthDataAvailable(), !started else { return }
        started = true
        await importWeights()
        await reportWorkouts()
        await dailySummary()
    }

    func stop() { started = false }

    // Weight from Health (other apps, smart scales) → the Weight module.
    private func importWeights() async {
        let type = HKQuantityType(.bodyMass)
        let anchorKey = "secretary.health.weightAnchor"
        let samples = await anchored(type, key: anchorKey)
        let ownSource = HKSource.default()
        guard let weight else { return }
        if weight.view == nil { await weight.refresh() }
        let existing = weight.view?.entries ?? []
        for case let sample as HKQuantitySample in samples where sample.sourceRevision.source != ownSource {
            let grams = Int((sample.quantity.doubleValue(for: .gramUnit(with: .kilo)) * 1000).rounded())
            let ms = Int(sample.endDate.timeIntervalSince1970 * 1000)
            // Skip what the Weight tab already has (same time ± 2 min, same grams).
            if existing.contains(where: { abs($0.measuredMs - ms) < 120_000 && abs($0.weightGrams - grams) < 50 }) { continue }
            await weight.run(.logEntry(weightGrams: grams, measuredMs: ms, note: String(localized: "From Apple Health")))
        }
    }

    /// The Weight tab logged an entry: write it to Health too.
    func exportWeight(grams: Int, at date: Date) async {
        guard bridgeAllowsHealth else { return }
        let sample = HKQuantitySample(type: HKQuantityType(.bodyMass), quantity: HKQuantity(unit: .gramUnit(with: .kilo), doubleValue: Double(grams) / 1000),
                                      start: date, end: date)
        try? await store.save(sample)
    }

    /// A finished gym workout: write it to Health as strength training.
    func exportWorkout(start: Date, end: Date) async {
        guard bridgeAllowsHealth, end > start else { return }
        let configuration = HKWorkoutConfiguration()
        configuration.activityType = .traditionalStrengthTraining
        let builder = HKWorkoutBuilder(healthStore: store, configuration: configuration, device: .local())
        do {
            try await builder.beginCollection(at: start)
            try await builder.endCollection(at: end)
            _ = try await builder.finishWorkout()
        } catch {}
    }

    private var bridgeAllowsHealth: Bool { UserDefaults.standard.bool(forKey: "secretary.access.\(DeviceSource.health.rawValue)") }

    // Workouts from Health (Apple Watch runs, other apps) → `workout` events.
    private func reportWorkouts() async {
        let samples = await anchored(HKObjectType.workoutType(), key: "secretary.health.workoutAnchor")
        let ownSource = HKSource.default()
        for case let workout as HKWorkout in samples where workout.sourceRevision.source != ownSource {
            var data: [String: JSONValue] = [
                "activity": .string(Self.activityName(workout.workoutActivityType)),
                "start": .number(workout.startDate.timeIntervalSince1970 * 1000),
                "end": .number(workout.endDate.timeIntervalSince1970 * 1000),
                "minutes": .number((workout.duration / 60).rounded()),
                "source": .string(workout.sourceRevision.source.name),
            ]
            if let energy = workout.statistics(for: HKQuantityType(.activeEnergyBurned))?.sumQuantity() {
                data["kcal"] = .number(energy.doubleValue(for: .kilocalorie()).rounded())
            }
            if let distance = workout.totalDistance { data["meters"] = .number(distance.doubleValue(for: .meter()).rounded()) }
            bridge.record(DeviceEvent(type: .workout, data: data, at: workout.endDate, id: "hk-\(workout.uuid.uuidString)"))
        }
    }

    /// Once a day: yesterday's steps, active energy and sleep.
    private func dailySummary() async {
        let calendar = Calendar.current
        let today = calendar.startOfDay(for: Date())
        let key = "secretary.health.lastSummary"
        guard let yesterday = calendar.date(byAdding: .day, value: -1, to: today),
              UserDefaults.standard.double(forKey: key) < today.timeIntervalSince1970 else { return }
        let steps = await sum(.stepCount, unit: .count(), from: yesterday, to: today)
        let energy = await sum(.activeEnergyBurned, unit: .kilocalorie(), from: yesterday, to: today)
        let sleep = await sleepHours(endingAt: today.addingTimeInterval(12 * 3600))
        var data: [String: JSONValue] = ["date": .string(ISO8601DateFormatter.string(from: yesterday, timeZone: .current, formatOptions: [.withFullDate]))]
        if let steps { data["steps"] = .number(steps.rounded()) }
        if let energy { data["activeKcal"] = .number(energy.rounded()) }
        if let sleep { data["sleepHours"] = .number((sleep * 10).rounded() / 10) }
        bridge.record(DeviceEvent(type: .healthSummary, data: data, id: "hk-summary-\(Int(yesterday.timeIntervalSince1970))"))
        UserDefaults.standard.set(today.timeIntervalSince1970, forKey: key)
    }

    private func sum(_ id: HKQuantityTypeIdentifier, unit: HKUnit, from: Date, to: Date) async -> Double? {
        let predicate = HKQuery.predicateForSamples(withStart: from, end: to)
        return await withCheckedContinuation { continuation in
            let query = HKStatisticsQuery(quantityType: HKQuantityType(id), quantitySamplePredicate: predicate, options: .cumulativeSum) { _, stats, _ in
                continuation.resume(returning: stats?.sumQuantity()?.doubleValue(for: unit))
            }
            store.execute(query)
        }
    }

    private func sleepHours(endingAt end: Date) async -> Double? {
        let predicate = HKQuery.predicateForSamples(withStart: end.addingTimeInterval(-24 * 3600), end: end)
        let samples: [HKSample] = await withCheckedContinuation { continuation in
            let query = HKSampleQuery(sampleType: HKCategoryType(.sleepAnalysis), predicate: predicate, limit: HKObjectQueryNoLimit, sortDescriptors: nil) { _, samples, _ in
                continuation.resume(returning: samples ?? [])
            }
            store.execute(query)
        }
        let asleep = samples.compactMap { $0 as? HKCategorySample }.filter {
            HKCategoryValueSleepAnalysis.allAsleepValues.contains(HKCategoryValueSleepAnalysis(rawValue: $0.value) ?? .awake)
        }
        guard !asleep.isEmpty else { return nil }
        return asleep.reduce(0) { $0 + $1.endDate.timeIntervalSince($1.startDate) } / 3600
    }

    /// New samples since the stored anchor (the first run looks back 30 days).
    private func anchored(_ type: HKSampleType, key: String) async -> [HKSample] {
        let stored = UserDefaults.standard.data(forKey: key).flatMap { try? NSKeyedUnarchiver.unarchivedObject(ofClass: HKQueryAnchor.self, from: $0) }
        let predicate = stored == nil ? HKQuery.predicateForSamples(withStart: Date().addingTimeInterval(-30 * 86_400), end: nil) : nil
        return await withCheckedContinuation { continuation in
            let query = HKAnchoredObjectQuery(type: type, predicate: predicate, anchor: stored, limit: 500) { _, samples, _, anchor, _ in
                if let anchor, let data = try? NSKeyedArchiver.archivedData(withRootObject: anchor, requiringSecureCoding: true) {
                    UserDefaults.standard.set(data, forKey: key)
                }
                continuation.resume(returning: samples ?? [])
            }
            store.execute(query)
        }
    }

    static func activityName(_ type: HKWorkoutActivityType) -> String {
        switch type {
        case .running: return "running"
        case .walking: return "walking"
        case .cycling: return "cycling"
        case .swimming: return "swimming"
        case .traditionalStrengthTraining, .functionalStrengthTraining: return "strength"
        case .yoga: return "yoga"
        case .hiking: return "hiking"
        case .highIntensityIntervalTraining: return "hiit"
        default: return "other"
        }
    }
}

// MARK: - Location visits

@MainActor
final class LocationCollector: NSObject, CLLocationManagerDelegate {
    private let manager = CLLocationManager()
    private let bridge: DeviceBridge
    private let geocoder = CLGeocoder()
    var onAuthorizationChange: (() -> Void)?

    init(bridge: DeviceBridge) {
        self.bridge = bridge
        super.init()
        manager.delegate = self
    }

    func requestAuthorization() {
        // iOS asks "While Using" first; "Always" is offered later by the system.
        if manager.authorizationStatus == .notDetermined {
            manager.requestWhenInUseAuthorization()
        } else {
            manager.requestAlwaysAuthorization()
        }
    }

    func start() {
        guard manager.authorizationStatus == .authorizedAlways || manager.authorizationStatus == .authorizedWhenInUse else { return }
        manager.startMonitoringVisits()
    }

    func stop() { manager.stopMonitoringVisits() }

    nonisolated func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        Task { @MainActor in
            if manager.authorizationStatus == .authorizedWhenInUse { manager.requestAlwaysAuthorization() }
            if UserDefaults.standard.bool(forKey: "secretary.access.\(DeviceSource.location.rawValue)") { self.start() }
            self.onAuthorizationChange?()
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didVisit visit: CLVisit) {
        let lat = visit.coordinate.latitude, lon = visit.coordinate.longitude
        let arrival = visit.arrivalDate, departure = visit.departureDate, accuracy = visit.horizontalAccuracy
        Task { @MainActor in
            guard UserDefaults.standard.bool(forKey: "secretary.access.\(DeviceSource.location.rawValue)") else { return }
            var data: [String: JSONValue] = [
                "lat": .number((lat * 10_000).rounded() / 10_000),
                "lon": .number((lon * 10_000).rounded() / 10_000),
                "accuracyM": .number(accuracy.rounded()),
            ]
            if arrival != .distantPast { data["arrival"] = .number(arrival.timeIntervalSince1970 * 1000) }
            if departure != .distantFuture { data["departure"] = .number(departure.timeIntervalSince1970 * 1000) }
            if let place = try? await self.geocoder.reverseGeocodeLocation(CLLocation(latitude: lat, longitude: lon)).first {
                if let name = place.name { data["place"] = .string(name) }
                if let city = place.locality { data["city"] = .string(city) }
            }
            let when = departure != .distantFuture ? departure : (arrival != .distantPast ? arrival : Date())
            self.bridge.record(DeviceEvent(type: .locationVisit, data: data, at: when))
        }
    }
}

// MARK: - Focus

enum FocusRecorder {
    static let currentKey = "secretary.focus.current"

    @MainActor
    static func record(mode: String?) {
        let value = mode?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty
        UserDefaults.standard.set(value, forKey: currentKey)
        guard UserDefaults.standard.bool(forKey: "secretary.access.\(DeviceSource.focus.rawValue)") else { return }
        AppModel.shared.device.bridge.record(DeviceEvent(type: .focusChange, data: ["mode": value.map(JSONValue.string) ?? .null]))
    }
}

// MARK: - Hub

/// Wires the bridge, the executors and the collectors to the access switches.
@Observable
@MainActor
final class DeviceHub {
    let access = DeviceAccess()
    let bridge: DeviceBridge
    @ObservationIgnored let executors: DeviceExecutors
    @ObservationIgnored let health: HealthCollector
    @ObservationIgnored let location: LocationCollector
    @ObservationIgnored private let eventStore = EKEventStore()
    @ObservationIgnored private var changeObserver: NSObjectProtocol?
    @ObservationIgnored private var changeDebounce: Task<Void, Never>?
    @ObservationIgnored private let client: APIClient

    init(client: APIClient, weight: WeightStore) {
        self.client = client
        bridge = DeviceBridge(client: { DeviceClient(client: client) })
        executors = DeviceExecutors(access: access)
        health = HealthCollector(bridge: bridge, weight: weight)
        location = LocationCollector(bridge: bridge)
        bridge.executor = executors
        location.onAuthorizationChange = { [weak self] in self?.access.touch() }
    }

    /// Turns a source on (asking iOS for permission, with the explanation shown first) or off.
    func set(_ source: DeviceSource, on: Bool) async {
        access.setOn(source, on)
        defer { syncCredentials() }
        guard on else {
            stop(source)
            return
        }
        switch source {
        case .alarms:
            #if canImport(AlarmKit)
            if #available(iOS 26.0, *) { _ = try? await AlarmManager.shared.requestAuthorization() }
            #endif
        case .reminders:
            _ = try? await eventStore.requestFullAccessToReminders()
            observeEventStore()
        case .calendar:
            _ = try? await eventStore.requestFullAccessToEvents()
            observeEventStore()
        case .health:
            try? await health.requestAuthorization()
            await health.start()
        case .location:
            location.requestAuthorization()
            location.start()
        case .focus, .safari:
            break
        case .screenTime:
            #if canImport(FamilyControls)
            try? await AuthorizationCenter.shared.requestAuthorization(for: .individual)
            #endif
        }
        access.touch()
        await bridge.reportState(force: true)
    }

    /// Lets the Safari and Screen Time extensions send their events: the token and server
    /// addresses go to the shared keychain group only while one of those sources is on.
    func syncCredentials() {
        let sources = [DeviceSource.safari, .screenTime].filter { access.isOn($0) }.map(\.rawValue)
        guard !sources.isEmpty, let token = client.accessKey, !token.isEmpty else {
            BridgeCredentials.write(nil)
            return
        }
        let endpoints = [client.baseURL] + client.endpoints.filter { $0 != client.baseURL }
        BridgeCredentials.write(BridgeCredentials(token: token, endpoints: endpoints.map(\.absoluteString), sources: sources))
    }

    private func stop(_ source: DeviceSource) {
        switch source {
        case .location: location.stop()
        case .health: health.stop()
        default: break
        }
    }

    /// On launch and each foreground: start what's on, deliver queued events, refresh state.
    func resume() async {
        syncCredentials()
        if access.isOn(.location) { location.start() }
        if access.isOn(.reminders) || access.isOn(.calendar) { observeEventStore() }
        if access.isOn(.health) { health.stop(); await health.start() }
        await bridge.flush()
        await bridge.reportState()
    }

    private func observeEventStore() {
        guard changeObserver == nil else { return }
        changeObserver = NotificationCenter.default.addObserver(forName: .EKEventStoreChanged, object: eventStore, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.eventStoreChanged() }
        }
    }

    private func eventStoreChanged() {
        changeDebounce?.cancel()
        changeDebounce = Task { [weak self] in
            try? await Task.sleep(for: .seconds(3))
            guard let self, !Task.isCancelled else { return }
            let state = await self.executors.currentState()
            if self.access.isOn(.calendar) {
                self.bridge.record(DeviceEvent(type: .calendarChange, data: ["upcoming": .number(Double(state.events.count))]))
            }
            if self.access.isOn(.reminders) {
                self.bridge.record(DeviceEvent(type: .reminderChange, data: ["open": .number(Double(state.reminders.count))]))
            }
            await self.bridge.reportState(force: true)
        }
    }

    func reset() {
        bridge.reset()
        BridgeCredentials.write(nil)
        for source in DeviceSource.allCases { access.setOn(source, false) }
        location.stop()
        health.stop()
    }
}
