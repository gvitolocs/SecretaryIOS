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
        case let r where ChatComposeRoute.text(r) != nil:
            // `chat/compose?text=…`: the chat with a message ready to send (the Server plugin's Ask Lucina).
            selectedTab = .chat
            chat.requestPrefill(ChatComposeRoute.text(r) ?? "")
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
        case "approvals":
            Task { await approvals.poll() }
        case let r where r.hasPrefix("approvals/"):
            switch ApprovalLink.parse(r) {
            case let .approve(id, payloadHash)?:
                // The Live Activity's Approve: the card, with Face ID there when it still shows that request.
                Task { await approvals.openFromActivityLink(id: id, shownHash: payloadHash) }
            case let .details(id)?:
                Task { await approvals.open(id: id) }
            case nil:
                Task { await approvals.poll() }
            }
        case "pokemon":
            selectedTab = .plugins
            showPokemon = true
        case let r where r.hasPrefix("map/"):
            // The maps feature's `/map/place?…` (AppModel.open(route:) on maps-ios).
            open(route: "/" + r)
        default:
            open(route: route)
        }
    }

    /// The approvals widget shows what the app just learned (a poll, or an approval push that woke it).
    static func publishApprovals(_ items: [ApprovalItem], now: Date = Date()) {
        let section = ApprovalsSection(fetchedAt: now, items: items.map {
            .init(id: $0.id, requester: $0.requester.displayName, summary: $0.summary, details: $0.details,
                  sensitive: $0.sensitive, payloadHash: $0.payloadHash, expiresAt: $0.expiresAt)
        })
        WidgetStore.write(section)
        WidgetStore.reload([.approvals])
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

/// `chat/compose?text=<percent-encoded>`: the chat with `text` in the field, ready to send.
enum ChatComposeRoute {
    static func route(text: String) -> String {
        "chat/compose?text=" + (text.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? "")
    }

    /// The text of a `chat/compose?text=` route, nil for any other route.
    static func text(_ route: String) -> String? {
        let path = route.hasPrefix("/") ? String(route.dropFirst()) : route
        guard path.hasPrefix("chat/compose?") else { return nil }
        let query = path.dropFirst("chat/compose?".count)
        for pair in query.split(separator: "&") {
            let kv = pair.split(separator: "=", maxSplits: 1).map(String.init)
            if kv.count == 2, kv[0] == "text" { return kv[1].removingPercentEncoding ?? kv[1] }
        }
        return nil
    }
}
