import Foundation

// The app's side of the widgets (Extensions/SecretaryWidgets): where their links land, and the
// sections only the app can fill (training runs in the on-device engine).

extension AppModel {
    /// A `secretary://open/<route>` link or an `OpenSecretaryIntent` from a widget or control.
    func openWidgetRoute(_ route: String) {
        guard phase == .signedIn else {
            pendingWidgetRoute = route
            return
        }
        switch route {
        case "chat/compose":
            selectedTab = .chat
            chat.requestPrefill("")
        case "shopping/add":
            selectedTab = .shopping
            showShoppingAdd = true
        case "weight/log":
            selectedTab = .fitness
            showQuickWeight = true
        case "gym/start":
            selectedTab = .fitness
            Task {
                if gym.view == nil { await gym.refresh() }
                if gym.view?.state.active == nil {
                    _ = await gym.run(.startWorkout(title: String(localized: "Workout"), routineId: nil))
                }
                gym.isPresentingWorkout = true
            }
        case "memos/record":
            openMemo(nil)
            Task { if !memos.recorder.isActive { await memos.record() } }
        case "pokemon":
            selectedTab = .plugins
            showPokemon = true
        default:
            open(route: route)
        }
    }

    /// Called when the tabs appear: the link that launched the app before sign-in was checked.
    func openPendingWidgetRoute() {
        guard let route = pendingWidgetRoute else { return }
        pendingWidgetRoute = nil
        openWidgetRoute(route)
    }

    /// Hands the widgets what the app knows, then asks WidgetKit to redraw them (free while the
    /// app is in the foreground or just leaving it). Called when the app goes to the background.
    func publishWidgets(now: Date = Date()) {
        guard phase == .signedIn else { return }
        if let view = gym.view {
            let week = Calendar.current.dateInterval(of: .weekOfYear, for: now)
            let sessions = view.state.history.compactMap { workout -> TrainingSection.Session? in
                guard let finished = workout.finishedMs else { return nil }
                let day = Date(timeIntervalSince1970: Double(finished) / 1000)
                guard week?.contains(day) ?? false else { return nil }
                return TrainingSection.Session(title: workout.title, day: day)
            }.sorted { $0.day < $1.day }
            let active = view.state.active
            WidgetStore.write(TrainingSection(
                fetchedAt: now, weekCount: max(view.summary.weeklyWorkouts, sessions.count),
                weeklyGoal: view.summary.weeklyGoal > 0 ? view.summary.weeklyGoal : view.state.settings.weeklyGoal,
                sessions: sessions, activeTitle: active?.title,
                activeSince: active.map { Date(timeIntervalSince1970: Double($0.startedMs) / 1000) }))
        }
        WidgetStore.reload()
    }
}
