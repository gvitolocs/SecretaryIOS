import Foundation

/// Lucina's text is plain text. The only markup ever produced is a `.link` on http(s)
/// URLs found by `NSDataDetector`; nothing is parsed as Markdown or HTML.
enum LinkText {
    static func attributed(_ text: String) -> AttributedString {
        var result = AttributedString(text)
        for (range, url) in links(in: text) {
            guard let lower = AttributedString.Index(range.lowerBound, within: result),
                  let upper = AttributedString.Index(range.upperBound, within: result) else { continue }
            result[lower..<upper].link = url
        }
        return result
    }

    /// The http(s) links in `text`, with their ranges.
    static func links(in text: String) -> [(range: Range<String.Index>, url: URL)] {
        guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else { return [] }
        let nsRange = NSRange(text.startIndex..<text.endIndex, in: text)
        return detector.matches(in: text, options: [], range: nsRange).compactMap { match in
            guard let url = match.url, let scheme = url.scheme?.lowercased(), scheme == "http" || scheme == "https",
                  let host = url.host, !host.isEmpty,
                  let range = Range(match.range, in: text) else { return nil }
            return (range, url)
        }
    }
}
