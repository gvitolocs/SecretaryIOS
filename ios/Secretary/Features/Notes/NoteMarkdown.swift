import Foundation
import SwiftUI

// Markdown for notes, rendered natively: blocks (headings, paragraphs, lists with tasks,
// quotes, fenced code, rules, simple tables) are parsed here; inline marks (bold, italic,
// code, links) go through Foundation's markdown parser. Nothing is ever HTML (SwiftUI
// Text has no HTML), and links keep only http(s) and mailto targets.

enum NoteBlock: Equatable, Hashable {
    case heading(level: Int, text: String)
    case paragraph(String)
    /// Items: (depth, ordinal or nil for bullets, checked for task items, text).
    case list([NoteListItem])
    case quote(String)
    case code(String)
    case rule
    case table(header: [String], rows: [[String]])
}

struct NoteListItem: Equatable, Hashable {
    var depth: Int
    var ordinal: Int?
    var checked: Bool?
    var text: String
}

enum NoteText {
    static func parse(_ source: String) -> [NoteBlock] {
        let lines = source.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
            .replacingOccurrences(of: "\t", with: "    ").components(separatedBy: "\n")
        var blocks: [NoteBlock] = []
        var i = 0
        func blank(_ l: String) -> Bool { l.trimmingCharacters(in: .whitespaces).isEmpty }
        while i < lines.count {
            let line = lines[i]
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty { i += 1; continue }
            if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                let fence = String(trimmed.prefix(3))
                var body: [String] = []
                i += 1
                while i < lines.count, !lines[i].trimmingCharacters(in: .whitespaces).hasPrefix(fence) { body.append(lines[i]); i += 1 }
                i += 1
                blocks.append(.code(body.joined(separator: "\n")))
                continue
            }
            if let h = heading(trimmed) { blocks.append(h); i += 1; continue }
            if isRule(trimmed) { blocks.append(.rule); i += 1; continue }
            if trimmed.hasPrefix(">") {
                var body: [String] = []
                while i < lines.count, lines[i].trimmingCharacters(in: .whitespaces).hasPrefix(">") {
                    var l = lines[i].trimmingCharacters(in: .whitespaces)
                    l.removeFirst()
                    body.append(l.hasPrefix(" ") ? String(l.dropFirst()) : l)
                    i += 1
                }
                blocks.append(.quote(body.joined(separator: "\n")))
                continue
            }
            if listItem(line) != nil {
                var items: [NoteListItem] = []
                while i < lines.count {
                    if let item = listItem(lines[i]) {
                        items.append(item)
                        i += 1
                    } else if !blank(lines[i]), lines[i].hasPrefix("  "), !items.isEmpty {
                        items[items.count - 1].text += " " + lines[i].trimmingCharacters(in: .whitespaces)
                        i += 1
                    } else { break }
                }
                blocks.append(.list(items))
                continue
            }
            if trimmed.contains("|"), i + 1 < lines.count, isTableSeparator(lines[i + 1]) {
                let header = cells(trimmed)
                var rows: [[String]] = []
                i += 2
                while i < lines.count, !blank(lines[i]), lines[i].contains("|") {
                    let r = cells(lines[i])
                    rows.append((0..<header.count).map { $0 < r.count ? r[$0] : "" })
                    i += 1
                }
                blocks.append(.table(header: header, rows: rows))
                continue
            }
            var body = [trimmed]
            i += 1
            while i < lines.count, !blank(lines[i]) {
                let t = lines[i].trimmingCharacters(in: .whitespaces)
                if heading(t) != nil || isRule(t) || t.hasPrefix(">") || t.hasPrefix("```") || listItem(lines[i]) != nil { break }
                body.append(t)
                i += 1
            }
            blocks.append(.paragraph(body.joined(separator: "\n")))
        }
        return blocks
    }

    private static func heading(_ t: String) -> NoteBlock? {
        let hashes = t.prefix(while: { $0 == "#" }).count
        guard (1...6).contains(hashes) else { return nil }
        let rest = t.dropFirst(hashes)
        guard rest.isEmpty || rest.first == " " else { return nil }
        var text = rest.trimmingCharacters(in: .whitespaces)
        while text.hasSuffix("#") { text.removeLast() }
        return .heading(level: hashes, text: text.trimmingCharacters(in: .whitespaces))
    }

    private static func isRule(_ t: String) -> Bool {
        let s = t.replacingOccurrences(of: " ", with: "")
        guard s.count >= 3, let c = s.first, "-*_".contains(c) else { return false }
        return s.allSatisfy { $0 == c }
    }

    static func listItem(_ line: String) -> NoteListItem? {
        let indent = line.prefix(while: { $0 == " " }).count
        let t = line.dropFirst(indent)
        var ordinal: Int?
        var rest: Substring
        if let c = t.first, "-*+".contains(c), t.dropFirst().first == " " {
            if isRule(String(t)) { return nil }
            rest = t.dropFirst(2)
        } else {
            let digits = t.prefix(while: \.isNumber)
            guard !digits.isEmpty, digits.count <= 9 else { return nil }
            let after = t.dropFirst(digits.count)
            guard let p = after.first, p == "." || p == ")", after.dropFirst().first == " " else { return nil }
            ordinal = Int(digits)
            rest = after.dropFirst(2)
        }
        var checked: Bool?
        if rest.hasPrefix("[ ] ") { checked = false; rest = rest.dropFirst(4) }
        else if rest.hasPrefix("[x] ") || rest.hasPrefix("[X] ") { checked = true; rest = rest.dropFirst(4) }
        return NoteListItem(depth: indent / 2, ordinal: ordinal, checked: checked, text: String(rest))
    }

    private static func isTableSeparator(_ line: String) -> Bool {
        let t = line.trimmingCharacters(in: .whitespaces)
        guard t.contains("-") else { return false }
        return t.allSatisfy { "|-: ".contains($0) } && t.filter { $0 == "-" }.count >= 2
    }

    private static func cells(_ line: String) -> [String] {
        var t = line.trimmingCharacters(in: .whitespaces)
        if t.hasPrefix("|") { t.removeFirst() }
        if t.hasSuffix("|") { t.removeLast() }
        return t.components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }
    }

    /// Inline markdown as an attributed string: links only to http(s) and mailto.
    static func inline(_ text: String) -> AttributedString {
        let options = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace,
                                                              failurePolicy: .returnPartiallyParsedIfPossible)
        var attributed = (try? AttributedString(markdown: text, options: options)) ?? AttributedString(text)
        for run in attributed.runs {
            guard let url = run.link else { continue }
            if !isSafe(url) { attributed[run.range].link = nil }
        }
        // Bare URLs become links too.
        let plain = String(attributed.characters)
        if let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) {
            for match in detector.matches(in: plain, range: NSRange(plain.startIndex..., in: plain)) {
                guard let url = match.url, isSafe(url), let r = Range(match.range, in: plain),
                      let lower = AttributedString.Index(r.lowerBound, within: attributed),
                      let upper = AttributedString.Index(r.upperBound, within: attributed) else { continue }
                if attributed[lower..<upper].runs.allSatisfy({ $0.link == nil }) { attributed[lower..<upper].link = url }
            }
        }
        return attributed
    }

    static func isSafe(_ url: URL) -> Bool {
        switch url.scheme?.lowercased() {
        case "http", "https": url.host != nil
        case "mailto": true
        default: false
        }
    }

    /// The first heading when it repeats the title, for the detail view (which shows the title).
    static func dropTitleHeading(_ blocks: [NoteBlock], title: String) -> [NoteBlock] {
        if case let .heading(_, text)? = blocks.first,
           text.trimmingCharacters(in: .whitespaces).lowercased() == title.trimmingCharacters(in: .whitespaces).lowercased() {
            return Array(blocks.dropFirst())
        }
        return blocks
    }

    /// A list excerpt as plain text.
    static func plainExcerpt(_ excerpt: String, title: String) -> String {
        var s = excerpt
        let rules: [(String, String)] = [
            (#"!?\[([^\]]*)\]\([^)]*\)"#, "$1"),
            (#"(^|\s)#{1,6}\s+"#, "$1"),
            (#"(^|\s)>\s+"#, "$1"),
            (#"(^|\s)(?:[-*+]|\d{1,3}[.)])\s+(?:\[[ xX]\]\s+)?"#, "$1"),
            (#"\*\*|__|~~|`+"#, ""),
            (#"(^|[\s(])[*_]+(?=\S)|(\S)[*_]+(?=[\s).,;:!?]|$)"#, "$1$2"),
            (#"\|"#, " "),
            (#"\s+"#, " "),
        ]
        for (pattern, template) in rules {
            s = s.replacingOccurrences(of: pattern, with: template, options: .regularExpression)
        }
        s = s.trimmingCharacters(in: .whitespaces)
        let t = title.trimmingCharacters(in: .whitespaces)
        if !t.isEmpty, s.lowercased().hasPrefix(t.lowercased()) {
            s = String(s.dropFirst(t.count)).trimmingCharacters(in: .whitespaces)
        }
        return s
    }
}

/// The rendered note.
struct NoteMarkdownView: View {
    let blocks: [NoteBlock]
    /// Turns a run of inline markdown into text; the notebook chat swaps in one that draws citation chips.
    var inline: (String) -> AttributedString = NoteText.inline

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                view(for: block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .textSelection(.enabled)
    }

    @ViewBuilder
    private func view(for block: NoteBlock) -> some View {
        switch block {
        case let .heading(level, text):
            Text(inline(text))
                .font(level <= 1 ? .title2.bold() : level == 2 ? .title3.bold() : .headline)
                .padding(.top, 6)
                .accessibilityAddTraits(.isHeader)
        case let .paragraph(text):
            Text(inline(text)).fixedSize(horizontal: false, vertical: true)
        case let .list(items):
            VStack(alignment: .leading, spacing: 6) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Group {
                            if let checked = item.checked {
                                Image(systemName: checked ? "checkmark.square.fill" : "square")
                                    .foregroundStyle(checked ? Theme.accent : .secondary)
                            } else if let n = item.ordinal {
                                Text(verbatim: "\(n).").monospacedDigit().foregroundStyle(Theme.accent)
                            } else {
                                Text(verbatim: "•").foregroundStyle(Theme.accent)
                            }
                        }
                        .accessibilityHidden(item.checked == nil)
                        Text(inline(item.text)).fixedSize(horizontal: false, vertical: true)
                    }
                    .padding(.leading, CGFloat(min(item.depth, 4)) * 16)
                }
            }
        case let .quote(text):
            Text(inline(text))
                .foregroundStyle(.secondary)
                .padding(.leading, 12)
                .overlay(alignment: .leading) {
                    RoundedRectangle(cornerRadius: 2).fill(Theme.accent.opacity(0.6)).frame(width: 3)
                }
        case let .code(text):
            ScrollView(.horizontal, showsIndicators: false) {
                Text(verbatim: text).font(.system(.footnote, design: .monospaced)).padding(12)
            }
            .background(Theme.field, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        case .rule:
            Divider()
        case let .table(header, rows):
            ScrollView(.horizontal, showsIndicators: false) {
                Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 8) {
                    GridRow {
                        ForEach(Array(header.enumerated()), id: \.offset) { _, h in Text(inline(h)).bold() }
                    }
                    Divider()
                    ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                        GridRow {
                            ForEach(Array(row.enumerated()), id: \.offset) { _, cell in Text(inline(cell)) }
                        }
                    }
                }
                .font(.subheadline)
                .padding(12)
            }
            .background(Theme.field.opacity(0.5), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
    }
}
