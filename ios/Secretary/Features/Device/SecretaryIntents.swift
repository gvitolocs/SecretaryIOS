import AppIntents
import Foundation

// Siri, Shortcuts, Spotlight and the Action button: "Ask Lucina", "Add to shopping",
// "Start workout", "Log weight", plus a Focus filter that tells Lucina the current Focus.

struct AskLucinaIntent: AppIntent {
    static let title: LocalizedStringResource = "Ask Lucina"
    static let description = IntentDescription("Sends a message to Lucina and returns her answer.")

    @Parameter(title: "Message", requestValueDialog: "What do you want to ask Lucina?")
    var message: String

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<String> & ProvidesDialog {
        let app = AppModel.shared
        guard app.phase != .signedOut, app.client.accessKey?.isEmpty == false else {
            throw IntentFailure.message(String(localized: "Open Secretary and sign in first."))
        }
        guard let reply = await app.chat.ask(message), !reply.isEmpty else {
            throw IntentFailure.message(String(localized: "Lucina didn't answer. Check the chat in Secretary."))
        }
        return .result(value: reply, dialog: IntentDialog(stringLiteral: reply))
    }
}

struct AddToShoppingIntent: AppIntent {
    static let title: LocalizedStringResource = "Add to shopping"
    static let description = IntentDescription("Adds an item to the current shopping list.")

    @Parameter(title: "Item", requestValueDialog: "What should I add?")
    var item: String

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        let app = AppModel.shared
        guard app.client.accessKey?.isEmpty == false else { throw IntentFailure.message(String(localized: "Open Secretary and sign in first.")) }
        let store = app.shoppingV2
        if store.availability == .unknown { await store.load(lang: ShoppingLanguage.preferred) }
        if store.availability == .unavailable {
            guard await app.shopping.add(item) else { throw IntentFailure.message(String(localized: "Couldn't add it to the list.")) }
        } else {
            store.add(item)
            await store.flush()
        }
        return .result(dialog: IntentDialog(stringLiteral: String(localized: "Added \(item) to the list.")))
    }
}

struct StartWorkoutIntent: AppIntent {
    static let title: LocalizedStringResource = "Start workout"
    static let description = IntentDescription("Starts an empty workout in the Gym tab.")
    static let openAppWhenRun = true

    @MainActor
    func perform() async throws -> some IntentResult {
        let app = AppModel.shared
        app.selectedTab = .fitness
        if app.gym.view == nil { await app.gym.refresh() }
        if app.gym.view?.state.active == nil {
            _ = await app.gym.run(.startWorkout(title: String(localized: "Workout"), routineId: nil))
        }
        app.gym.isPresentingWorkout = true
        return .result()
    }
}

struct LogWeightIntent: AppIntent {
    static let title: LocalizedStringResource = "Log weight"
    static let description = IntentDescription("Logs a weigh-in in the Weight tab, in your unit.")

    @Parameter(title: "Weight", requestValueDialog: "How much do you weigh?")
    var weight: Double

    @MainActor
    func perform() async throws -> some IntentResult & ProvidesDialog {
        let app = AppModel.shared
        guard app.client.accessKey?.isEmpty == false else { throw IntentFailure.message(String(localized: "Open Secretary and sign in first.")) }
        if app.weight.view == nil { await app.weight.refresh() }
        let text = String(format: "%.3f", weight)
        guard await app.weight.log(text) else {
            throw IntentFailure.message(app.weight.actionError ?? String(localized: "Couldn't log the weight."))
        }
        let label = app.weight.view?.entries.first.map { app.weight.view?.label(for: $0) ?? "" } ?? text
        return .result(dialog: IntentDialog(stringLiteral: String(localized: "Logged \(label).")))
    }
}

/// "What's next?": the next appointment, with the travel time when the server has one.
struct NextAppointmentIntent: AppIntent {
    static let title: LocalizedStringResource = "Next appointment"
    static let description = IntentDescription("Tells you your next appointment, where it is and when to leave.")

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<String> & ProvidesDialog {
        let app = AppModel.shared
        guard app.client.accessKey?.isEmpty == false else { throw IntentFailure.message(String(localized: "Open Secretary and sign in first.")) }
        let section = try await WidgetFetcher(client: app.client, mediaClient: app.mediaClient).appointment()
        WidgetStore.write(section)
        guard let event = section.next else {
            let text = String(localized: "Nothing else today or tomorrow.")
            return .result(value: text, dialog: IntentDialog(stringLiteral: text))
        }
        let when = event.allDay ? (Calendar.current.isDateInToday(event.start) ? String(localized: "today") : String(localized: "tomorrow"))
            : event.start.formatted(.relative(presentation: .named))
        var text = event.location.map { String(localized: "\(event.title), \(when), at \($0).") } ?? String(localized: "\(event.title), \(when).")
        if let minutes = event.travelMinutes, let leave = event.leaveAt {
            text += " " + String(localized: "It's \(minutes) minutes away: leave at \(leave.formatted(date: .omitted, time: .shortened)).")
        }
        return .result(value: text, dialog: IntentDialog(stringLiteral: text))
    }
}

/// "What's on my shopping list?"
struct ShoppingListIntent: AppIntent {
    static let title: LocalizedStringResource = "Shopping list"
    static let description = IntentDescription("Reads what is still on the shopping list.")

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<[String]> & ProvidesDialog {
        let app = AppModel.shared
        guard app.client.accessKey?.isEmpty == false else { throw IntentFailure.message(String(localized: "Open Secretary and sign in first.")) }
        let section = try await WidgetFetcher(client: app.client, mediaClient: app.mediaClient)
            .shopping(listId: nil, previous: WidgetStore.read(ShoppingSection.self))
        WidgetStore.write(section)
        let names = section.items.map { $0.spec.isEmpty ? $0.name : "\($0.name) (\($0.spec))" }
        let text = names.isEmpty ? String(localized: "The list is empty.")
            : String(localized: "\(names.count) to buy: \(names.formatted(.list(type: .and))).")
        return .result(value: names, dialog: IntentDialog(stringLiteral: text))
    }
}

/// "What do I have to do today?": Todoist tasks and Lucina's reminders due today.
struct TodayTasksIntent: AppIntent {
    static let title: LocalizedStringResource = "Today's tasks"
    static let description = IntentDescription("Reads the tasks and reminders due today.")

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<[String]> & ProvidesDialog {
        let app = AppModel.shared
        guard app.client.accessKey?.isEmpty == false else { throw IntentFailure.message(String(localized: "Open Secretary and sign in first.")) }
        let section = try await WidgetFetcher(client: app.client, mediaClient: app.mediaClient).tasks()
        WidgetStore.write(section)
        let titles = section.items.map(\.title)
        let text = titles.isEmpty ? String(localized: "Nothing due today.")
            : String(localized: "\(titles.count) due today: \(titles.formatted(.list(type: .and))).")
        return .result(value: titles, dialog: IntentDialog(stringLiteral: text))
    }
}

enum IntentFailure: Error, CustomLocalizedStringResourceConvertible {
    case message(String)

    var localizedStringResource: LocalizedStringResource {
        switch self {
        case let .message(text): return LocalizedStringResource(stringLiteral: text)
        }
    }
}

struct RecordMemoIntent: AppIntent {
    static let title: LocalizedStringResource = "Record a memo"
    static let description = IntentDescription("Opens Secretary and starts recording a voice memo. Lucina transcribes and summarises it.")
    static let openAppWhenRun = true

    @MainActor
    func perform() async throws -> some IntentResult {
        let app = AppModel.shared
        guard app.client.accessKey?.isEmpty == false else { throw IntentFailure.message(String(localized: "Open Secretary and sign in first.")) }
        app.openMemo(nil)
        if !app.memos.recorder.isActive { await app.memos.record() }
        return .result()
    }
}

struct SecretaryShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(intent: SendToLucinaIntent(), phrases: ["Send to Lucina with \(.applicationName)", "Share with Lucina in \(.applicationName)"],
                    shortTitle: "Send to Lucina", systemImageName: "paperplane")
        AppShortcut(intent: AskLucinaIntent(), phrases: ["Ask \(.applicationName)", "Ask Lucina in \(.applicationName)"],
                    shortTitle: "Ask Lucina", systemImageName: "bubble.left.and.bubble.right")
        AppShortcut(intent: AddToShoppingIntent(), phrases: ["Add to shopping in \(.applicationName)", "Add to my list in \(.applicationName)"],
                    shortTitle: "Add to shopping", systemImageName: "cart.badge.plus")
        AppShortcut(intent: StartWorkoutIntent(), phrases: ["Start a workout in \(.applicationName)"],
                    shortTitle: "Start workout", systemImageName: "dumbbell")
        AppShortcut(intent: LogWeightIntent(), phrases: ["Log my weight in \(.applicationName)"],
                    shortTitle: "Log weight", systemImageName: "scalemass")
        AppShortcut(intent: RecordMemoIntent(), phrases: ["Record a memo in \(.applicationName)", "New voice memo in \(.applicationName)"],
                    shortTitle: "Record memo", systemImageName: "waveform")
        AppShortcut(intent: NextAppointmentIntent(), phrases: ["What's next in \(.applicationName)", "My next appointment in \(.applicationName)"],
                    shortTitle: "Next appointment", systemImageName: "calendar")
        AppShortcut(intent: ShoppingListIntent(), phrases: ["What's on my shopping list in \(.applicationName)", "Read my list in \(.applicationName)"],
                    shortTitle: "Shopping list", systemImageName: "cart")
        AppShortcut(intent: TodayTasksIntent(), phrases: ["What do I have to do today in \(.applicationName)", "Today's tasks in \(.applicationName)"],
                    shortTitle: "Today's tasks", systemImageName: "checklist")
    }
}

/// Settings → Focus → (a Focus) → Focus Filters → Secretary. Lucina learns the Focus name
/// you type here when that Focus turns on, and "off" when it ends.
struct LucinaFocusFilter: SetFocusFilterIntent {
    static let title: LocalizedStringResource = "Tell Lucina"
    static let description: IntentDescription? = IntentDescription("Lets Lucina know which Focus is on.")

    @Parameter(title: "Focus name", default: "")
    var mode: String

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: mode.isEmpty ? "Tell Lucina" : "Tell Lucina: \(mode)")
    }

    @MainActor
    func perform() async throws -> some IntentResult {
        // When the Focus ends the system calls this with the filter off; `current` is then empty.
        let active = (try? await Self.current)?.mode
        FocusRecorder.record(mode: (active?.isEmpty == false) ? active : nil)
        return .result()
    }
}
