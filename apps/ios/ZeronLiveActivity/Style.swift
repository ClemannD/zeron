import SwiftUI
import UIKit

/// The app's faces (Geist), bundled with the extension.
enum Face {
    static func sans(_ size: CGFloat) -> Font { .custom("Geist-Medium", size: size) }
    static func mono(_ size: CGFloat) -> Font { .custom("GeistMono-Medium", size: size) }
}

/// Zeron Dark/Light colors (`Palette`, `StatusTone`), as the app paints them.
enum Tone {
    static let background = dynamic(0xF3F3F5, 0x060606)
    static let text = dynamic(0x27272C, 0xE8E8EA)
    static let secondary = dynamic(0x62626A, 0xA9A9AE)
    static let accent = dynamic(0x5B43E8, 0x8B7CF6)
    static let warning = dynamic(0xA16207, 0xFACC15)
    static let success = dynamic(0x15803D, 0x34D399)
    static let danger = dynamic(0xDC2626, 0xF87171)
    /// The sidebar's working spinner, row by row (`StatusGlyph.glyphTints`).
    static let spinner = [dynamic(0x7965EC, 0xABA1F9), dynamic(0x5B43E8, 0x8B7CF6), dynamic(0x4332AC, 0x7266CA)]

    static func dynamic(_ light: UInt32, _ dark: UInt32) -> Color {
        Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(hex: dark) : UIColor(hex: light) })
    }
}

extension UIColor {
    convenience init(hex: UInt32, alpha: CGFloat = 1) {
        self.init(
            red: CGFloat(hex >> 16 & 0xFF) / 255,
            green: CGFloat(hex >> 8 & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: alpha
        )
    }
}
