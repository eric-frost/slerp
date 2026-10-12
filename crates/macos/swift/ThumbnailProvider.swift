// Slerp's Quick Look thumbnail extension.  Finder asks Quick Look for
// thumbnails, Quick Look asks this extension for the types listed in
// data/macos/Thumbnails-Info.plist, and the extension asks Slerp's Rust
// core (libslerp_macos, declared in ../include/slerp.h) for the
// pixels.  tools/build-macos.sh compiles it with swiftc, without Xcode.

import CoreGraphics
import Foundation
import QuickLookThumbnailing
import os

@objc(SlerpThumbnailProvider)
final class ThumbnailProvider: QLThumbnailProvider {
    private static let log = Logger(subsystem: "com.slerp3d.Slerp.Thumbnails", category: "thumbnail")

    override func provideThumbnail(
        for request: QLFileThumbnailRequest, _ handler: @escaping (QLThumbnailReply?, Error?) -> Void
    ) {
        // 1. The pixels, at the size Quick Look will show: its largest
        //    size in points times the screen's scale.
        let maximum = request.maximumSize
        let pixels = UInt32(min(2048, max(16, (max(maximum.width, maximum.height) * request.scale).rounded())))
        let started = DispatchTime.now()
        var image = SlerpImage()
        var message = [CChar](repeating: 0, count: 512)
        let capacity = message.count
        let status = request.fileURL.withUnsafeFileSystemRepresentation { path in
            slerp_thumbnail(path, pixels, &image, &message, capacity)
        }
        let milliseconds = Double(DispatchTime.now().uptimeNanoseconds - started.uptimeNanoseconds) / 1e6
        let name = request.fileURL.lastPathComponent
        guard status == SLERP_OK, let picture = Self.cgImage(&image) else {
            let text = message.withUnsafeBufferPointer { String(cString: $0.baseAddress!) }
            Self.log.error("\(name, privacy: .public): no thumbnail (status \(status)): \(text, privacy: .public)")
            handler(nil, NSError(domain: "dev.slerp", code: Int(status), userInfo: [NSLocalizedDescriptionKey: text]))
            return
        }
        Self.log.info(
            "\(name, privacy: .public): \(picture.width)x\(picture.height) in \(milliseconds, format: .fixed(precision: 1)) ms"
        )

        // 2. Quick Look scales the drawing by request.scale, so the size
        //    is in points: the picture fitted inside the largest size.
        let fit = min(maximum.width / CGFloat(picture.width), maximum.height / CGFloat(picture.height))
        let size = CGSize(width: CGFloat(picture.width) * fit, height: CGFloat(picture.height) * fit)
        handler(
            QLThumbnailReply(contextSize: size) { context in
                context.interpolationQuality = .high
                context.draw(picture, in: CGRect(origin: .zero, size: size))
                return true
            }, nil)
    }

    /// Copies a thumbnail from Rust into a CGImage and frees the original.
    private static func cgImage(_ image: inout SlerpImage) -> CGImage? {
        defer { slerp_free(&image) }
        guard let rgba = image.rgba, image.width > 0, image.height > 0 else { return nil }
        let (width, height) = (Int(image.width), Int(image.height))
        let data = Data(bytes: rgba, count: width * height * 4) as CFData
        guard let provider = CGDataProvider(data: data), let sRGB = CGColorSpace(name: CGColorSpace.sRGB) else {
            return nil
        }
        return CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
            space: sRGB, bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue), provider: provider,
            decode: nil, shouldInterpolate: true, intent: .defaultIntent)
    }
}
