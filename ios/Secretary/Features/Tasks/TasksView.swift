import SwiftUI

// Tasks: Todoist's layout on the iPhone (Today with an Overdue group and Reschedule, Inbox,
// Upcoming), round priority checkboxes, date chips with the repeat icon, swipe to complete or
// reschedule, and quick add in Todoist's syntax (it/en/da). Synced with Todoist by the server.

/// Tasks open from a button next to the chat title (like Notes: the tab bar has five tabs).
private struct TasksEntry: ViewModifier {
    @Environment(AppModel.self) private var app

    func body(content: Content) -> some View {
        @Bindable var app = app
        content
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button { app.showTasks = true } label: { Image(systemName: "checkmark.circle") }
                        .accessibilityLabel("Tasks")
                        .accessibilityIdentifier("tasksButton")
                }
            }
            .navigationDestination(isPresented: $app.showTasks) { TasksView() }
    }
}

extension View {
    /// The Tasks button and screen, on the chat.
    func tasksEntry() -> some View { modifier(TasksEntry()) }
}

enum TaskColors {
    static func priority(_ p: Int) -> Color {
        switch p {
        case 1: Color(red: 1.0, green: 0.35, blue: 0.43)
        case 2: Color(red: 1.0, green: 0.65, blue: 0.24)
        case 3: Color(red: 0.29, green: 0.66, blue: 1.0)
        default: Color.secondary
        }
    }

    static func due(_ rel: Int) -> Color {
        switch rel {
        case ..<0: Color(red: 1.0, green: 0.42, blue: 0.5)
        case 0: Color(red: 0.24, green: 0.78, blue: 0.53)
        case 1: Color(red: 1.0, green: 0.62, blue: 0.3)
        case 2...6: Color(red: 0.62, green: 0.48, blue: 1.0)
        default: Color.secondary
        }
    }
}

struct TaskCheck: View {
    let task: TaskItem
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            ZStack {
                Circle()
                    .strokeBorder(TaskColors.priority(task.priority), lineWidth: task.priority < 4 ? 2 : 1.4)
                    .background(Circle().fill(TaskColors.priority(task.priority).opacity(task.checked ? 1 : 0.1)))
                if task.checked {
                    Image(systemName: "checkmark").font(.caption2.weight(.bold)).foregroundStyle(.white)
                }
            }
            .frame(width: 22, height: 22)
            .contentShape(Rectangle().inset(by: -10))
        }
        .buttonStyle(.plain)
        .accessibilityLabel(task.checked ? Text("Mark as not done") : Text("Complete"))
    }
}

struct TaskRow: View {
    let task: TaskItem
    let showProject: Bool
    let onToggle: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            TaskCheck(task: task, action: onToggle).padding(.top, 1)
            VStack(alignment: .leading, spacing: 4) {
                Text(task.content)
                    .font(.body)
                    .strikethrough(task.checked)
                    .foregroundStyle(task.checked ? .secondary : .primary)
                if !task.description.isEmpty {
                    Text(task.description.components(separatedBy: "\n").first ?? "")
                        .font(.footnote).foregroundStyle(.secondary).lineLimit(1)
                }
                meta
            }
            Spacer(minLength: 0)
            if showProject {
                Text(task.projectName + (task.sectionName.map { " / \($0)" } ?? ""))
                    .font(.caption2).foregroundStyle(.tertiary).lineLimit(1)
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var meta: some View {
        let hasMeta = task.due != nil || !task.labels.isEmpty || task.subtasks > 0 || task.pending
        if hasMeta {
            HStack(spacing: 10) {
                if let due = task.due {
                    HStack(spacing: 3) {
                        Image(systemName: "calendar")
                        Text(verbatim: TaskDates.label(due))
                        if due.isRecurring { Image(systemName: "repeat") }
                    }
                    .foregroundStyle(TaskColors.due(due.relDays))
                    .opacity(due.provisional ? 0.7 : 1)
                }
                if task.subtasks > 0 {
                    Label("\(task.subtasksDone)/\(task.subtasks)", systemImage: "arrow.turn.down.right").labelStyle(.titleAndIcon)
                }
                ForEach(task.labels, id: \.self) { l in
                    HStack(spacing: 2) { Image(systemName: "tag"); Text(verbatim: l) }
                }
                if task.pending {
                    Image(systemName: "arrow.triangle.2.circlepath").accessibilityLabel("Not yet in Todoist")
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }
}

struct TasksView: View {
    @Environment(TasksStore.self) private var store
    @State private var quickAdd = ""
    @State private var adding = false
    @State private var picking: TaskItem?
    @State private var reschedulingOverdue = false
    @State private var customDate = Date()
    @State private var customFor: TaskItem??
    @FocusState private var quickFocused: Bool

    var body: some View {
        @Bindable var store = store
        List {
            Section {
                Picker("View", selection: Binding(get: { store.view }, set: { v in Task { await store.show(v) } })) {
                    Text("Today").tag(TaskViewKind.today)
                    Text("Inbox").tag(TaskViewKind.inbox)
                    Text("Upcoming").tag(TaskViewKind.upcoming)
                }
                .pickerStyle(.segmented)
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets())
                .accessibilityIdentifier("tasksViewPicker")
            }
            if let error = store.loadError, store.state == nil {
                Section { Text(error).foregroundStyle(.secondary) }
            }
            if let s = store.state {
                status(s)
                ForEach(s.list.groups) { group in
                    let tasks = store.visible(group)
                    Section {
                        ForEach(tasks) { task in row(task, group: group) }
                    } header: {
                        header(group, count: tasks.count)
                    }
                }
                if s.list.groups.allSatisfy({ store.visible($0).isEmpty }) {
                    Section {
                        VStack(spacing: 8) {
                            Image(systemName: "checkmark.circle").font(.system(size: 44)).foregroundStyle(Theme.accent)
                            Text(store.view == .inbox ? "Your inbox is clear" : "You're all done for today").font(.headline)
                        }
                        .frame(maxWidth: .infinity).padding(.vertical, 24)
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
        .navigationTitle(Text(title))
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                if let g = store.state?.goal {
                    Text(verbatim: "\(g.completedToday)/\(g.dailyGoal)")
                        .font(.footnote.monospacedDigit().weight(.semibold))
                        .foregroundStyle(g.reached ? Color.green : Color.secondary)
                        .accessibilityLabel(Text("Daily goal: \(g.completedToday) of \(g.dailyGoal)"))
                }
            }
        }
        .safeAreaInset(edge: .bottom) { quickAddBar }
        .overlay(alignment: .bottom) { undoBanner }
        .refreshable { await store.refresh() }
        .task { await store.refresh(); await store.flush() }
        .confirmationDialog(Text("Reschedule"), isPresented: Binding(get: { picking != nil || reschedulingOverdue },
                                                                     set: { if !$0 { picking = nil; reschedulingOverdue = false } })) {
            if let today = store.state?.today {
                ForEach(TasksStore.choices(today: today), id: \.date) { c in
                    Button(c.label) { reschedule(to: c.date) }
                }
                Button("Pick a date") { customFor = .some(picking); picking = nil; reschedulingOverdue = false }
            }
        }
        .sheet(isPresented: Binding(get: { customFor != nil }, set: { if !$0 { customFor = nil } })) {
            NavigationStack {
                DatePicker("Date", selection: $customDate, displayedComponents: .date)
                    .datePickerStyle(.graphical)
                    .padding()
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) { Button("Cancel") { customFor = nil } }
                        ToolbarItem(placement: .confirmationAction) {
                            Button("Set") {
                                let target = customFor ?? nil
                                customFor = nil
                                let date = TaskDates.iso(customDate)
                                Task {
                                    if let t = target { await store.run(.reschedule(t.id, date: date)) } else { await store.run(.rescheduleOverdue(date: date)) }
                                }
                            }
                        }
                    }
            }
            .presentationDetents([.medium, .large])
        }
        .alert(Text("Tasks"), isPresented: Binding(get: { store.actionError != nil }, set: { if !$0 { store.actionError = nil } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(store.actionError ?? "")
        }
    }

    private var title: LocalizedStringKey {
        switch store.view {
        case .today: "Today"
        case .inbox: "Inbox"
        case .upcoming: "Upcoming"
        }
    }

    @ViewBuilder private func status(_ s: TasksState) -> some View {
        if !store.outbox.isEmpty {
            Section { Label("Offline: \(store.outbox.count) changes will be sent when you're back online", systemImage: "wifi.slash").font(.footnote) }
        }
        if s.sync.status == "disconnected" {
            Section { Label("Todoist isn't connected. Connect it in Settings → Links.", systemImage: "link").font(.footnote) }
        } else if s.sync.status == "reconnect" {
            Section { Label("Todoist needs reconnecting in Settings → Links.", systemImage: "exclamationmark.triangle").font(.footnote) }
        }
        if !s.conflicts.isEmpty {
            Section("Todoist kept its version") {
                ForEach(s.conflicts) { c in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(c.content).font(.subheadline.weight(.semibold))
                        Text(c.message).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
    }

    @ViewBuilder private func header(_ group: TaskGroup, count: Int) -> some View {
        HStack {
            switch group.kind {
            case "overdue":
                Text("Overdue").foregroundStyle(TaskColors.due(-1))
            case "date":
                Text(verbatim: TaskDates.header(group))
            default:
                Text(verbatim: group.title)
            }
            if count > 0 { Text(verbatim: "\(count)").foregroundStyle(.tertiary) }
            Spacer()
            if group.kind == "overdue" {
                Button("Reschedule") { reschedulingOverdue = true }
                    .font(.footnote.weight(.semibold))
                    .textCase(nil)
                    .accessibilityIdentifier("rescheduleOverdue")
            }
        }
        .textCase(nil)
    }

    private func row(_ task: TaskItem, group: TaskGroup) -> some View {
        TaskRow(task: task, showProject: store.view != .inbox) { Task { await store.toggle(task) } }
            .swipeActions(edge: .leading, allowsFullSwipe: true) {
                Button { Task { await store.toggle(task) } } label: { Label("Complete", systemImage: "checkmark") }
                    .tint(.green)
            }
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                Button(role: .destructive) { Task { await store.delete(task) } } label: { Label("Delete", systemImage: "trash") }
                Button { picking = task } label: { Label("Reschedule", systemImage: "calendar") }
                    .tint(.purple)
            }
    }

    private func reschedule(to date: String) {
        let target = picking
        picking = nil
        let overdue = reschedulingOverdue
        reschedulingOverdue = false
        Task {
            if let t = target {
                await store.run(.reschedule(t.id, date: date))
            } else if overdue {
                await store.run(.rescheduleOverdue(date: date))
            }
        }
    }

    private var quickAddBar: some View {
        HStack(spacing: 10) {
            Image(systemName: "plus.circle.fill").foregroundStyle(Theme.accent).font(.title3)
            TextField("Add task — “Call Marco tomorrow 9am #Work p1”", text: $quickAdd)
                .focused($quickFocused)
                .submitLabel(.done)
                .onSubmit(addTask)
                .accessibilityIdentifier("tasksQuickAdd")
            if !quickAdd.trimmingCharacters(in: .whitespaces).isEmpty {
                Button("Add", action: addTask).bold().disabled(adding)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.regularMaterial, in: Capsule())
        .padding(.horizontal, 12)
        .padding(.bottom, 6)
    }

    private func addTask() {
        let text = quickAdd.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty, !adding else { return }
        adding = true
        Task {
            let ok = await store.run(.quickAdd(text: text, defaultDue: store.view == .today ? "today" : nil))
            adding = false
            if ok { quickAdd = "" }
        }
    }

    @ViewBuilder private var undoBanner: some View {
        if let u = store.undo {
            HStack {
                Text(verbatim: u.text).font(.subheadline)
                Spacer()
                Button("Undo") { Task { await store.performUndo() } }.bold()
                Button { store.undo = nil } label: { Image(systemName: "xmark") }.buttonStyle(.plain).foregroundStyle(.secondary)
                    .accessibilityLabel("Dismiss")
            }
            .padding(14)
            .background(.thickMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .padding(.horizontal, 16)
            .padding(.bottom, 64)
            .task(id: u.text + "\(Date().timeIntervalSince1970)") {
                try? await Task.sleep(for: .seconds(7))
                if store.undo == u { store.undo = nil }
            }
        }
    }
}
