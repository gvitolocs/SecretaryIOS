import Foundation
import UIKit

/// ThumbHash decoding (Evan Wallace's public-domain algorithm, https://evanw.github.io/thumbhash/): a ~25-byte
/// hash from lucina-media becomes a blurry 32-pixel placeholder at once, before any rendition loads.
enum ThumbHash {
    struct RGBA: Equatable {
        let width: Int
        let height: Int
        let pixels: [UInt8]
    }

    static func approximateAspectRatio(_ hash: [UInt8]) -> Double? {
        guard hash.count >= 5 else { return nil }
        let header = Int(hash[3])
        let hasAlpha = hash[2] & 0x80 != 0
        let isLandscape = hash[4] & 0x80 != 0
        let lx = isLandscape ? (hasAlpha ? 5 : 7) : header & 7
        let ly = isLandscape ? header & 7 : (hasAlpha ? 5 : 7)
        guard lx > 0, ly > 0 else { return nil }
        return Double(lx) / Double(ly)
    }

    /// The decoded pixels (at most 32×32), or nil for a malformed hash.
    static func decode(_ hash: [UInt8]) -> RGBA? {
        guard hash.count >= 5 else { return nil }
        let header24 = Int(hash[0]) | Int(hash[1]) << 8 | Int(hash[2]) << 16
        let header16 = Int(hash[3]) | Int(hash[4]) << 8
        let lDC = Double(header24 & 63) / 63
        let pDC = Double((header24 >> 6) & 63) / 31.5 - 1
        let qDC = Double((header24 >> 12) & 63) / 31.5 - 1
        let lScale = Double((header24 >> 18) & 31) / 31
        let hasAlpha = (header24 >> 23) != 0
        let pScale = Double((header16 >> 3) & 63) / 63
        let qScale = Double((header16 >> 9) & 63) / 63
        let isLandscape = (header16 >> 15) != 0
        let lx = max(3, isLandscape ? (hasAlpha ? 5 : 7) : header16 & 7)
        let ly = max(3, isLandscape ? header16 & 7 : (hasAlpha ? 5 : 7))
        if hasAlpha && hash.count < 6 { return nil }
        let aDC = hasAlpha ? Double(hash[5] & 15) / 15 : 1
        let aScale = hasAlpha ? Double(hash[5] >> 4) / 15 : 0

        // The varying factors, saturation boosted 1.25× to make up for the quantization.
        let acStart = hasAlpha ? 6 : 5
        var acIndex = 0
        var malformed = false
        func decodeChannel(_ nx: Int, _ ny: Int, _ scale: Double) -> [Double] {
            var ac: [Double] = []
            for cy in 0..<ny {
                var cx = cy > 0 ? 0 : 1
                while cx * ny < nx * (ny - cy) {
                    let byteIndex = acStart + (acIndex >> 1)
                    guard byteIndex < hash.count else { malformed = true; return ac }
                    let nibble = (Int(hash[byteIndex]) >> ((acIndex & 1) << 2)) & 15
                    ac.append((Double(nibble) / 7.5 - 1) * scale)
                    acIndex += 1
                    cx += 1
                }
            }
            return ac
        }
        let lAC = decodeChannel(lx, ly, lScale)
        let pAC = decodeChannel(3, 3, pScale * 1.25)
        let qAC = decodeChannel(3, 3, qScale * 1.25)
        let aAC = hasAlpha ? decodeChannel(5, 5, aScale) : []
        guard !malformed, let ratio = approximateAspectRatio(hash) else { return nil }

        let w = Int((ratio > 1 ? 32 : 32 * ratio).rounded())
        let h = Int((ratio > 1 ? 32 / ratio : 32).rounded())
        guard w > 0, h > 0 else { return nil }
        var rgba = [UInt8](repeating: 0, count: w * h * 4)
        var fx = [Double](repeating: 0, count: max(lx, hasAlpha ? 5 : 3))
        var fy = [Double](repeating: 0, count: max(ly, hasAlpha ? 5 : 3))
        var i = 0
        for y in 0..<h {
            for x in 0..<w {
                var l = lDC, p = pDC, q = qDC, a = aDC
                for cx in 0..<fx.count { fx[cx] = cos(Double.pi / Double(w) * (Double(x) + 0.5) * Double(cx)) }
                for cy in 0..<fy.count { fy[cy] = cos(Double.pi / Double(h) * (Double(y) + 0.5) * Double(cy)) }

                var j = 0
                for cy in 0..<ly {
                    var cx = cy > 0 ? 0 : 1
                    let fy2 = fy[cy] * 2
                    while cx * ly < lx * (ly - cy) {
                        l += lAC[j] * fx[cx] * fy2
                        j += 1
                        cx += 1
                    }
                }
                j = 0
                for cy in 0..<3 {
                    var cx = cy > 0 ? 0 : 1
                    let fy2 = fy[cy] * 2
                    while cx < 3 - cy {
                        let f = fx[cx] * fy2
                        p += pAC[j] * f
                        q += qAC[j] * f
                        j += 1
                        cx += 1
                    }
                }
                if hasAlpha {
                    j = 0
                    for cy in 0..<5 {
                        var cx = cy > 0 ? 0 : 1
                        let fy2 = fy[cy] * 2
                        while cx < 5 - cy {
                            a += aAC[j] * fx[cx] * fy2
                            j += 1
                            cx += 1
                        }
                    }
                }

                let b = l - 2.0 / 3.0 * p
                let r = (3 * l - b + q) / 2
                let g = r - q
                rgba[i] = channel(r)
                rgba[i + 1] = channel(g)
                rgba[i + 2] = channel(b)
                rgba[i + 3] = channel(a)
                i += 4
            }
        }
        return RGBA(width: w, height: h, pixels: rgba)
    }

    private static func channel(_ value: Double) -> UInt8 {
        UInt8(max(0, min(255, (255 * min(1, value)).rounded(.down))))
    }

    static func decode(base64: String) -> RGBA? {
        guard let data = Data(base64Encoded: padded(base64)) else { return nil }
        return decode([UInt8](data))
    }

    /// lucina-media may send URL-safe or unpadded base64.
    private static func padded(_ value: String) -> String {
        var s = value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while s.count % 4 != 0 { s += "=" }
        return s
    }

    private static let cache = NSCache<NSString, UIImage>()

    /// The placeholder as an image (cached by hash).
    static func image(base64: String) -> UIImage? {
        if let cached = cache.object(forKey: base64 as NSString) { return cached }
        guard let rgba = decode(base64: base64) else { return nil }
        let data = Data(rgba.pixels) as CFData
        guard let provider = CGDataProvider(data: data),
              let cgImage = CGImage(width: rgba.width, height: rgba.height, bitsPerComponent: 8, bitsPerPixel: 32,
                                    bytesPerRow: rgba.width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                    bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
                                    provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent) else { return nil }
        let image = UIImage(cgImage: cgImage)
        cache.setObject(image, forKey: base64 as NSString)
        return image
    }

    static func aspectRatio(base64: String) -> Double? {
        guard let data = Data(base64Encoded: padded(base64)) else { return nil }
        return approximateAspectRatio([UInt8](data))
    }
}
