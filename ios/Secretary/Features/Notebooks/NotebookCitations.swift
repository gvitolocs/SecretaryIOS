import Foundation

// Citations in a notebook answer. The model writes `[source:<chunk id>]`, `[insight:<id>]` and
// `[note:<slug>]` inline; the message also carries the resolved `citations` list. This turns the
// tokens into numbered marks (1, 2, 3 in order of first use) that the chat draws as small chips,
// using the resolved data, so no view ever parses ids to show a title or a page.

enum NotebookCitations {
    /// Replaces a citation token with its number(s) in the text handed to the markdown parser.
    /// Private-use characters, which markdown never touches, and which cannot appear in an answer.
    static let open: Character = "\u{E000}"
    static let close: Character = "\u{E001}"

    struct Parsed: Equatable {
        /// The answer with every citation token replaced by a mark: `open 1,2 close`.
        let text: String
        /// The cited passages, numbered from 1 in order of first appearance.
        let sources: [NotebookCitation]

        /// Splits one run of text (a paragraph, a list item) into text and marks.
        func pieces(of run: String) -> [Piece] { NotebookCitations.pieces(of: run) }
    }

    enum Piece: Equatable {
        case text(String)
        /// A mark for the citation numbered `number` (1-based).
        case mark(Int)
    }

    /// `[source:s0a9be3c112-14]`, `[note:rm-class-2]`, `[insight:i1]` and the grouped form
    /// `[source:s1-1, source:s1-2]`.
    private static let token: NSRegularExpression = {
        let ref = #"(?:source|insight|note):[^\]\s,\[]+"#
        let pattern = #"\[\s*("# + ref + #"(?:\s*,\s*"# + ref + #")*)\s*\]"#
        return try! NSRegularExpression(pattern: pattern)
    }()

    static func parse(_ text: String, citations: [NotebookCitation]) -> Parsed {
        let byRef = Dictionary(citations.map { ($0.ref, $0) }, uniquingKeysWith: { first, _ in first })
        var order: [NotebookCitation] = []
        var numbers: [String: Int] = [:]
        func number(for ref: String) -> Int {
            if let n = numbers[ref] { return n }
            // A reference the server did not resolve still gets a chip, with what the id itself says.
            order.append(byRef[ref] ?? NotebookCitation(ref: ref, sourceId: sourceId(of: ref), chunk: chunk(of: ref)))
            numbers[ref] = order.count
            return order.count
        }

        var out = ""
        var last = text.startIndex
        let whole = NSRange(text.startIndex..., in: text)
        for match in token.matches(in: text, range: whole) {
            guard let all = Range(match.range, in: text), let inner = Range(match.range(at: 1), in: text) else { continue }
            out += text[last..<all.lowerBound]
            let refs = text[inner].split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
            let marks = refs.map { String(number(for: $0)) }
            out += "\(open)\(marks.joined(separator: ","))\(close)"
            last = all.upperBound
        }
        out += text[last...]
        return Parsed(text: out, sources: order)
    }

    static func pieces(of run: String) -> [Piece] {
        var pieces: [Piece] = []
        var buffer = ""
        var index = run.startIndex
        while index < run.endIndex {
            let ch = run[index]
            if ch == open, let end = run[index...].firstIndex(of: close) {
                let inside = run[run.index(after: index)..<end]
                let numbers = inside.split(separator: ",").compactMap { Int($0) }
                if !numbers.isEmpty {
                    if !buffer.isEmpty { pieces.append(.text(buffer)); buffer = "" }
                    pieces.append(contentsOf: numbers.map(Piece.mark))
                    index = run.index(after: end)
                    continue
                }
            }
            buffer.append(ch)
            index = run.index(after: index)
        }
        if !buffer.isEmpty { pieces.append(.text(buffer)) }
        return pieces
    }

    /// `source:s0a9be3c112-14` → `s0a9be3c112`.
    static func sourceId(of ref: String) -> String? {
        guard ref.hasPrefix("source:") else { return nil }
        let id = ref.dropFirst("source:".count)
        guard let dash = id.lastIndex(of: "-") else { return String(id) }
        return String(id[id.startIndex..<dash])
    }

    /// `source:s0a9be3c112-14` → 14.
    static func chunk(of ref: String) -> Int? {
        guard ref.hasPrefix("source:"), let dash = ref.lastIndex(of: "-") else { return nil }
        return Int(ref[ref.index(after: dash)...])
    }

    /// The answer without any citation token, for copying, saving or speaking.
    static func stripped(_ text: String) -> String {
        let parsed = parse(text, citations: [])
        var out = ""
        for piece in pieces(of: parsed.text) {
            if case let .text(t) = piece { out += t }
        }
        return out.replacingOccurrences(of: " .", with: ".").replacingOccurrences(of: " ,", with: ",")
    }
}
