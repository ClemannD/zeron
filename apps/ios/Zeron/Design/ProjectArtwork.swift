import UIKit

/// Projects' own artwork — the desktop's project icon (a favicon, logo or
/// app icon from the repository) — asked once of a device holding the
/// checkout (`CoreClient.projectIcon`, which searches and renders it there)
/// and kept here decoded. Main-thread, like `AppModel`.
///
/// Answers refresh like the desktop's (every 5 minutes); a project without
/// artwork, or whose host couldn't be reached, is asked again sooner. When
/// an image arrives, `didChange` is posted with the space id as its object,
/// so whatever drew the monogram meanwhile can redraw.
final class ProjectArtwork {
    static let shared = ProjectArtwork()
    static let didChange = Notification.Name("ProjectArtwork.didChange")

    /// Pixels asked of the host: the largest tile (18 pt) at 3x.
    private static let side: UInt32 = 64
    private static let refresh: TimeInterval = 300
    private static let retry: TimeInterval = 60

    private struct Entry {
        var image: UIImage?
        var at: Date
        var loading: Bool
    }

    /// Set while signed in (`AppModel.start`), cleared on sign-out.
    var client: CoreClient? {
        didSet { if client !== oldValue { entries = [:] } }
    }
    private var entries: [String: Entry] = [:]

    /// The project's artwork when it's here; asks for it otherwise (or when
    /// it's due a refresh).
    func image(spaceId: String) -> UIImage? {
        let entry = entries[spaceId]
        if let entry {
            let due = entry.image == nil ? Self.retry : Self.refresh
            if !entry.loading && Date().timeIntervalSince(entry.at) > due { load(spaceId) }
        } else {
            load(spaceId)
        }
        return entry?.image
    }

    private func load(_ spaceId: String) {
        guard let client else { return }
        let previous = entries[spaceId]?.image
        entries[spaceId] = Entry(image: previous, at: Date(), loading: true)
        Task { @MainActor [weak self] in
            var image = previous
            do {
                image = try await client.projectIcon(spaceId: spaceId, maxSide: Self.side).flatMap { UIImage(data: $0) }
            } catch {
                // Unreachable host: keep what was shown, ask again later.
            }
            guard let self, self.client === client else { return }
            self.entries[spaceId] = Entry(image: image, at: Date(), loading: false)
            if image !== previous {
                NotificationCenter.default.post(name: Self.didChange, object: spaceId)
            }
        }
    }
}
