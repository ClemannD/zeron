import ActivityKit
import SwiftUI
import UIKit
import WidgetKit

/// The sessions this phone sent a message to, outside the app. Beside the
/// camera: the sidebar's working glyph and the run's clock (the count when
/// several run, "Input" when one waits on you). Expanded, and on the Lock
/// Screen, the sessions themselves, each opening its chat. The Lock Screen
/// draws on the app's wallpaper, tinted and faded as on the sessions list.
struct SessionsLiveActivity: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: SessionsActivityAttributes.self) { context in
            LockScreenSessions(state: context.state)
                .activityBackgroundTint(WallpaperBackground.isSet ? Tone.background : nil)
                .activitySystemActionForegroundColor(Tone.text)
                .widgetURL(Links.open(context.state))
        } dynamicIsland: { context in
            let state = context.state
            let single = state.single
            return DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Group {
                        if let single, !single.harness.isEmpty {
                            HarnessMark(harness: single.harness)
                                .frame(width: 30, height: 30)
                        } else {
                            Glyph(state: state, size: 30)
                        }
                    }
                    // Level with the title (the center region sits lower,
                    // below the camera).
                    .frame(maxHeight: .infinity, alignment: .bottom)
                    .padding(.leading, 6)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    Group {
                        if let single {
                            EntryTrailing(entry: single, size: 15)
                        } else if state.more > 0 {
                            Text("+\(state.more)")
                                .font(Face.mono(14))
                                .foregroundStyle(Tone.secondary)
                        }
                    }
                    .frame(maxHeight: .infinity, alignment: .bottom)
                    .padding(.trailing, 6)
                }
                DynamicIslandExpandedRegion(.center) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(Summary.title(state))
                            .font(Face.sans(16))
                            .foregroundStyle(state.awaiting > 0 && single == nil ? Tone.warning : Tone.text)
                            .lineLimit(1)
                        // Several sessions: the rows below need the room.
                        if single != nil {
                            Text(Summary.subtitle(state))
                                .font(Face.sans(12))
                                .foregroundStyle(Tone.secondary)
                                .lineLimit(1)
                        }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
                    .padding(.leading, 4)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    if single == nil {
                        VStack(spacing: 4) {
                            ForEach(state.entries.prefix(3)) { entry in
                                SessionRow(entry: entry, size: 13)
                            }
                        }
                        .padding(.horizontal, 6)
                    }
                }
            } compactLeading: {
                Glyph(state: state, size: 18)
                    .padding(.leading, 2)
            } compactTrailing: {
                CompactTrailing(state: state)
                    .padding(.trailing, 2)
            } minimal: {
                Glyph(state: state, size: 18)
            }
            .keylineTint(state.awaiting > 0 ? Tone.warning : Tone.accent)
            .widgetURL(Links.open(state))
        }
    }
}

private enum Links {
    /// One session opens its chat; several open the sessions list.
    static func open(_ state: SessionsActivityAttributes.ContentState) -> URL? {
        state.single?.url ?? URL(string: "zeron://sessions")
    }
}

private extension SessionsActivityAttributes.ContentState {
    /// The only session, when there is just one.
    var single: SessionsActivityEntry? { entries.count == 1 && more == 0 ? entries[0] : nil }

    var oldestRunning: SessionsActivityEntry? {
        entries.filter { $0.status == .working || $0.status == .awaiting }.min { $0.since < $1.since }
    }
}

/// The words at the top: the session's title, or what the sessions are doing.
private enum Summary {
    static func title(_ s: SessionsActivityAttributes.ContentState) -> String {
        if let single = s.single { return single.title }
        var parts: [String] = []
        if s.awaiting > 0 { parts.append(s.awaiting == 1 ? "1 needs you" : "\(s.awaiting) need you") }
        if s.running > 0 { parts.append("\(s.running) running") }
        if !parts.isEmpty { return parts.joined(separator: " · ") }
        if s.failed > 0 { return s.done > 0 ? "\(s.done) done · \(s.failed) failed" : "\(s.failed) failed" }
        return s.done > 0 ? "All done" : "Stopped"
    }

    static func subtitle(_ s: SessionsActivityAttributes.ContentState) -> String {
        if let single = s.single {
            let status = single.status == .awaiting ? "Needs your input" : single.status.label
            return single.host.isEmpty ? status : "\(status) · \(single.host)"
        }
        var hosts: [String] = []
        for entry in s.entries where !entry.host.isEmpty && !hosts.contains(entry.host) { hosts.append(entry.host) }
        let finished = s.done + s.failed
        let tally = s.active && finished > 0 ? "\(finished) finished" : nil
        return [hosts.joined(separator: ", ").nonEmpty, tally].compactMap { $0 }.joined(separator: " · ")
    }
}

// MARK: Lock Screen

private struct LockScreenSessions: View {
    let state: SessionsActivityAttributes.ContentState

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 12) {
                if let single = state.single, !single.harness.isEmpty {
                    HarnessMark(harness: single.harness)
                        .frame(width: 30, height: 30)
                } else {
                    Glyph(state: state, size: 30)
                }
                VStack(alignment: .leading, spacing: 2) {
                    Text(Summary.title(state))
                        .font(Face.sans(17))
                        .foregroundStyle(Tone.text)
                        .lineLimit(1)
                    Text(Summary.subtitle(state))
                        .font(Face.sans(13))
                        .foregroundStyle(Tone.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                if let single = state.single {
                    EntryTrailing(entry: single, size: 15)
                } else if state.more > 0 {
                    Text("+\(state.more) more")
                        .font(Face.sans(13))
                        .foregroundStyle(Tone.secondary)
                }
            }
            if state.single == nil {
                VStack(spacing: 7) {
                    ForEach(state.entries) { entry in
                        SessionRow(entry: entry)
                    }
                }
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background { WallpaperBackground() }
    }
}

/// The app's wallpaper as the sessions list paints it (rendered by the app
/// into the shared container for this appearance).
private struct WallpaperBackground: View {
    @Environment(\.colorScheme) private var scheme

    static var isSet: Bool {
        SessionsActivityWallpaper.url(dark: true).map { FileManager.default.fileExists(atPath: $0.path) } ?? false
    }

    var body: some View {
        if let url = SessionsActivityWallpaper.url(dark: scheme == .dark), let image = UIImage(contentsOfFile: url.path) {
            Image(uiImage: image)
                .resizable()
                .scaledToFill()
                .accessibilityHidden(true)
        }
    }
}

// MARK: Pieces

/// One session: its state, its agent, its title, and its clock or outcome.
private struct SessionRow: View {
    let entry: SessionsActivityEntry
    var size: CGFloat = 14

    var body: some View {
        Link(destination: entry.url ?? URL(string: "zeron://sessions")!) {
            HStack(spacing: 8) {
                EntryGlyph(status: entry.status)
                    .frame(width: 12, height: 12)
                if !entry.harness.isEmpty {
                    HarnessMark(harness: entry.harness)
                        .frame(width: 13, height: 13)
                }
                Text(entry.title)
                    .font(Face.sans(size))
                    .foregroundStyle(entry.status.running ? Tone.text : Tone.secondary)
                    .lineLimit(1)
                Spacer(minLength: 6)
                EntryTrailing(entry: entry, size: size - 1)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// A working session's clock, or the word for where it stands.
private struct EntryTrailing: View {
    let entry: SessionsActivityEntry
    let size: CGFloat

    var body: some View {
        switch entry.status {
        case .working:
            // A clock reserves room for its widest reading: pin it.
            Clock(since: entry.sinceDate)
                .font(Face.mono(size))
                .foregroundStyle(Tone.secondary)
                .lineLimit(1)
                .frame(width: size * 4.4, alignment: .trailing)
        case .awaiting:
            Text(entry.status.label).font(Face.sans(size)).foregroundStyle(Tone.warning)
        case .done:
            Text(entry.status.label).font(Face.sans(size)).foregroundStyle(Tone.success)
        case .failed:
            Text(entry.status.label).font(Face.sans(size)).foregroundStyle(Tone.danger)
        case .starting, .stopped:
            Text(entry.status.label).font(Face.sans(size)).foregroundStyle(Tone.secondary)
        }
    }
}

private struct CompactTrailing: View {
    let state: SessionsActivityAttributes.ContentState

    var body: some View {
        if state.awaiting > 0 {
            Text("Input")
                .font(Face.sans(13))
                .foregroundStyle(Tone.warning)
        } else if state.running > 1 {
            Text("\(state.running)")
                .font(Face.mono(14))
                .foregroundStyle(Tone.accent)
        } else if let running = state.oldestRunning {
            Clock(since: running.sinceDate)
                .font(Face.mono(14))
                .frame(width: 44, alignment: .trailing)
        } else if state.running == 1 {
            Image(systemName: "ellipsis")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Tone.secondary)
        } else {
            EntryGlyph(status: state.failed > 0 ? .failed : .done)
                .frame(width: 14, height: 14)
        }
    }
}

/// Elapsed time, ticking on its own between updates.
private struct Clock: View {
    let since: Date

    var body: some View {
        Text(timerInterval: since...Date.distantFuture, countsDown: false)
            .monospacedDigit()
            .multilineTextAlignment(.trailing)
    }
}

/// The whole activity's state: waiting on you, working, or how it ended.
private struct Glyph: View {
    let state: SessionsActivityAttributes.ContentState
    let size: CGFloat

    var body: some View {
        Group {
            if state.awaiting > 0 {
                Circle().fill(Tone.warning).padding(size * 0.28)
            } else if state.running > 0 {
                Spinner()
            } else {
                EntryGlyph(status: state.failed > 0 ? .failed : .done)
            }
        }
        .frame(width: size, height: size)
        .accessibilityLabel(Summary.title(state))
    }
}

/// One session's state at row size.
private struct EntryGlyph: View {
    let status: SessionsActivityStatus

    var body: some View {
        switch status {
        case .working:
            Spinner()
        case .starting:
            Circle().strokeBorder(Tone.accent.opacity(0.6), lineWidth: 1.5).padding(1.5)
        case .awaiting:
            Circle().fill(Tone.warning).padding(2.5)
        case .done:
            Image(systemName: "checkmark").resizable().scaledToFit().fontWeight(.bold).foregroundStyle(Tone.success).padding(1)
        case .failed:
            Image(systemName: "xmark").resizable().scaledToFit().fontWeight(.bold).foregroundStyle(Tone.danger).padding(1.5)
        case .stopped:
            RoundedRectangle(cornerRadius: 1.5).fill(Tone.secondary).padding(2.5)
        }
    }
}

/// The sidebar's working glyph, a 2×3 grid of violet dots, caught mid-chase
/// (an activity can't animate it).
private struct Spinner: View {
    private static let ring = [[0, 1], [5, 2], [4, 3]]

    var body: some View {
        GeometryReader { geo in
            let cell = min(geo.size.width / 2.6, geo.size.height / 4.1)
            let gap = cell * 0.43
            VStack(spacing: gap) {
                ForEach(0..<3, id: \.self) { row in
                    HStack(spacing: gap) {
                        ForEach(0..<2, id: \.self) { col in
                            Circle()
                                .fill(Tone.spinner[row])
                                .opacity(Self.opacity(Double(Self.ring[row][col]) / 6))
                                .frame(width: cell, height: cell)
                        }
                    }
                }
            }
            .frame(width: geo.size.width, height: geo.size.height)
        }
    }

    /// `StatusGlyph.opacity`: bright, falling over 45 %, then dim (less dim
    /// than the animation's floor, so the still grid keeps its shape).
    private static func opacity(_ t: Double) -> Double {
        t < 0.45 ? 1 - 0.65 * t / 0.45 : 0.35
    }
}

/// The agent's mark, in its own color (Claude's orange) or the text's.
private struct HarnessMark: View {
    let harness: String

    var body: some View {
        let mark = BrandMark.forHarness(harness)
        BrandMarkShape(mark: mark)
            .fill(BrandMark.brandTint(for: harness).map { Color($0) } ?? Tone.text, style: FillStyle(eoFill: mark.evenOddFill))
            .accessibilityHidden(true)
    }
}

private extension String {
    var nonEmpty: String? { isEmpty ? nil : self }
}

#if DEBUG
private extension SessionsActivityAttributes.ContentState {
    static func preview(_ entries: [SessionsActivityEntry], more: Int = 0) -> Self {
        Self(
            entries: entries,
            running: entries.filter { $0.status == .working || $0.status == .starting }.count,
            awaiting: entries.filter { $0.status == .awaiting }.count,
            done: entries.filter { $0.status == .done }.count,
            failed: entries.filter { $0.status == .failed }.count,
            more: more
        )
    }
}

private func entry(_ id: String, _ title: String, _ status: SessionsActivityStatus, harness: String = "claude-code", host: String = "MacBook Pro", ago: Double = 83) -> SessionsActivityEntry {
    SessionsActivityEntry(id: id, title: title, host: host, harness: harness, status: status, since: Date().timeIntervalSince1970 - ago)
}

private let oneWorking = SessionsActivityAttributes.ContentState.preview([entry("a", "Fix sync backoff", .working)])
private let several = SessionsActivityAttributes.ContentState.preview([
    entry("b", "Migrate auth tables", .awaiting, harness: "codex", host: "Fedora", ago: 400),
    entry("a", "Fix sync backoff", .working, ago: 492),
    entry("c", "Add live activities", .working, ago: 41),
    entry("d", "Bump dependencies", .done, harness: "codex"),
], more: 1)
private let finished = SessionsActivityAttributes.ContentState.preview([
    entry("a", "Fix sync backoff", .done, ago: 20),
    entry("b", "Migrate auth tables", .failed, harness: "codex", host: "Fedora", ago: 60),
])

#Preview("Island", as: .dynamicIsland(.expanded), using: SessionsActivityAttributes()) {
    SessionsLiveActivity()
} contentStates: {
    oneWorking
    several
    finished
}

#Preview("Compact", as: .dynamicIsland(.compact), using: SessionsActivityAttributes()) {
    SessionsLiveActivity()
} contentStates: {
    oneWorking
    several
    SessionsActivityAttributes.ContentState.preview([entry("a", "Fix sync backoff", .working), entry("c", "Add live activities", .working)])
    finished
}

#Preview("Minimal", as: .dynamicIsland(.minimal), using: SessionsActivityAttributes()) {
    SessionsLiveActivity()
} contentStates: {
    oneWorking
    several
}

#Preview("Lock Screen", as: .content, using: SessionsActivityAttributes()) {
    SessionsLiveActivity()
} contentStates: {
    oneWorking
    several
    finished
}
#endif
