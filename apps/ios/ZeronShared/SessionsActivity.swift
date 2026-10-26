import ActivityKit
import Foundation

/// The sessions this phone sent a message to, as one Live Activity. The edge
/// starts, updates and ends it over APNs (edge/src/live-activity.ts): the
/// sessions run on other machines while Zeron is suspended. Shared by the
/// app, which hands the edge the activity's tokens, and the widget extension,
/// which draws it. The edge writes this state as JSON: names and shapes must
/// match `ActivityState` there.
struct SessionsActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        /// Waiting on you first, then the longest running, then the latest
        /// finished; at most four.
        var entries: [SessionsActivityEntry]
        /// Sending or working.
        var running: Int
        var awaiting: Int
        var done: Int
        var failed: Int
        /// Sessions past `entries`.
        var more: Int

        var active: Bool { running + awaiting > 0 }
    }
}

struct SessionsActivityEntry: Codable, Hashable, Identifiable {
    /// The chat id.
    var id: String
    var title: String
    /// The host's name ("MacBook Pro"); empty when unknown.
    var host: String
    /// The harness id ("claude-code", "codex"); empty when unknown.
    var harness: String
    var status: SessionsActivityStatus
    /// Epoch seconds: the run's start while it runs, its finish once done.
    var since: Double

    var sinceDate: Date { Date(timeIntervalSince1970: since) }
    var url: URL? { URL(string: "zeron://chat/\(id)") }
}

enum SessionsActivityStatus: String, Codable, Hashable {
    /// Sent; the host hasn't picked it up yet.
    case starting
    case working
    case awaiting
    case done
    case failed
    /// Interrupted, or its host went away.
    case stopped

    var running: Bool { self == .starting || self == .working || self == .awaiting }

    var label: String {
        switch self {
        case .starting: "Sending"
        case .working: "Working"
        case .awaiting: "Input"
        case .done: "Done"
        case .failed: "Failed"
        case .stopped: "Stopped"
        }
    }
}

/// The wallpaper the activity draws on, rendered by the app into the app
/// group the extension can read (the fork's builds have one; elsewhere the
/// activity keeps the system's material).
enum SessionsActivityWallpaper {
    static var directory: URL? {
        guard let group = Bundle.main.object(forInfoDictionaryKey: "ZeronAppGroup") as? String,
              !group.isEmpty, !group.contains("$(")
        else { return nil }
        return FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group)
    }

    static func url(dark: Bool) -> URL? {
        directory?.appendingPathComponent("live-activity-wallpaper-\(dark ? "dark" : "light").jpg")
    }
}
