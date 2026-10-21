// Generates the app icon PNGs (1024x1024: default, dark, tinted) with CoreGraphics.
//   swift ios/scripts/make-icon.swift ios/Secretary/Resources/Assets.xcassets/AppIcon.appiconset
// An "S" monogram on a deep-teal gradient. No external assets.
import AppKit
import CoreGraphics
import Foundation

let outDir = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "."
let size = 1024

func color(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat, _ a: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: r, green: g, blue: b, alpha: a)
}

enum Variant { case light, dark, tinted }

func render(_ variant: Variant, to path: String) throws {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    guard let ctx = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                              bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { fatalError("context") }
    let rect = CGRect(x: 0, y: 0, width: size, height: size)

    // Background (the system applies the rounded mask; fill the whole square, no alpha).
    let colors: [CGColor]
    switch variant {
    case .light: colors = [color(0.09, 0.50, 0.52), color(0.03, 0.27, 0.33)]
    case .dark: colors = [color(0.05, 0.16, 0.19), color(0.01, 0.07, 0.09)]
    case .tinted: colors = [color(0, 0, 0), color(0, 0, 0)]
    }
    let gradient = CGGradient(colorsSpace: space, colors: colors as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(gradient, start: CGPoint(x: 0, y: CGFloat(size)), end: CGPoint(x: CGFloat(size), y: 0), options: [])

    // A soft ring behind the letter.
    let ringColor: CGColor = variant == .tinted ? color(1, 1, 1, 0.35) : (variant == .dark ? color(0.31, 0.75, 0.76, 0.35) : color(1, 1, 1, 0.16))
    ctx.setStrokeColor(ringColor)
    ctx.setLineWidth(22)
    ctx.strokeEllipse(in: rect.insetBy(dx: 170, dy: 170))

    // The "S" monogram.
    let letterColor: NSColor = variant == .dark ? NSColor(srgbRed: 0.42, green: 0.85, blue: 0.86, alpha: 1) : .white
    let font = NSFont(name: "NewYork-Semibold", size: 560) ?? NSFont.systemFont(ofSize: 560, weight: .semibold)
    let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: letterColor]
    let text = NSAttributedString(string: "S", attributes: attributes)
    let line = CTLineCreateWithAttributedString(text)
    let bounds = CTLineGetBoundsWithOptions(line, .useGlyphPathBounds)
    ctx.textPosition = CGPoint(x: (CGFloat(size) - bounds.width) / 2 - bounds.minX,
                               y: (CGFloat(size) - bounds.height) / 2 - bounds.minY)
    CTLineDraw(line, ctx)

    guard let image = ctx.makeImage() else { fatalError("image") }
    let rep = NSBitmapImageRep(cgImage: image)
    guard let data = rep.representation(using: .png, properties: [:]) else { fatalError("png") }
    try data.write(to: URL(fileURLWithPath: path))
    print("wrote \(path)")
}

try render(.light, to: "\(outDir)/AppIcon.png")
try render(.dark, to: "\(outDir)/AppIcon-dark.png")
try render(.tinted, to: "\(outDir)/AppIcon-tinted.png")
