import UIKit

/// The desktop's project monogram: a rounded tile in the project's tone
/// (fill @ 0.08, letter @ 0.85, mono medium) — `ui/src/shell/project_icon.rs`.
enum ProjectTile {
    static func letter(_ name: String) -> String {
        String(name.trimmingCharacters(in: .whitespaces).first ?? "?").uppercased()
    }

    /// The tile as an image (composer chips), light/dark aware: the
    /// project's own artwork when `spaceId` has some here
    /// (`ProjectArtwork`), else its monogram.
    static func image(name: String, colorIndex: Int, side: CGFloat = 14, spaceId: String? = nil) -> UIImage {
        if let spaceId, let art = ProjectArtwork.shared.image(spaceId: spaceId) {
            return artwork(art, side: side)
        }
        let tone = Palette.projectDots[colorIndex % Palette.projectDots.count]
        let asset = UIImageAsset()
        let scale = UITraitCollection.current.displayScale > 0 ? UITraitCollection.current.displayScale : 3
        let format = UIGraphicsImageRendererFormat()
        format.scale = scale
        for style in [UIUserInterfaceStyle.light, .dark] {
            let traits = UITraitCollection { $0.userInterfaceStyle = style; $0.displayScale = scale }
            let t = tone.resolvedColor(with: traits)
            let img = UIGraphicsImageRenderer(size: CGSize(width: side, height: side), format: format).image { _ in
                let k = side / 13
                t.withAlphaComponent(0.08).setFill()
                UIBezierPath(roundedRect: CGRect(x: 0, y: 0, width: side, height: side), cornerRadius: 3 * k).fill()
                let font = Fonts.ui(.monoMedium, 9 * k)
                let s = NSAttributedString(string: letter(name), attributes: [.font: font, .foregroundColor: t.withAlphaComponent(0.85)])
                let size = s.size()
                s.draw(at: CGPoint(x: (side - size.width) / 2, y: side / 2 - (font.ascender - font.capHeight / 2)))
            }.withRenderingMode(.alwaysOriginal)
            asset.register(img, with: traits)
        }
        return asset.image(with: UITraitCollection { $0.userInterfaceStyle = UITraitCollection.current.userInterfaceStyle; $0.displayScale = scale })
    }

    /// Artwork fitted into the tile, with the monogram's subtle rounding
    /// (app art often has square corners), as the desktop draws it.
    private static func artwork(_ art: UIImage, side: CGFloat) -> UIImage {
        let format = UIGraphicsImageRendererFormat()
        format.scale = UITraitCollection.current.displayScale > 0 ? UITraitCollection.current.displayScale : 3
        return UIGraphicsImageRenderer(size: CGSize(width: side, height: side), format: format).image { _ in
            let k = side / 13
            UIBezierPath(roundedRect: CGRect(x: 0, y: 0, width: side, height: side), cornerRadius: 3 * k).addClip()
            let w = art.size.width, h = art.size.height
            guard w > 0, h > 0 else { return }
            let fit = min(side / w, side / h)
            let size = CGSize(width: w * fit, height: h * fit)
            art.draw(in: CGRect(x: (side - size.width) / 2, y: (side - size.height) / 2, width: size.width, height: size.height))
        }.withRenderingMode(.alwaysOriginal)
    }
}

/// The desktop's `git-branch.svg` (template).
enum BranchIcon {
    static let image: UIImage? = UIImage(named: "tool-git-branch")

    /// Rasterized at `side` points (buttons size images intrinsically).
    static func sized(_ side: CGFloat = 13) -> UIImage? {
        guard let image else { return nil }
        return UIGraphicsImageRenderer(size: CGSize(width: side, height: side)).image { _ in
            image.draw(in: CGRect(x: 0, y: 0, width: side, height: side))
        }.withRenderingMode(.alwaysTemplate)
    }
}
