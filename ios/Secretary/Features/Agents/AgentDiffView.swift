import SwiftUI

/// The agent's workspace diff (`GET /v1/agents/workspaces/{id}/diff`), read-only.
struct AgentDiffView: View {
    let model: AgentDetailModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                if model.diffLoading, model.diff == nil {
                    HStack { Spacer(); ProgressView(); Spacer() }
                } else if let error = model.diffError, model.diff == nil {
                    Text(verbatim: error).foregroundStyle(.secondary)
                } else if let diff = model.diff {
                    if diff.files.isEmpty {
                        Text("No changes.").foregroundStyle(.secondary)
                    }
                    if diff.diffTooLarge {
                        Text("The diff is too large to show in full.").font(.footnote).foregroundStyle(Theme.warning)
                    }
                    ForEach(diff.files) { file in
                        AgentDiffFileRow(file: file)
                    }
                }
            }
            .lucinaList()
            .navigationTitle(Text("Changes"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
            .refreshable { await model.loadDiff() }
            .task { await model.loadDiff() }
        }
        .lucinaSheet()
    }
}

private struct AgentDiffFileRow: View {
    let file: AgentDiffFile
    private static let lineLimit = 400

    var body: some View {
        DisclosureGroup {
            switch file.status {
            case "binary": Text("Binary file").font(.footnote).foregroundStyle(.secondary)
            case "too_large": Text("Too large to show").font(.footnote).foregroundStyle(.secondary)
            default:
                let lines = file.hunks.flatMap { hunk in [AgentDiffLine.header("@@ -\(hunk.oldStart) +\(hunk.newStart) @@")] + hunk.lines }
                ScrollView(.horizontal) {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(lines.prefix(Self.lineLimit).enumerated()), id: \.offset) { _, line in
                            Text(verbatim: line.content.isEmpty ? " " : line.content)
                                .font(.caption2.monospaced())
                                .foregroundStyle(Self.color(line.kind))
                                .padding(.horizontal, 4)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(Self.background(line.kind))
                        }
                        if lines.count > Self.lineLimit {
                            Text("… \(lines.count - Self.lineLimit) more lines").font(.caption2).foregroundStyle(.secondary).padding(4)
                        }
                    }
                }
            }
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: file.path).font(.footnote.monospaced()).lineLimit(2).truncationMode(.head)
                HStack(spacing: 8) {
                    if file.isNew { Text("new").foregroundStyle(Theme.positive) }
                    if file.isDeleted { Text("deleted").foregroundStyle(Theme.negative) }
                    Text(verbatim: "+\(file.additions)").foregroundStyle(Theme.positive)
                    Text(verbatim: "−\(file.deletions)").foregroundStyle(Theme.negative)
                }
                .font(.caption.monospacedDigit())
            }
        }
    }

    static func color(_ kind: String) -> Color {
        switch kind {
        case "add": Theme.positive
        case "remove": Theme.negative
        case "header": Theme.info
        default: Theme.text
        }
    }

    static func background(_ kind: String) -> Color {
        switch kind {
        case "add": Theme.positive.opacity(0.12)
        case "remove": Theme.negative.opacity(0.12)
        default: .clear
        }
    }
}

extension AgentDiffLine {
    init(kind: String, content: String) {
        self.kind = kind
        self.content = content
    }

    static func header(_ text: String) -> AgentDiffLine { AgentDiffLine(kind: "header", content: text) }
}
