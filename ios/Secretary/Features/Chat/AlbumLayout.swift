import CoreGraphics

/// Telegram-style album mosaic: where each picture of one bubble goes.
/// 1 = the picture's own shape (at most 260×340), 2 = side by side, 3 = one big + two stacked,
/// 4 = 2×2, 5-10 = rows of 3 (a shorter last row shares the full width).
enum AlbumLayout {
    static let width: CGFloat = 260
    static let maxHeight: CGFloat = 340
    static let minSide: CGFloat = 120
    static let spacing: CGFloat = 2

    struct Layout: Equatable {
        var size: CGSize
        var frames: [CGRect]
    }

    /// `aspect` = width / height of the first picture (used for a single one).
    static func layout(count: Int, aspect: CGFloat? = nil, width: CGFloat = width) -> Layout {
        let s = spacing
        switch count {
        case ...0:
            return Layout(size: .zero, frames: [])
        case 1:
            let ratio = max(0.2, min(5, aspect ?? 4.0 / 3.0))
            var w = width
            var h = w / ratio
            if h > maxHeight {
                h = maxHeight
                w = max(minSide, h * ratio)
            }
            h = max(minSide * 0.75, h)
            return Layout(size: CGSize(width: w, height: h), frames: [CGRect(x: 0, y: 0, width: w, height: h)])
        case 2:
            let w = (width - s) / 2
            let h = w * 1.2
            return Layout(size: CGSize(width: width, height: h),
                          frames: [CGRect(x: 0, y: 0, width: w, height: h), CGRect(x: w + s, y: 0, width: w, height: h)])
        case 3:
            let big = ((width - s) * 2 / 3).rounded(.down)
            let small = width - s - big
            let h = width * 0.8
            let half = (h - s) / 2
            return Layout(size: CGSize(width: width, height: h),
                          frames: [CGRect(x: 0, y: 0, width: big, height: h),
                                   CGRect(x: big + s, y: 0, width: small, height: half),
                                   CGRect(x: big + s, y: half + s, width: small, height: half)])
        case 4:
            let w = (width - s) / 2
            return Layout(size: CGSize(width: width, height: w * 2 + s),
                          frames: [CGRect(x: 0, y: 0, width: w, height: w), CGRect(x: w + s, y: 0, width: w, height: w),
                                   CGRect(x: 0, y: w + s, width: w, height: w), CGRect(x: w + s, y: w + s, width: w, height: w)])
        default:
            let rowHeight = (width - 2 * s) / 3
            var frames: [CGRect] = []
            var y: CGFloat = 0
            var remaining = count
            while remaining > 0 {
                let inRow = min(3, remaining)
                let w = (width - CGFloat(inRow - 1) * s) / CGFloat(inRow)
                for column in 0..<inRow {
                    frames.append(CGRect(x: CGFloat(column) * (w + s), y: y, width: w, height: rowHeight))
                }
                remaining -= inRow
                y += rowHeight + (remaining > 0 ? s : 0)
            }
            return Layout(size: CGSize(width: width, height: y), frames: frames)
        }
    }
}
