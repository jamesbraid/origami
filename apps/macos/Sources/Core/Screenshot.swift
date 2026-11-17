// Saving the machine's screen: QEMU's screendump is a binary PPM; a screenshot is a PNG.

import AppKit

enum ScreenshotError: Error, LocalizedError {
    case noPicture, badPPM
    var errorDescription: String? {
        switch self {
        case .noPicture: return "The machine has no picture to save."
        case .badPPM: return "The screen could not be read."
        }
    }
}

enum Screenshot {
    /// Convert a P6 PPM (maxval 255) to a PNG at `url`.
    static func writePNG(fromPPM ppm: URL, to url: URL) throws {
        let d = try Data(contentsOf: ppm)
        var i = 0
        func token() -> String {
            while i < d.count, d[i] == 0x20 || d[i] == 0x0a { i += 1 }
            var s = ""
            while i < d.count, d[i] != 0x20, d[i] != 0x0a { s.append(Character(UnicodeScalar(d[i]))); i += 1 }
            return s
        }
        guard token() == "P6", let w = Int(token()), let h = Int(token()), token() == "255", d.count >= i + 1 + w * h * 3,
              let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: w, pixelsHigh: h, bitsPerSample: 8, samplesPerPixel: 3,
                                         hasAlpha: false, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: w * 3, bitsPerPixel: 24),
              let dest = rep.bitmapData else { throw ScreenshotError.badPPM }
        i += 1
        d.withUnsafeBytes { raw in dest.update(from: raw.bindMemory(to: UInt8.self).baseAddress! + i, count: w * h * 3) }
        guard let png = rep.representation(using: .png, properties: [:]) else { throw ScreenshotError.badPPM }
        try png.write(to: url)
    }
}
