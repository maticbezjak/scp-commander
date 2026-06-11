// Generates the app icon — classic WinSCP motif: two overlapping monitors
// (amber behind-left, blue front-right) with green transfer arrows between
// them, drawn edge-to-edge. CoreGraphics only — no design tools needed.
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

    func rr(_ rect: CGRect, _ radius: CGFloat) -> CGPath {
        CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil)
    }

    /// One monitor: rounded body + inset screen + stand neck + base.
    /// (x, y) is the bottom-left of the base; h includes the stand.
    func monitor(
        x: CGFloat, y: CGFloat, w: CGFloat, h: CGFloat,
        frame: CGColor, screen: CGColor, outline: CGColor
    ) {
        let standH = h * 0.16
        let baseH = h * 0.055
        let bodyH = h - standH
        let lineW = s * 0.012

        // Base + neck
        ctx.setFillColor(frame)
        let baseW = w * 0.46
        ctx.addPath(rr(
            CGRect(x: x + (w - baseW) / 2, y: y, width: baseW, height: baseH), baseH / 2))
        ctx.fillPath()
        ctx.fill(CGRect(x: x + w / 2 - w * 0.055, y: y + baseH, width: w * 0.11, height: standH - baseH))

        // Body with outline
        let body = rr(CGRect(x: x, y: y + standH, width: w, height: bodyH), w * 0.055)
        ctx.addPath(body)
        ctx.fillPath()
        ctx.addPath(body)
        ctx.setStrokeColor(outline)
        ctx.setLineWidth(lineW)
        ctx.strokePath()

        // Screen
        ctx.setFillColor(screen)
        let inset = w * 0.065
        ctx.addPath(rr(
            CGRect(
                x: x + inset, y: y + standH + inset,
                width: w - 2 * inset, height: bodyH - 2 * inset),
            w * 0.03))
        ctx.fillPath()
    }

    // Back monitor — amber/yellow (the classic WinSCP "remote" machine).
    monitor(
        x: s * 0.01, y: s * 0.30, w: s * 0.58, h: s * 0.66,
        frame: CGColor(red: 0.95, green: 0.72, blue: 0.10, alpha: 1),
        screen: CGColor(red: 1.00, green: 0.88, blue: 0.45, alpha: 1),
        outline: CGColor(red: 0.55, green: 0.40, blue: 0.02, alpha: 1))

    // Front monitor — blue, lower right, overlapping.
    monitor(
        x: s * 0.39, y: s * 0.02, w: s * 0.60, h: s * 0.68,
        frame: CGColor(red: 0.10, green: 0.36, blue: 0.78, alpha: 1),
        screen: CGColor(red: 0.42, green: 0.68, blue: 0.97, alpha: 1),
        outline: CGColor(red: 0.03, green: 0.15, blue: 0.40, alpha: 1))

    // Green transfer arrows (⇄) across the overlap, with white outline so
    // they read against both screens.
    func arrow(yMid: CGFloat, toRight: Bool) {
        let x0 = s * 0.26, x1 = s * 0.74
        let bodyH = s * 0.085
        let headW = s * 0.16
        let headH = s * 0.10
        let dir: CGFloat = toRight ? 1 : -1
        let tip: CGFloat = toRight ? x1 : x0
        let bodyStart: CGFloat = toRight ? x0 : x0 + headW
        ctx.beginPath()
        ctx.move(to: CGPoint(x: tip, y: yMid))
        ctx.addLine(to: CGPoint(x: tip - dir * headW, y: yMid + headH))
        ctx.addLine(to: CGPoint(x: tip - dir * headW, y: yMid + bodyH / 2))
        ctx.addLine(to: CGPoint(x: bodyStart, y: yMid + bodyH / 2))
        ctx.addLine(to: CGPoint(x: bodyStart, y: yMid - bodyH / 2))
        ctx.addLine(to: CGPoint(x: tip - dir * headW, y: yMid - bodyH / 2))
        ctx.addLine(to: CGPoint(x: tip - dir * headW, y: yMid - headH))
        ctx.closePath()
        ctx.setFillColor(CGColor(red: 0.13, green: 0.72, blue: 0.22, alpha: 1))
        ctx.fillPath()
        // re-trace for the outline
        ctx.beginPath()
        ctx.move(to: CGPoint(x: tip, y: yMid))
        ctx.addLine(to: CGPoint(x: tip - dir * headW, y: yMid + headH))
        ctx.addLine(to: CGPoint(x: tip - dir * headW, y: yMid + bodyH / 2))
        ctx.addLine(to: CGPoint(x: bodyStart, y: yMid + bodyH / 2))
        ctx.addLine(to: CGPoint(x: bodyStart, y: yMid - bodyH / 2))
        ctx.addLine(to: CGPoint(x: tip - dir * headW, y: yMid - bodyH / 2))
        ctx.addLine(to: CGPoint(x: tip - dir * headW, y: yMid - headH))
        ctx.closePath()
        ctx.setStrokeColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.95))
        ctx.setLineWidth(s * 0.018)
        ctx.strokePath()
    }
    arrow(yMid: s * 0.565, toRight: true)
    arrow(yMid: s * 0.40, toRight: false)

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
