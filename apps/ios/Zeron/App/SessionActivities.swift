import ActivityKit
import UIKit

/// This phone's half of the sessions Live Activity (`SessionsLiveActivity`).
/// The edge starts, updates and ends it over APNs (edge/src/live-activity.ts)
/// because the sessions run elsewhere while Zeron is suspended. This hands the
/// edge what it needs: the push-to-start token, each started activity's update
/// token, and every message sent from here (the activity follows exactly
/// those sessions). It also renders the wallpaper the Lock Screen view draws on.
@MainActor
final class SessionActivities {
    static let shared = SessionActivities()

    private weak var app: AppModel?
    private var installed = false
    private var startToken: String?
    private var sentStartToken: String?
    /// Activity id → the update token the edge has.
    private var sentTokens: [String: String] = [:]
    private var observed: Set<String> = []
    private var wallpaperObserver: NSObjectProtocol?

    private let defaults = UserDefaults.standard
    private static let enabledKey = "liveActivities.enabled"
    /// Sends not yet reported (offline): chat id → when it was sent.
    private static let pendingKey = "liveActivities.pending"
    /// A send reported later than this is old news.
    private static let pendingTTL: TimeInterval = 120

    // MARK: Choices

    /// This phone's switch (on unless turned off in Settings).
    var enabled: Bool {
        get { defaults.object(forKey: Self.enabledKey) as? Bool ?? true }
        set {
            defaults.set(newValue, forKey: Self.enabledKey)
            if newValue { sync() } else { turnOff() }
        }
    }

    /// iOS allows Zeron's Live Activities (Settings → Zeron).
    var allowed: Bool { ActivityAuthorizationInfo().areActivitiesEnabled }

    // MARK: Lifecycle

    /// At launch, before a scene: a push can start the activity while Zeron is
    /// in the background, and the system wakes it only to collect the token.
    func install() {
        guard !installed else { return }
        installed = true
        Task {
            for await data in Activity<SessionsActivityAttributes>.pushToStartTokenUpdates {
                startToken = Self.hex(data)
                sync()
            }
        }
        Task {
            for await activity in Activity<SessionsActivityAttributes>.activityUpdates {
                // One combined activity: a new one replaces any left over.
                for other in Activity<SessionsActivityAttributes>.activities where other.id != activity.id {
                    Task { await other.end(nil, dismissalPolicy: .immediate) }
                }
                observe(activity)
            }
        }
        for activity in Activity<SessionsActivityAttributes>.activities { observe(activity) }
        wallpaperObserver = NotificationCenter.default.addObserver(forName: WallpaperStore.didChange, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { SessionActivities.shared.renderWallpaper() }
        }
    }

    /// Signed in with a client (launch, sign-in, foreground): catch the edge up.
    func attach(app: AppModel) {
        self.app = app
        sync()
        renderWallpaper()
    }

    /// Signing out: this phone stops showing the account's sessions.
    func signingOut(client: CoreClient?) {
        if let client { Task.detached { try? await client.unregisterLiveActivity() } }
        endAll()
        reset()
    }

    /// A message (or an answer) was sent to `chatId` from this phone. Callable
    /// from anywhere (the session sources aren't main-actor isolated).
    nonisolated static func sent(_ chatId: String) {
        Task { @MainActor in shared.record(chatId) }
    }

    private func record(_ chatId: String) {
        guard enabled, allowed, app?.isDemo == false else { return }
        var pending = self.pending
        pending[chatId] = Date().timeIntervalSince1970
        self.pending = pending
        flushPending()
    }

    // MARK: Edge

    private func sync() {
        guard enabled, allowed else { return }
        if let token = startToken, token != sentStartToken {
            let environment = PushNotifications.apnsEnvironment
            post(.start(token: token, environment: environment)) { [weak self] ok in
                if ok { self?.sentStartToken = token }
            }
        }
        for activity in Activity<SessionsActivityAttributes>.activities {
            if let token = activity.pushToken.map(Self.hex) { register(activity.id, token) }
        }
        flushPending()
    }

    private func observe(_ activity: Activity<SessionsActivityAttributes>) {
        guard observed.insert(activity.id).inserted else { return }
        let id = activity.id
        Task {
            for await data in activity.pushTokenUpdates { register(id, Self.hex(data)) }
        }
        Task {
            for await state in activity.activityStateUpdates where state == .dismissed || state == .ended {
                // Swiped away (or ended by the edge): stop following until
                // the next send from here.
                observed.remove(id)
                sentTokens[id] = nil
                post(.ended(activityId: id)) { _ in }
                break
            }
        }
    }

    private func register(_ activityId: String, _ token: String) {
        guard enabled, sentTokens[activityId] != token else { return }
        post(.activity(id: activityId, token: token)) { [weak self] ok in
            if ok { self?.sentTokens[activityId] = token }
        }
    }

    private var pending: [String: Double] {
        get { defaults.dictionary(forKey: Self.pendingKey) as? [String: Double] ?? [:] }
        set { defaults.set(newValue, forKey: Self.pendingKey) }
    }

    private func flushPending() {
        let now = Date().timeIntervalSince1970
        for (chatId, at) in pending {
            guard now - at < Self.pendingTTL else {
                pending[chatId] = nil
                continue
            }
            post(.watch(chatId: chatId)) { [weak self] ok in
                if ok, self?.pending[chatId] == at { self?.pending[chatId] = nil }
            }
        }
    }

    private func turnOff() {
        if let client = app?.client, app?.isDemo == false { Task.detached { try? await client.unregisterLiveActivity() } }
        endAll()
        reset()
    }

    private func reset() {
        sentStartToken = nil
        sentTokens = [:]
        pending = [:]
    }

    private func endAll() {
        for activity in Activity<SessionsActivityAttributes>.activities {
            Task { await activity.end(nil, dismissalPolicy: .immediate) }
        }
    }

    private enum Request {
        case start(token: String, environment: String)
        case activity(id: String, token: String)
        case watch(chatId: String)
        case ended(activityId: String)

        var body: [String: String] {
            switch self {
            case let .start(token, environment): ["startToken": token, "environment": environment]
            case let .activity(id, token): ["activityId": id, "token": token]
            case let .watch(chatId): ["watch": chatId]
            case let .ended(activityId): ["ended": activityId]
            }
        }
    }

    /// Through the signed-in client; woken in the background without one (no
    /// scene connects), straight to the edge with the stored access token
    /// while it's still valid. Never refreshes it: that would rotate the
    /// refresh token under the client. Missed tokens go out on the next sync.
    private func post(_ request: Request, done: @escaping (Bool) -> Void) {
        let background = UIApplication.shared.beginBackgroundTask(withName: "Live Activity token")
        let finish: (Bool) -> Void = { ok in
            done(ok)
            if background != .invalid { UIApplication.shared.endBackgroundTask(background) }
        }
        if let client = app?.client {
            guard app?.isDemo == false else { return finish(false) }
            Task.detached {
                let ok: Bool
                do {
                    switch request {
                    case let .start(token, environment): try await client.registerLiveActivityStart(token: token, environment: environment)
                    case let .activity(id, token): try await client.registerLiveActivity(activityId: id, token: token)
                    case let .watch(chatId): try await client.watchLiveActivity(chatId: chatId)
                    case let .ended(activityId): try await client.liveActivityEnded(activityId: activityId)
                    }
                    ok = true
                } catch {
                    NSLog("live activity sync failed: \(error)")
                    ok = false
                }
                await MainActor.run { finish(ok) }
            }
            return
        }
        guard case let .workOs(_, orgId, tokens) = Credentials.stored(),
              let expiry = jwtExpiry(jwt: tokens.accessToken), Double(expiry) > Date().timeIntervalSince1970 + 30,
              let url = URL(string: "\(AppModel.edgeURL.trimmingCharacters(in: CharacterSet(charactersIn: "/")))/registry/\(orgId)/live-activity?device=\(AppModel.deviceId)")
        else { return finish(false) }
        var http = URLRequest(url: url)
        http.httpMethod = "POST"
        http.setValue("Bearer \(tokens.accessToken)", forHTTPHeaderField: "Authorization")
        http.setValue("application/json", forHTTPHeaderField: "Content-Type")
        http.httpBody = try? JSONSerialization.data(withJSONObject: request.body)
        URLSession.shared.dataTask(with: http) { _, response, _ in
            let ok = (response as? HTTPURLResponse).map { (200..<300).contains($0.statusCode) } ?? false
            DispatchQueue.main.async { finish(ok) }
        }.resume()
    }

    #if DEBUG
    /// `-sessions-activity-preview [one|several|done]`: a local activity with
    /// sample sessions, no edge (screenshots, design checks).
    func preview(_ kind: String) {
        let now = Date().timeIntervalSince1970
        func entry(_ id: String, _ title: String, _ status: SessionsActivityStatus, _ harness: String, _ host: String, _ ago: Double) -> SessionsActivityEntry {
            SessionsActivityEntry(id: id, title: title, host: host, harness: harness, status: status, since: now - ago)
        }
        let entries: [SessionsActivityEntry] = switch kind {
        case "one": [entry("a", "Fix sync backoff", .working, "claude-code", "MacBook Pro", 83)]
        case "done": [entry("a", "Fix sync backoff", .done, "claude-code", "MacBook Pro", 20), entry("b", "Migrate auth tables", .failed, "codex", "Fedora", 60)]
        default: [
            entry("b", "Migrate auth tables", .awaiting, "codex", "Fedora", 400),
            entry("a", "Fix sync backoff", .working, "claude-code", "MacBook Pro", 492),
            entry("c", "Add live activities", .working, "claude-code", "MacBook Pro", 41),
            entry("d", "Bump dependencies", .done, "codex", "MacBook Pro", 30),
        ]
        }
        let state = SessionsActivityAttributes.ContentState(
            entries: entries,
            running: entries.filter { $0.status == .working || $0.status == .starting }.count,
            awaiting: entries.filter { $0.status == .awaiting }.count,
            done: entries.filter { $0.status == .done }.count,
            failed: entries.filter { $0.status == .failed }.count,
            more: kind == "several" ? 1 : 0
        )
        renderWallpaper()
        endAll()
        guard let activity = try? Activity.request(attributes: SessionsActivityAttributes(), content: ActivityContent(state: state, staleDate: nil), pushType: nil) else { return }
        // As the edge's alerts do: expands the island, or shows the Lock
        // Screen view as a banner on phones without one.
        let background = UIApplication.shared.beginBackgroundTask(withName: "Live Activity preview")
        Task {
            defer { UIApplication.shared.endBackgroundTask(background) }
            try? await Task.sleep(for: .seconds(8))
            let alert = AlertConfiguration(title: "Migrate auth tables", body: "Waiting on your input", sound: .named("request.wav"))
            await activity.update(ActivityContent(state: state, staleDate: nil), alertConfiguration: alert)
        }
    }
    #endif

    private static func hex(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }

    // MARK: Wallpaper

    /// The wallpaper as the sessions list paints it, in both appearances,
    /// where the extension can read it (nothing without an app group).
    func renderWallpaper() {
        for dark in [false, true] {
            guard let url = SessionsActivityWallpaper.url(dark: dark) else { return }
            WallpaperStore.render(dark: dark) { render in
                guard let render else {
                    try? FileManager.default.removeItem(at: url)
                    return
                }
                let background = Palette.background.resolvedColor(with: UITraitCollection(userInterfaceStyle: dark ? .dark : .light))
                DispatchQueue.global(qos: .utility).async {
                    try? Self.compose(render, over: background)?.write(to: url, options: .atomic)
                }
            }
        }
    }

    /// The Lock Screen activity's size at 2x: artwork aspect-filled at the
    /// contrast guard's opacity over the page background, fading downward
    /// toward the rows like the list's hero fades into the page.
    nonisolated private static func compose(_ render: WallpaperStore.Render, over background: UIColor) -> Data? {
        let size = CGSize(width: 740, height: 320)
        let format = UIGraphicsImageRendererFormat()
        format.scale = 1
        format.opaque = true
        let image = render.image
        guard image.size.width > 0, image.size.height > 0 else { return nil }
        return UIGraphicsImageRenderer(size: size, format: format).jpegData(withCompressionQuality: 0.82) { ctx in
            let rect = CGRect(origin: .zero, size: size)
            background.setFill()
            ctx.fill(rect)
            let scale = max(size.width / image.size.width, size.height / image.size.height)
            let drawn = CGSize(width: image.size.width * scale, height: image.size.height * scale)
            image.draw(in: CGRect(x: (size.width - drawn.width) / 2, y: (size.height - drawn.height) / 2, width: drawn.width, height: drawn.height), blendMode: .normal, alpha: render.opacity)
            // Eased like the hero's mask: clear at the top, mostly page by the bottom.
            let steps = 12
            var colors: [CGColor] = []
            var locations: [CGFloat] = []
            for i in 0...steps {
                let t = CGFloat(i) / CGFloat(steps)
                colors.append(background.withAlphaComponent(0.7 * t * t * (3 - 2 * t)).cgColor)
                locations.append(t)
            }
            if let gradient = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: colors as CFArray, locations: locations) {
                ctx.cgContext.drawLinearGradient(gradient, start: .zero, end: CGPoint(x: 0, y: size.height), options: [])
            }
        }
    }
}
