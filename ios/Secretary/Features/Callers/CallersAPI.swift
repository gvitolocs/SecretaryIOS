import Foundation

// "Who called?": caller screening with Jev. Contract: `POST /v1/callers/screen` and
// `GET /v1/callers/directory` on getsecretary.tech (any signed-in device).

/// `POST /v1/callers/screen`. 1-20 numbers, any common format (Danish by default).
struct CallerScreenRequest: Encodable, Equatable, Sendable {
    static let maxNumbers = 20

    var numbers: [String]
    /// `en`, `it` or `da`: the language of `label` and `reasons`.
    var lang: String?
    var context: Context?
    /// Only a real incoming call is recorded as one; a lookup from the app leaves this out.
    var call: Call?

    struct Context: Encodable, Equatable, Sendable {
        /// Only with exactly one number.
        var contact: String?
    }

    struct Call: Encodable, Equatable, Sendable {
        var source: String
        var at: String?
    }
}

struct CallerScreenResponse: Decodable, Equatable, Sendable {
    var lang: String?
    var results: [CallerResult]
    /// The same rows as Markdown, for chat; the app draws its own.
    var table: String?
}

/// One screened number. Invalid numbers come back with `type: "invalid"`, `source: "rules"` and only a label.
struct CallerResult: Decodable, Equatable, Sendable {
    /// E.164 ("+4570405205"); for an invalid one, what was sent.
    var number: String
    var display: String?
    var country: String?
    var countryName: String?
    /// mobile, landline, non_geographic, freephone, premium, unknown, invalid.
    var type: String?
    /// P(spam: telemarketing, robocall or scam).
    var spam: Double?
    /// person, business, telemarketing, scam, unknown.
    var kind: String?
    var kindP: Double?
    /// P(he should pick up).
    var pickUp: Double?
    /// Notification title, in the request's language.
    var label: String?
    /// At most 32 characters: the Call Directory label.
    var short: String?
    /// jev, rules (Jev unavailable: the deterministic fallback) or contact.
    var source: String?
    var known: Known?
    var reasons: [String]

    struct Known: Decodable, Equatable, Sendable {
        var contact: String?
        var previousCalls: Int?
        var lastCall: String?
        var messages: Int?
        /// The public caller-ID page Lucina read first (180.dk), quoted with its URL.
        var publicReport: PublicReport?

        private enum CodingKeys: String, CodingKey {
            case contact, previousCalls, lastCall, messages
            case publicReport = "public"
        }
    }

    struct PublicReport: Decodable, Equatable, Sendable {
        var source: String
        var url: String
        var reports: Int?
        var latestComment: String?
    }

    private enum CodingKeys: String, CodingKey {
        case number, display, country, countryName, type, spam, kind, kindP, pickUp, label, short, source, known, reasons
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        number = try c.decode(String.self, forKey: .number)
        display = try c.decodeIfPresent(String.self, forKey: .display)
        country = try c.decodeIfPresent(String.self, forKey: .country)
        countryName = try c.decodeIfPresent(String.self, forKey: .countryName)
        type = try c.decodeIfPresent(String.self, forKey: .type)
        spam = try c.decodeIfPresent(Double.self, forKey: .spam)
        kind = try c.decodeIfPresent(String.self, forKey: .kind)
        kindP = try c.decodeIfPresent(Double.self, forKey: .kindP)
        pickUp = try c.decodeIfPresent(Double.self, forKey: .pickUp)
        label = try c.decodeIfPresent(String.self, forKey: .label)
        short = try c.decodeIfPresent(String.self, forKey: .short)
        source = try c.decodeIfPresent(String.self, forKey: .source)
        known = try c.decodeIfPresent(Known.self, forKey: .known)
        reasons = try c.decodeIfPresent([String].self, forKey: .reasons) ?? []
    }

    init(number: String, display: String? = nil, country: String? = nil, countryName: String? = nil, type: String? = nil,
         spam: Double? = nil, kind: String? = nil, kindP: Double? = nil, pickUp: Double? = nil, label: String? = nil,
         short: String? = nil, source: String? = nil, known: Known? = nil, reasons: [String] = []) {
        self.number = number
        self.display = display
        self.country = country
        self.countryName = countryName
        self.type = type
        self.spam = spam
        self.kind = kind
        self.kindP = kindP
        self.pickUp = pickUp
        self.label = label
        self.short = short
        self.source = source
        self.known = known
        self.reasons = reasons
    }

    var isInvalid: Bool { type == "invalid" }
}

struct CallersAPI: Sendable {
    let client: APIClient

    /// Screening only reads and scores, so a dropped connection may be retried on the other endpoint.
    func screen(_ request: CallerScreenRequest) async throws -> CallerScreenResponse {
        try await client.send("POST", "/v1/callers/screen", body: request, timeout: 30, idempotent: true)
    }

    func directory() async throws -> CallerDirectoryResponse {
        try await client.get("/v1/callers/directory", timeout: 30)
    }
}

/// What the owner types or pastes into the lookup field: one number or a list.
enum CallerNumbers {
    /// Splits on new lines, commas, semicolons and tabs, and between numbers written side by side
    /// ("+45 70 40 52 05 +357 94 095733"). Spaces inside a number stay. Duplicates (the same digits) are dropped,
    /// the first spelling is kept.
    static func parse(_ text: String) -> [String] {
        var seen = Set<String>()
        var numbers: [String] = []
        for line in text.components(separatedBy: CharacterSet(charactersIn: "\n\r\t,;\u{2028}\u{2029}")) {
            for piece in line.split(usingRegex: "\\s+(?=\\+)") {
                var number = piece.trimmingCharacters(in: .whitespacesAndNewlines)
                if number.lowercased().hasPrefix("tel:") { number = String(number.dropFirst(4)).trimmingCharacters(in: .whitespaces) }
                let key = number.filter(\.isNumber)
                guard !key.isEmpty, seen.insert((number.hasPrefix("+") ? "+" : "") + key).inserted else { continue }
                numbers.append(number)
            }
        }
        return numbers
    }

    /// Requests of at most `CallerScreenRequest.maxNumbers`.
    static func chunks(_ numbers: [String]) -> [[String]] {
        stride(from: 0, to: numbers.count, by: CallerScreenRequest.maxNumbers).map {
            Array(numbers[$0 ..< min($0 + CallerScreenRequest.maxNumbers, numbers.count)])
        }
    }
}

private extension String {
    func split(usingRegex pattern: String) -> [String] {
        guard let regex = try? NSRegularExpression(pattern: pattern) else { return [self] }
        let whole = NSRange(startIndex..., in: self)
        var pieces: [String] = []
        var start = startIndex
        for match in regex.matches(in: self, range: whole) {
            guard let range = Range(match.range, in: self) else { continue }
            pieces.append(String(self[start ..< range.lowerBound]))
            start = range.upperBound
        }
        pieces.append(String(self[start...]))
        return pieces
    }
}
