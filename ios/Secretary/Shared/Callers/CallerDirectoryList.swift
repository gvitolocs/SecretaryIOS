import Foundation

// The labels the Call Directory extension shows on incoming calls ("Telemarketing 86%").
// Shared by the app (which fetches, checks and saves them) and the extension (which reads them), so it
// is Foundation only and small. Contract: GET /v1/callers/directory.

/// One label: the number as digits with the country code, and the text the phone shows.
struct CallerDirectoryEntry: Equatable, Hashable, Sendable {
    var number: Int64
    var label: String
}

/// `GET /v1/callers/directory`: `{"generatedAt": "...", "entries": [{"number": 4570405205, "label": "Telemarketing 86%"}]}`.
/// A malformed entry is skipped, not a reason to drop the whole list.
struct CallerDirectoryResponse: Decodable, Equatable, Sendable {
    var generatedAt: String?
    var entries: [CallerDirectoryEntry]
    /// Numbers to block (`?block=1` only): already above the owner's thresholds, ascending. Empty without `block=1`.
    var block: [Int64]

    private enum CodingKeys: String, CodingKey { case generatedAt, entries, block }

    init(generatedAt: String?, entries: [CallerDirectoryEntry], block: [Int64] = []) {
        self.generatedAt = generatedAt
        self.entries = entries
        self.block = block
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        generatedAt = try container.decodeIfPresent(String.self, forKey: .generatedAt)
        var list = try container.nestedUnkeyedContainer(forKey: .entries)
        var entries: [CallerDirectoryEntry] = []
        while !list.isAtEnd {
            if let entry = try? list.decode(Row.self) {
                entries.append(CallerDirectoryEntry(number: entry.number, label: entry.label))
            } else {
                _ = try? list.decode(Skip.self)   // advance past the bad element
            }
        }
        self.entries = entries
        var numbers: [Int64] = []
        if var blocked = try? container.nestedUnkeyedContainer(forKey: .block) {
            while !blocked.isAtEnd {
                if let number = try? blocked.decode(Int64.self) {
                    numbers.append(number)
                } else if let text = try? blocked.decode(String.self), let number = Int64(text) {
                    numbers.append(number)
                } else {
                    _ = try? blocked.decode(Skip.self)
                }
            }
        }
        block = numbers
    }

    /// When the server built the list.
    var generatedDate: Date? { generatedAt.flatMap { CallerDirectoryList.parseDate($0) } }

    private struct Skip: Decodable {}

    /// `number` is an integer; a numeric string is accepted too.
    private struct Row: Decodable {
        let number: Int64
        let label: String

        private enum CodingKeys: String, CodingKey { case number, label }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            label = try container.decode(String.self, forKey: .label)
            if let value = try? container.decode(Int64.self, forKey: .number) {
                number = value
            } else if let text = try? container.decode(String.self, forKey: .number), let value = Int64(text) {
                number = value
            } else {
                throw DecodingError.dataCorruptedError(forKey: .number, in: container, debugDescription: "not a number")
            }
        }
    }
}

/// A saved list, as the extension reads it.
struct CallerDirectorySnapshot: Equatable, Sendable {
    var generatedAt: String?
    var entries: [CallerDirectoryEntry]
    /// Numbers iOS blocks (Call Directory blocking entries). Empty when the owner turned blocking off.
    var block: [Int64] = []
}

enum CallerDirectoryList {
    /// The server sends at most 5000; this is a memory guard for the extension, not a protocol limit.
    static let maxEntries = 10_000
    /// CallKit shows the label on one line of the incoming-call screen.
    static let maxLabelLength = 64
    /// E.164 numbers have 7 to 15 digits (country code included).
    static let numberRange: ClosedRange<Int64> = 1_000_000 ... 999_999_999_999_999

    /// Cleans a label: control characters (newline, tab) become spaces, runs of spaces collapse, the ends are
    /// trimmed and the text is cut to `maxLabelLength` characters. Empty means the entry is dropped.
    static func cleanLabel(_ raw: String) -> String {
        var text = ""
        var lastWasSpace = true
        for scalar in raw.unicodeScalars {
            let isSpace = scalar.properties.isWhitespace || scalar.properties.generalCategory == .control
                || scalar.properties.generalCategory == .format
            if isSpace {
                if !lastWasSpace { text.unicodeScalars.append(" ") }
                lastWasSpace = true
            } else {
                text.unicodeScalars.append(scalar)
                lastWasSpace = false
            }
        }
        if text.hasSuffix(" ") { text.removeLast() }
        return String(text.prefix(maxLabelLength))
    }

    /// What CallKit needs: valid numbers, clean labels, strictly ascending and unique. The first entry of a
    /// number wins; more than `maxEntries` keeps the lowest numbers.
    static func normalize(_ entries: [CallerDirectoryEntry]) -> [CallerDirectoryEntry] {
        var seen = Set<Int64>()
        var kept: [CallerDirectoryEntry] = []
        kept.reserveCapacity(min(entries.count, maxEntries))
        for entry in entries where numberRange.contains(entry.number) {
            let label = cleanLabel(entry.label)
            guard !label.isEmpty, seen.insert(entry.number).inserted else { continue }
            kept.append(CallerDirectoryEntry(number: entry.number, label: label))
        }
        kept.sort { $0.number < $1.number }
        return kept.count > maxEntries ? Array(kept.prefix(maxEntries)) : kept
    }

    /// Blocking numbers for CallKit: valid, strictly ascending, unique, at most `maxEntries` (the lowest).
    static func normalizeBlock(_ numbers: [Int64]) -> [Int64] {
        let kept = Set(numbers.filter { numberRange.contains($0) }).sorted()
        return kept.count > maxEntries ? Array(kept.prefix(maxEntries)) : kept
    }

    // MARK: File format
    //
    //   SCD2
    //   <generatedAt, or "-">
    //   <entry count>
    //   <block count>
    //   <number><TAB><label>      one per entry, ascending
    //   <number>                  one per blocked number, ascending
    //
    // Plain text keeps the extension's parse loop trivial and the file readable when something goes wrong.
    // SCD1 (labels only, no block count) is still read.

    static let magic = "SCD2"
    static let legacyMagic = "SCD1"

    static func encode(_ snapshot: CallerDirectorySnapshot) -> Data {
        let entries = normalize(snapshot.entries)
        let block = normalizeBlock(snapshot.block)
        let stamp = cleanLabel(snapshot.generatedAt ?? "")
        var text = "\(magic)\n\(stamp.isEmpty ? "-" : stamp)\n\(entries.count)\n\(block.count)\n"
        for entry in entries { text += "\(entry.number)\t\(entry.label)\n" }
        for number in block { text += "\(number)\n" }
        return Data(text.utf8)
    }

    /// The saved list, or nil when the data is not a list this version wrote (wrong header, truncated, garbled).
    /// Everything is normalized again: the extension must never hand CallKit an unsorted list.
    static func decode(_ data: Data) -> CallerDirectorySnapshot? {
        guard let text = String(data: data, encoding: .utf8) else { return nil }
        var lines = text.split(separator: "\n", omittingEmptySubsequences: false).makeIterator()
        guard let header = lines.next(), header == Substring(magic) || header == Substring(legacyMagic),
              let generated = lines.next(), let countLine = lines.next(), let count = Int(countLine), count >= 0 else { return nil }
        var blockCount = 0
        if header == Substring(magic) {
            guard let line = lines.next(), let value = Int(line), value >= 0 else { return nil }
            blockCount = value
        }
        var entries: [CallerDirectoryEntry] = []
        entries.reserveCapacity(min(count, maxEntries))
        for _ in 0 ..< count {
            guard let line = lines.next(), let tab = line.firstIndex(of: "\t"),
                  let number = Int64(line[line.startIndex ..< tab]) else { return nil }
            entries.append(CallerDirectoryEntry(number: number, label: String(line[line.index(after: tab)...])))
        }
        var block: [Int64] = []
        block.reserveCapacity(min(blockCount, maxEntries))
        for _ in 0 ..< blockCount {
            guard let line = lines.next(), let number = Int64(line) else { return nil }
            block.append(number)
        }
        return CallerDirectorySnapshot(generatedAt: generated == "-" ? nil : String(generated), entries: normalize(entries),
                                       block: normalizeBlock(block))
    }

    // MARK: Dates

    nonisolated(unsafe) private static let isoFraction: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()
    nonisolated(unsafe) private static let iso = ISO8601DateFormatter()

    static func parseDate(_ text: String) -> Date? {
        iso.date(from: text) ?? isoFraction.date(from: text)
    }
}
