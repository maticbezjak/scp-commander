// Generates the app icon (dual-pane + transfer arrows motif) as an .iconset
// directory using only CoreGraphics — no design tools needed.
// Usage: swift scripts/make-icon.swift <output.iconset>
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let outDir = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "ScpCommander.iconset"
try? FileManager.default.createDirectory(
    atPath: outDir, withIntermediateDirectories: true)

func draw(size: Int, scale: Int, name: String) {
    let px = size * scale
    let ctx = CGContext(
        data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    let s = CGFloat(px)

    // Rounded-rect background, deep blue gradient.
    let r = s * 0.18
    let bg = CGPath(
        roundedRect: CGRect(x: s * 0.04, y: s * 0.04, width: s * 0.92, height: s * 0.92),
        cornerWidth: r, cornerHeight: r, transform: nil)
    ctx.addPath(bg)
    ctx.clip()
    let grad = CGGradient(
        colorsSpace: CGColorSpace(name: CGColorSpace.sRGB)!,
        colors: [
            CGColor(red: 0.08, green: 0.22, blue: 0.45, alpha: 1),
            CGColor(red: 0.05, green: 0.12, blue: 0.25, alpha: 1),
        ] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(
        grad, start: CGPoint(x: 0, y: s), end: CGPoint(x: 0, y: 0), options: [])

    // Two panes.
    ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.12))
    let paneW = s * 0.34
    let paneH = s * 0.56
    let paneY = s * 0.24
    for x in [s * 0.12, s * 0.54] {
        ctx.addPath(
            CGPath(
                roundedRect: CGRect(x: x, y: paneY, width: paneW, height: paneH),
                cornerWidth: s * 0.04, cornerHeight: s * 0.04, transform: nil))
        ctx.fillPath()
    }
    // Pane "rows".
    ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.35))
    for x in [s * 0.16, s * 0.58] {
        for i in 0..<3 {
            ctx.fill(
                CGRect(
                    x: x, y: paneY + paneH - s * (0.10 + 0.13 * CGFloat(i)),
                    width: paneW - s * 0.08, height: s * 0.045))
        }
    }

    // Transfer arrows between panes (green right, white left).
    func arrow(yMid: CGFloat, toRight: Bool, color: CGColor) {
        ctx.setFillColor(color)
        let x0 = s * 0.40, x1 = s * 0.60
        let bodyH = s * 0.035
        let head = s * 0.05
        let (tail, tip): (CGFloat, CGFloat) = toRight ? (x0, x1) : (x1, x0)
        ctx.fill(
            CGRect(
                x: min(tail, tip == x1 ? x1 - head : tip + head),
                y: yMid - bodyH / 2,
                width: x1 - x0 - head, height: bodyH))
        ctx.beginPath()
        let dir: CGFloat = toRight ? 1 : -1
        ctx.move(to: CGPoint(x: tip, y: yMid))
        ctx.addLine(to: CGPoint(x: tip - dir * head, y: yMid + head * 0.8))
        ctx.addLine(to: CGPoint(x: tip - dir * head, y: yMid - head * 0.8))
        ctx.closePath()
        ctx.fillPath()
    }
    arrow(yMid: s * 0.58, toRight: true, color: CGColor(red: 0.30, green: 0.85, blue: 0.45, alpha: 1))
    arrow(yMid: s * 0.42, toRight: false, color: CGColor(red: 1, green: 1, blue: 1, alpha: 0.9))

    let img = ctx.makeImage()!
    let suffix = scale == 1 ? "" : "@2x"
    let url = URL(fileURLWithPath: "\(outDir)/icon_\(size)x\(size)\(suffix).png")
    let dest = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, img, nil)
    CGImageDestinationFinalize(dest)
}

for size in [16, 32, 128, 256, 512] {
    draw(size: size, scale: 1, name: "icon_\(size)x\(size)")
    draw(size: size, scale: 2, name: "icon_\(size)x\(size)@2x")
}
print("iconset written to \(outDir)")
