import Foundation

/// One item of a typed text: the owner's words as its label, and a quantity/unit when given.
struct ItemPart: Equatable, Sendable {
    var name: String
    var quantity: Int?
    var unit: String?
}

/// Free text to shopping-list items (docs/design/shopping-labels.md), the twin of the Rust crate
/// `crates/shopping-text` and of `web/src/lib/shoppingText.ts`; all three pass
/// `tests/fixtures/shopping-split-cases.json`.
///
/// The owner's rule: an item is a label, and a sentence never becomes one item. Command words
/// ("add", "to the shopping list", "aggiungi alla lista della spesa", "tilføj … til
/// indkøbslisten") go, the text splits on commas, semicolons, line breaks and conjunctions, and
/// a leading quantity is read ("4 baguettes", "500 g burro").
enum ShoppingText {
    static let maxParts = 50
    static let maxQuantity = 9999

    private static let verbs: Set<String> = [
        "add", "please", "put", "buy", "aggiungi", "aggiungere", "metti", "compra", "comprare", "prendi",
        "tilfoej", "koeb", "saet", "husk",
    ]
    private static let courtesy: Set<String> = ["please", "grazie", "tak"]
    private static let preps: Set<String> = [
        "to", "on", "onto", "in", "into", "for", "alla", "alle", "nella", "sulla", "sulle", "per", "a", "til", "paa", "i",
    ]
    private static let dets: Set<String> = ["the", "my", "la", "mia", "min"]
    /// List names: (words, may stand without a preposition).
    private static let lists: [([String], Bool)] = [
        (["shopping", "list"], true),
        (["grocery", "list"], true),
        (["list"], false),
        (["shopping"], false),
        (["lista", "della", "spesa"], true),
        (["lista", "spesa"], true),
        (["lista"], false),
        (["spesa"], false),
        (["indkoebslisten"], true),
        (["indkoebsliste"], true),
        (["listen"], false),
        (["indkoebssedlen"], false),
    ]
    private static let conjunctions: Set<String> = ["e", "ed", "and", "og", "&", "+"]
    private static let units: Set<String> = ["g", "gr", "hg", "kg", "l", "lt", "dl", "cl", "ml", "pz", "pcs", "pc", "stk"]
    private static let unitLinks: Set<String> = ["di", "d", "of"]
    private static let wordTrim: Set<Character> = [":", ",", ".", ";", "!", "?", "\"", "'", "“", "”", "«", "»"]
    private static let partTrim: Set<Character> = [".", ";", ":", "!", "?", "\"", "'", "“", "”", "«", "»"]
    private static let special: [Character: String] = ["æ": "ae", "ø": "oe", "å": "aa", "œ": "oe", "ß": "ss", "ð": "d", "þ": "th", "ł": "l"]

    private static func trim(_ text: String, _ drop: (Character) -> Bool) -> String {
        let chars = Array(text)
        var lo = 0
        var hi = chars.count
        while lo < hi, drop(chars[lo]) { lo += 1 }
        while hi > lo, drop(chars[hi - 1]) { hi -= 1 }
        return String(chars[lo..<hi])
    }

    /// A word as the command tables compare it: surrounding punctuation trimmed, then folded.
    static func foldWord(_ word: String) -> String {
        var out = ""
        for c in trim(word, { wordTrim.contains($0) }).lowercased() {
            if let s = special[c] {
                out += s
            } else {
                out += String(c).folding(options: .diacriticInsensitive, locale: nil)
            }
        }
        return out
    }

    private static func startsWith(_ words: ArraySlice<String>, at k: Int, _ seq: [String]) -> Bool {
        let base = words.startIndex + k
        guard words.count - k >= seq.count else { return false }
        for (i, s) in seq.enumerated() where words[base + i] != s { return false }
        return true
    }

    /// Lengths of every list phrase (`[PREP] [DET] LIST`) at the start of `f`.
    private static func listPhrases(_ f: ArraySlice<String>) -> [Int] {
        var out: [Int] = []
        let hasPrep = f.first.map { preps.contains($0) } ?? false
        for prep in [false, true] {
            if prep && !hasPrep { continue }
            let p = prep ? 1 : 0
            for det in [false, true] {
                let k = p + (det ? 1 : 0)
                if det && !(f.count > p && dets.contains(f[f.startIndex + p])) { continue }
                for (seq, alone) in lists where (prep || alone) && f.count >= k && startsWith(f, at: k, seq) {
                    out.append(k + seq.count)
                }
            }
        }
        return out
    }

    private static func leadLen(_ f: ArraySlice<String>) -> Int {
        let verb = (f.first.map { verbs.contains($0) } ?? false) ? 1 : 0
        return listPhrases(f).reduce(verb) { Swift.max($0, $1) }
    }

    private static func trailLen(_ f: ArraySlice<String>) -> Int {
        var best = (f.last.map { courtesy.contains($0) } ?? false) ? 1 : 0
        for i in 0..<f.count where listPhrases(f.dropFirst(i)).contains(f.count - i) {
            best = Swift.max(best, f.count - i)
            break
        }
        return best
    }

    /// The words left once leading and trailing command words are gone: `lo..<hi`, never empty.
    static func stripCommands(_ folded: [String]) -> Range<Int> {
        var lo = 0
        var hi = folded.count
        while true {
            var changed = false
            var n = leadLen(folded[lo..<hi])
            if n > 0, lo + n < hi {
                lo += n
                changed = true
            }
            n = trailLen(folded[lo..<hi])
            if n > 0, lo + n < hi {
                hi -= n
                changed = true
            }
            if !changed { return lo..<hi }
        }
    }

    /// A segment's words split at conjunctions that have a word on both sides.
    static func splitConjunctions(_ words: [String]) -> [[String]] {
        var groups: [[String]] = []
        var cur: [String] = []
        for (i, w) in words.enumerated() {
            if conjunctions.contains(foldWord(w)), !cur.isEmpty, i + 1 < words.count {
                groups.append(cur)
                cur = []
            } else {
                cur.append(w)
            }
        }
        if !cur.isEmpty { groups.append(cur) }
        return groups
    }

    private static func number(_ s: Substring) -> Int? {
        guard (1...4).contains(s.count), s.allSatisfy({ $0.isASCII && $0.isNumber }), let n = Int(s), (1...maxQuantity).contains(n) else {
            return nil
        }
        return n
    }

    private static func timesPrefix(_ s: String) -> Substring? {
        s.hasPrefix("x") || s.hasPrefix("×") ? s.dropFirst() : nil
    }

    private static func timesSuffix(_ s: String) -> Substring? {
        s.hasSuffix("x") || s.hasSuffix("×") ? s.dropLast() : nil
    }

    /// (quantity, unit, name words) of an item's words, or nil.
    static func parseQuantity(_ words: [String]) -> (Int, String?, [String])? {
        guard let first = words.first else { return nil }
        var lead: (Int, Int)?
        if let n = number(Substring(first)) {
            let times = words.count > 1 && (words[1] == "x" || words[1] == "×")
            lead = (n, times ? 2 : 1)
        } else if let n = timesSuffix(first).flatMap(number) {
            lead = (n, 1)
        } else if let n = timesPrefix(first).flatMap(number) {
            lead = (n, 1)
        }
        if case let (n, start)? = lead {
            var next = start
            var unit: String?
            if next < words.count, units.contains(foldWord(words[next])) {
                unit = foldWord(words[next])
                next += 1
                if next < words.count, unitLinks.contains(foldWord(words[next])) { next += 1 }
            }
            return next < words.count ? (n, unit, Array(words[next...])) : nil
        }
        if words.count >= 2, let n = timesPrefix(words[words.count - 1]).flatMap(number) {
            return (n, nil, Array(words.dropLast()))
        }
        return nil
    }

    private static func part(_ text: String, isKnown: (String) -> Bool) -> ItemPart? {
        let t = trim(text) { $0.isWhitespace || partTrim.contains($0) }
        guard !t.isEmpty else { return nil }
        if isKnown(t) { return ItemPart(name: t, quantity: nil, unit: nil) }
        let words = t.split(whereSeparator: \.isWhitespace).map(String.init)
        if case let (n, unit, name)? = parseQuantity(words) {
            return ItemPart(name: name.joined(separator: " "), quantity: n, unit: unit)
        }
        return ItemPart(name: t, quantity: nil, unit: nil)
    }

    /// The items of a free text. `isKnown(s)`: `s` is exactly a catalog name or alias, so it stays
    /// whole ("sale e pepe").
    static func splitItems(_ text: String, isKnown: (String) -> Bool) -> [ItemPart] {
        let unified = String(text.map { $0 == "\n" || $0 == "\r" || $0 == "\r\n" ? "," : $0 })
        let words = unified.split(whereSeparator: \.isWhitespace).map(String.init)
        guard !words.isEmpty else { return [] }
        let whole = words.joined(separator: " ")
        if isKnown(whole) { return part(whole, isKnown: isKnown).map { [$0] } ?? [] }
        let range = stripCommands(words.map(foldWord))
        let rest = words[range].joined(separator: " ")
        var out: [ItemPart] = []
        for segment in rest.split(omittingEmptySubsequences: false, whereSeparator: { $0 == "," || $0 == ";" }) {
            let seg = segment.trimmingCharacters(in: .whitespaces)
            if seg.isEmpty { continue }
            let groups = isKnown(seg)
                ? [seg]
                : splitConjunctions(seg.split(whereSeparator: \.isWhitespace).map(String.init)).map { $0.joined(separator: " ") }
            for g in groups {
                if out.count == maxParts { return out }
                if let p = part(g, isKnown: isKnown) { out.append(p) }
            }
        }
        return out
    }
}
