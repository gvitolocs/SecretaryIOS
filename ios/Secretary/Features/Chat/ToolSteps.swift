import SwiftUI

/// Lucina's tool calls in the chat ("🔧 drive {…} → ok", `meta.kind == "tool"`) are steps of her turn, not
/// messages: a run of them between two messages shows as one quiet line, "Worked on Drive · 4 steps", that opens
/// to the steps. The web chat does the same (web/src/lib/toolSteps.ts).
enum ToolSteps {
    /// A transcript entry: a message, or a run of tool steps.
    enum Entry: Identifiable, Equatable {
        case item(TimelineItem)
        case tools([TimelineItem])

        var id: String {
            switch self {
            case let .item(item): return item.id
            case let .tools(steps): return "tools-" + (steps.first?.id ?? "")
            }
        }
    }

    static func isStep(_ item: TimelineItem) -> Bool {
        item.role == .lucina && item.meta?.kind == "tool"
    }

    /// Items with every run of consecutive tool steps folded into one entry.
    static func entries(_ items: [TimelineItem]) -> [Entry] {
        var out: [Entry] = []
        var run: [TimelineItem] = []
        for item in items {
            if isStep(item) {
                run.append(item)
                continue
            }
            if !run.isEmpty { out.append(.tools(run)); run = [] }
            out.append(.item(item))
        }
        if !run.isEmpty { out.append(.tools(run)) }
        return out
    }

    private static let names: [String: String] = [
        "drive": "Drive", "media": "Gallery", "gallery": "Gallery", "memo": "Memos", "memos": "Memos",
        "meshy": "3D models", "calendar": "Calendar", "computer": "Computer", "android": "Android",
        "shopping": "Shopping", "reminders": "Reminders", "notes": "Notes", "tasks": "Tasks", "maps": "Maps",
        "secrets": "Secrets",
    ]

    /// "Drive" for `drive`, "Web search" for `web_search`.
    static func name(_ tool: String) -> String {
        if let known = names[tool.lowercased()] { return known }
        let words = tool.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ")
            .trimmingCharacters(in: .whitespaces)
        guard let first = words.first else { return tool }
        return first.uppercased() + words.dropFirst()
    }

    /// The tools a run used, in first-use order, and how many steps failed.
    static func summary(_ steps: [TimelineItem]) -> (tools: [String], failed: Int) {
        var tools: [String] = []
        var failed = 0
        for step in steps {
            let tool = name(step.meta?.tool ?? toolInText(step.text) ?? "tool")
            if !tools.contains(tool) { tools.append(tool) }
            if step.meta?.ok == false { failed += 1 }
        }
        return (tools, failed)
    }

    /// A step's text without the wrench: `drive {"action":"list"} → ok`.
    static func text(_ step: TimelineItem) -> String {
        var text = Substring(step.text.trimmingCharacters(in: .whitespaces))
        if text.hasPrefix("🔧") { text = text.dropFirst() }
        return text.trimmingCharacters(in: .whitespaces)
    }

    private static func toolInText(_ text: String) -> String? {
        let name = text.replacingOccurrences(of: "🔧", with: "").trimmingCharacters(in: .whitespaces)
            .prefix { $0.isLetter || $0.isNumber || $0 == "_" || $0 == "-" }
        return name.isEmpty ? nil : String(name)
    }

    static func label(_ steps: [TimelineItem]) -> String {
        let summary = summary(steps)
        let tools = summary.tools.joined(separator: ", ")
        let base = steps.count == 1
            ? String(localized: "Worked on \(tools) · 1 step")
            : String(localized: "Worked on \(tools) · \(steps.count) steps")
        guard summary.failed > 0 else { return base }
        return base + " · " + String(localized: "\(summary.failed) failed")
    }
}

/// One turn's tool calls as a single quiet line that opens to the steps.
struct ToolStepsRow: View {
    let steps: [TimelineItem]
    @State private var expanded = false

    var body: some View {
        let failed = ToolSteps.summary(steps).failed > 0
        DisclosureGroup(isExpanded: $expanded) {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(steps) { step in
                    Text(verbatim: ToolSteps.text(step))
                        .font(.caption2.monospaced())
                        .foregroundStyle(step.meta?.ok == false ? Color.red : Theme.textSecondary)
                        .textSelection(.enabled)
                        .accessibilityLabel(step.meta?.ok == false
                            ? ToolSteps.text(step) + " " + String(localized: "(failed)")
                            : ToolSteps.text(step))
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, 4)
        } label: {
            Text(verbatim: ToolSteps.label(steps))
                .font(.caption)
                .foregroundStyle(failed ? Color.red : Theme.textSecondary)
        }
        .tint(Theme.textSecondary)
        .frame(maxWidth: 420, alignment: .leading)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 4)
    }
}
