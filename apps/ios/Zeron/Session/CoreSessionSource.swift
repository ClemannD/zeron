import UIKit

/// A live session backed by the Rust core. The transcript flows Rust→Rust
/// (`TranscriptView.attach`); this maps the composer-facing state.
final class CoreSessionSource: SessionSource {
    private(set) var chrome = SessionChrome()
    var onChange: (() -> Void)?
    private weak var app: AppModel?
    private let client: CoreClient
    private let handle: SessionHandle
    private let chatId: String
    private var token: AnyObject?
    private var appToken: AnyObject?
    private var hostDevice = "" {
        didSet { if hostDevice != oldValue { connectVisuals() } }
    }
    /// Between turns, on a host that switches providers, the model menu
    /// offers the other providers too.
    private var canSwitchProvider = false
    private var hostSwitchesProviders = false
    private var turnRunning = false
    /// The host's providers, read ahead: a menu whose top level waits on the
    /// host doesn't open until it answers.
    private var hostHarnesses: [HarnessInfo] = []
    private var harnessesDevice = ""
    /// The host's models per provider, by "device|harness": its default (the
    /// first) and each model's options. Seeded from the built-in list so the
    /// chip never shows a bare provider name, then the host's.
    private var hostModels: [String: [ModelInfo]] = [:]
    private var hostModelsLoading: Set<String> = []
    private weak var engine: TranscriptView?

    init(app: AppModel, client: CoreClient, handle: SessionHandle, chatId: String) {
        self.app = app
        self.client = client
        self.handle = handle
        self.chatId = chatId
        token = app.observeSession(chatId) { [weak self] in self?.refresh() }
        appToken = app.observe { [weak self] in self?.refresh() }
        refresh()
    }

    func attach(_ engine: TranscriptView) {
        _ = engine.attach(client: client, chatId: chatId)
        self.engine = engine
        connectVisuals()
        handle.setViewAttached(attached: true)
    }

    /// Agent visuals load their scripts from the chat's host device.
    private func connectVisuals() {
        guard let engine, !hostDevice.isEmpty else { return }
        engine.setVisualSource(client: client, deviceId: hostDevice, pixelScale: Float(max(2, UITraitCollection.current.displayScale)))
    }

    func detach() {
        handle.setViewAttached(attached: false)
    }

    var isUnused: Bool {
        let t = handle.transcriptStatus()
        return t.entryCount == 0 && t.pendingCount == 0 && handle.composer().queue.isEmpty
    }

    func reattach() {
        handle.setViewAttached(attached: true)
    }

    private func refresh() {
        let c = handle.composer()
        let row = app?.row(chatId)
        hostDevice = c.host.deviceId
        hostSwitchesProviders = c.host.capabilities.providerSwitch
        turnRunning = c.live.turnRunning
        // Not mid-turn, and not while a message is still on its way (its
        // turn is about to start on the current provider).
        let sendPending = c.sendState == .sending || c.sendState == .queued
        canSwitchProvider = hostSwitchesProviders && !turnRunning && !sendPending
        if canSwitchProvider, harnessesDevice != hostDevice {
            let device = hostDevice
            harnessesDevice = device
            Task { @MainActor [weak self] in
                guard let self else { return }
                let list = await self.client.listHarnesses(deviceId: device)
                guard self.hostDevice == device else { return }
                // Nothing back (offline, slow): ask again on the next refresh.
                guard !list.isEmpty else {
                    self.harnessesDevice = ""
                    return
                }
                self.hostHarnesses = list
                self.onChange?()
            }
        }
        var next = SessionChrome()
        next.title = c.title
        let project = row?.project?.name ?? "No project"
        // A Cloud chat's machine is "Cloud", not its own device name.
        let hostName = row?.isCloud == true ? row?.deviceName : c.host.name
        next.subtitle = hostName.map { "\(project) @ \($0)" } ?? project
        next.running = c.live.turnRunning
        next.canSteer = c.host.capabilities.midTurnSteering ?? false
        next.placeholder = "Message \(row?.harnessLabel ?? "the agent")"
        var chips: [ComposerChip] = []
        if let model = row?.modelLabel ?? row.flatMap({ defaultModel(for: $0)?.label }) ?? row?.harnessLabel {
            chips.append(ComposerChip(id: "model", title: model, symbol: nil, icon: BrandMarks.image(for: row?.harness ?? "claude-code", side: 13)))
        }
        if let r = row?.reasoning, !r.isEmpty {
            chips.append(ComposerChip(id: "effort", title: reasoningLabel(level: r), symbol: "gauge.with.dots.needle.67percent"))
        }
        if let row, let model = currentModel(row) {
            chips += ModelOptionChips.chips(options: model.options, picks: client.sessionConfig(chatId: chatId)?.modelOptions ?? [:])
        }
        if let pr = row?.pullRequest {
            let state: SessionRowVM.PR = switch pr.state { case .open: .open; case .merged: .merged; case .closed: .closed }
            chips.append(ComposerChip(id: "pr", title: "\(pr.number)", symbol: nil, tint: PRBadgeView.tone(state), icon: PRIcon.image(side: 12)))
        } else if let b = row?.branch, !b.isEmpty {
            chips.append(ComposerChip(id: "branch", title: b, symbol: nil, icon: BranchIcon.sized()))
        }
        if let usage = c.contextUsage, let tokens = usage.tokens, let window = usage.window, window > 0 {
            let fraction = Double(tokens) / Double(window)
            if fraction >= 0.5 {
                chips.append(ComposerChip(
                    id: "context",
                    title: "\(Int((fraction * 100).rounded()))% context",
                    symbol: fraction >= 0.85 ? "exclamationmark.circle" : "circle.lefthalf.filled",
                    tint: fraction >= 0.85 ? Palette.warning : nil
                ))
            }
        }
        next.chips = chips
        if let sendFailure {
            next.banner = .failed(sendFailure)
        } else if c.sendState == .failed {
            next.banner = .notDelivered
        } else if c.sendState == .queued {
            // A Cloud chat's message waits for its machine to start or wake —
            // the transcript's setup step says so, as on the desktop.
            if row?.isCloud != true {
                next.banner = .failed("\(c.host.name ?? "Host") is offline — will send when it's back")
            }
        } else if c.sendState == .sending {
            next.banner = .sending
        } else if app?.connectivity?.state == .offline {
            next.banner = .offline
        } else if !c.room.connected, let retry = c.room.retryAtMs {
            next.banner = .reconnecting(in: max(1, Int((retry - Int64(Date().timeIntervalSince1970 * 1000)) / 1000)))
        }
        next.uploadProgress = c.transferProgress
        // Working state is shown at the transcript tail (layout engine), not here.
        if let input = c.openInput {
            next.questions = (input.requestId, input.questions.map {
                SessionChrome.Question(id: $0.id, header: $0.header, text: $0.question, options: $0.options, multiSelect: $0.multiSelect, prefill: $0.prefill, multiline: $0.multiline)
            })
        }
        next.queue = c.queue.map { q in
            let gate: String? = switch q.gate {
            case let .editing(_, _, mine)?: mine ? "Editing" : "Being edited"
            case .reviewRequired?: "Needs review"
            case nil: q.actionPending ? "Updating" : nil
            }
            return SessionChrome.QueuedItem(id: q.id, text: q.visibleText, thumbnail: nil, gate: gate)
        }
        next.error = c.queueError
        if next != chrome {
            chrome = next
            onChange?()
        }
    }

    /// False when the core refused the message (it stays in the composer).
    @discardableResult
    func send(text: String, images: [StagedImage], mode: DeliveryMode) -> Bool {
        do {
            if mode == .interrupt, chrome.running { try handle.interrupt() }
            _ = try handle.send(request: SendRequest(text: text, attachments: images.map(\.outgoing), worktree: nil, busy: mode == .steer ? .steer : .queue))
            app?.forgetFreshSideChat(chatId)
            app?.noteDeliveryActivity()
            SessionActivities.sent(chatId)
            sendFailure = nil
            return true
        } catch {
            // Kept across refreshes (which rebuild the chrome) until the next send.
            sendFailure = "Couldn't send: \(error)"
            refresh()
            return false
        }
    }

    private var sendFailure: String?

    func stop() {
        try? handle.interrupt()
    }

    func answer(requestId: String, answers: [(questionId: String, labels: [String])]) {
        if (try? handle.respondInput(requestId: requestId, answers: answers.map { UserInputAnswer(questionId: $0.questionId, labels: $0.labels) })) != nil {
            SessionActivities.sent(chatId)
        }
    }

    func queueAction(_ id: String, _ action: QueueAction) {
        switch action {
        // Steers text into the live turn (never interrupts it); only
        // attachment rows stop the turn to send.
        case .sendNow: Task { _ = try? await handle.deliverQueuedNow(id: id) }
        case .remove: Task { _ = try? await handle.removeQueued(id: id) }
        case .moveUp: _ = try? handle.moveQueuedBy(id: id, delta: -1)
        case .moveDown: _ = try? handle.moveQueuedBy(id: id, delta: 1)
        case .edit: break // Driven by the view: beginEdit / finishEdit.
        }
    }

    private var lease: QueueEditLease?
    private var renewal: Task<Void, Never>?
    /// The row as it was when the edit began: the composer only edits its
    /// visible text, the rest (Appshot context, attachment trailer) rides along.
    private var editBase: (raw: String, visible: String)?

    func beginEdit(_ id: String) async -> String? {
        let start = await handle.beginQueuedEdit(id: id, instanceId: UUID().uuidString)
        guard case let .acquired(lease) = start else { return nil }
        self.lease = lease
        renewal?.cancel()
        renewal = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(20))
                guard let self, let lease = self.lease, !Task.isCancelled else { return }
                if await !self.handle.renewQueuedEdit(lease: lease) { return }
            }
        }
        guard let item = handle.composer().queue.first(where: { $0.id == id }) else { return nil }
        editBase = (item.text, item.visibleText)
        return item.visibleText
    }

    func finishEdit(text: String?) async {
        renewal?.cancel()
        let base = editBase
        editBase = nil
        guard let lease else { return }
        self.lease = nil
        var body = text
        if let edited = text, let base {
            // Unchanged: release the row as it was (a commit would drop what
            // the composer never showed).
            body = edited == base.visible ? nil : Self.replacingVisible(in: base.raw, visible: base.visible, with: edited)
        }
        _ = await handle.finishQueuedEdit(lease: lease, action: body == nil ? .cancel : .commit, text: body)
    }

    /// The row's raw text with its visible part replaced, keeping the hidden
    /// context after it. Blank edits stay blank (the host removes the row).
    static func replacingVisible(in raw: String, visible: String, with edited: String) -> String {
        if edited.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || raw == visible { return edited }
        let body = raw.drop(while: \.isWhitespace)
        if body.hasPrefix(visible) { return edited + body.dropFirst(visible.count) }
        // Attachment-only rows show a placeholder: all of the raw text is context.
        return body.isEmpty ? edited : edited + "\n\n" + body
    }

    func retryDelivery() {
        try? handle.retryDelivery()
    }

    func chipMenu(_ id: String) -> UIMenu? {
        guard let row = app?.row(chatId) else { return nil }
        let harness = row.harness ?? "claude-code"
        switch id {
        case "model":
            // The picker sheet takes the tap (see `modelPicker`).
            return nil
        case "effort":
            return UIMenu(title: "Reasoning effort", children: [UIDeferredMenuElement { [weak self] done in
                guard let self else { return done([]) }
                Task { @MainActor in
                    let models = (try? await self.client.listModels(deviceId: self.hostDevice, harness: harness)) ?? fallbackModels(harness: harness)
                    let levels = models.first { $0.id == row.model }?.reasoningLevels ?? models.first?.reasoningLevels ?? []
                    done(levels.map { l in
                        UIAction(title: reasoningLabel(level: l), state: l == row.reasoning ? .on : .off) { [weak self] _ in
                            self?.setConfig { $0.reasoning = l }
                        }
                    })
                }
            }])
        case "pr":
            guard let url = row.pullRequest.flatMap({ URL(string: $0.url) }) else { return nil }
            return UIMenu(title: row.pullRequest?.title ?? "", children: [
                UIAction(title: "Open Pull Request", image: UIImage(systemName: "safari")) { _ in UIApplication.shared.open(url) },
                UIAction(title: "Copy Link", image: UIImage(systemName: "link")) { _ in UIPasteboard.general.url = url },
            ])
        default:
            guard let optionId = ModelOptionChips.optionId(chip: id),
                  let option = currentModel(row)?.options.first(where: { $0.id == optionId }) else { return nil }
            let picks = client.sessionConfig(chatId: chatId)?.modelOptions ?? [:]
            return ModelOptionChips.menu(option, picks: picks) { [weak self] choice in
                self?.setConfig { $0.modelOptions[optionId] = choice }
                self?.refresh()
            }
        }
    }

    func modelPicker() -> UIViewController? {
        guard let row = app?.row(chatId) else { return nil }
        let harness = row.harness ?? "claude-code"
        let rows: (String) async -> [ModelPickerRow] = { [weak self] id in
            guard let self else { return [] }
            let models = (try? await self.client.listModels(deviceId: self.hostDevice, harness: id)) ?? fallbackModels(harness: id)
            return models.map { ModelPickerRow(id: $0.id, label: $0.label, detail: $0.description) }
        }
        var tabs = [ModelPickerTab(harness: harness, label: row.harnessLabel ?? HarnessNames.label(harness)) { await rows(harness) }]
        if canSwitchProvider {
            tabs += providers(cloud: row.isCloud).filter { $0.id != harness }.map { h in
                ModelPickerTab(harness: h.id, label: h.label, footer: "Your next message continues this chat with \(h.label).") { await rows(h.id) }
            }
        }
        #if DEBUG
        // Screenshots: `-all-providers` offers every agent there is.
        if ProcessInfo.processInfo.arguments.contains("-all-providers") {
            tabs = [tabs[0]] + ["claude-code", "codex", "opencode", "cursor", "devin", "grok", "hermes", "fx", "pi", "antigravity", "muse"]
                .filter { $0 != harness }
                .map { id in ModelPickerTab(harness: id, label: HarnessNames.label(id)) { await rows(id) } }
        }
        #endif
        let note: String? = canSwitchProvider ? nil
            : turnRunning ? "You can switch provider when this turn ends."
            : hostSwitchesProviders ? nil
            : "This computer needs a newer Zeron to switch provider."
        let host = row.isCloud ? (name: "Cloud", symbol: "cloud") : (name: app?.deviceName(hostDevice) ?? "This computer", symbol: "desktopcomputer")
        // No model of its own: it runs the default, so that's the one ticked.
        let current = row.model ?? defaultModel(for: row)?.id
        return ModelPickerViewController.sheet(tabs: tabs, harness: harness, model: current, host: host, note: note) { [weak self] pickedHarness, model in
            guard let self else { return }
            // Re-checked at the pick: a turn may have started since the
            // sheet opened with other providers' tabs.
            if pickedHarness != (self.app?.row(self.chatId)?.harness ?? harness), !self.canSwitchProvider { return }
            self.setConfig {
                if pickedHarness != $0.harness {
                    $0.harness = pickedHarness
                    // The old provider's effort and options don't carry over.
                    $0.reasoning = nil
                    $0.modelOptions = [:]
                } else {
                    let offered = self.models(for: pickedHarness).first { $0.id == model.id }?.options ?? []
                    $0.modelOptions = ModelOptionChips.offered($0.modelOptions, by: offered)
                }
                $0.model = model.id
            }
            self.refresh()
        }
    }

    /// The chat's provider, where it runs: Cloud's account for a Cloud chat,
    /// else the computer hosting it.
    func signInTarget() -> AgentAccountsViewController.Target? {
        guard let row = app?.row(chatId) else { return nil }
        let harness = row.harness ?? "claude-code"
        if row.isCloud {
            return CloudStore.agents.first { $0.harness == harness }.map { .cloud($0) }
        }
        guard !hostDevice.isEmpty, let app else { return nil }
        return .device(id: hostDevice, name: app.deviceName(hostDevice), harness: harness, label: row.harnessLabel ?? HarnessNames.label(harness))
    }

    /// The model a chat without one runs: the host's default for its
    /// provider.
    private func defaultModel(for row: SessionRow) -> ModelPickerRow? {
        models(for: row.harness ?? "claude-code").first.map { ModelPickerRow(id: $0.id, label: $0.label, detail: $0.description) }
    }

    /// The model the chat runs (its own, else the default), with its options.
    private func currentModel(_ row: SessionRow) -> ModelInfo? {
        let list = models(for: row.harness ?? "claude-code")
        guard let id = row.model else { return list.first }
        return list.first { $0.id == id }
    }

    /// The host's models for `harness`. The built-in list answers at once;
    /// the host's (or Cloud's) own list replaces it when it arrives.
    private func models(for harness: String) -> [ModelInfo] {
        let key = "\(hostDevice)|\(harness)"
        if hostModels[key] == nil { hostModels[key] = fallbackModels(harness: harness) }
        if !hostDevice.isEmpty, hostModelsLoading.insert(key).inserted {
            let device = hostDevice
            Task { @MainActor [weak self] in
                guard let self else { return }
                let list = await self.client.listModels(deviceId: device, harness: harness)
                guard !list.isEmpty, self.hostModels[key] != list else { return }
                self.hostModels[key] = list
                self.refresh()
            }
        }
        return hostModels[key] ?? []
    }

    /// The providers a chat on this host can move to: Cloud runs Claude
    /// Code and Codex; a computer, the agents it offers.
    private func providers(cloud: Bool) -> [HarnessInfo] {
        // A computer offers what it reports; until it has, only the current
        // provider (a built-in guess could offer one it doesn't have).
        cloud
            ? fallbackHarnesses().filter { AppModel.cloudHarnesses.contains($0.id) }
            : hostHarnesses.filter(\.offered)
    }

    private func setConfig(_ change: (inout ChatConfig) -> Void) {
        var config = client.sessionConfig(chatId: chatId) ?? ChatConfig(harness: app?.row(chatId)?.harness ?? "claude-code", model: nil, reasoning: nil, modelOptions: [:], sandbox: .workspaceWrite)
        change(&config)
        try? client.setSessionConfig(chatId: chatId, config: config)
    }

    func searchFiles(_ query: String) async -> [FileMatch] {
        (try? await client.searchFiles(deviceId: hostDevice, chatId: chatId, spaceId: nil, query: query)) ?? []
    }

    private static let images = NSCache<NSString, UIImage>()
    /// The core's prefix for an image a reply names in Markdown (a host path,
    /// read under the chat's jail) rather than an attachment or upload.
    static let chatImagePrefix = "chat-image:"

    /// How to read `reference`'s bytes from the chat's host: an attachment
    /// (`ReadAttachmentChunk`), or a reply's Markdown image
    /// (`ReadChatImageChunk`, only from inside this chat's folder).
    private func reader(for reference: String) -> @Sendable () async throws -> Data {
        let client = self.client
        let device = hostDevice
        guard reference.hasPrefix(Self.chatImagePrefix) else {
            return { try await client.readAttachment(deviceId: device, path: reference) }
        }
        let chat = chatId
        let path = Self.hostPath(String(reference.dropFirst(Self.chatImagePrefix.count)), cwd: app?.row(chatId)?.cwd)
        return { try await client.readChatImage(deviceId: device, chatId: chat, path: path) }
    }

    /// A path a reply names, made absolute on its host: a relative one joins
    /// the chat's folder in that host's separator style (as on the desktop).
    static func hostPath(_ path: String, cwd: String?) -> String {
        let absolute = path.hasPrefix("/") || path.hasPrefix("\\")
            || path.range(of: #"^[A-Za-z]:[/\\]"#, options: .regularExpression) != nil
        guard !absolute, let cwd, !cwd.isEmpty else { return path }
        let relative = path.hasPrefix("./") ? String(path.dropFirst(2)) : path
        let windows = cwd.contains("\\")
        var base = cwd
        while base.count > 1, base.hasSuffix("/") || (windows && base.hasSuffix("\\")) { base.removeLast() }
        return windows
            ? base + "\\" + relative.replacingOccurrences(of: "/", with: "\\")
            : base + "/" + relative
    }

    func image(_ reference: String) async -> UIImage? {
        if let hit = Self.images.object(forKey: reference as NSString) { return hit }
        guard let data = try? await reader(for: reference)(),
              let image = await UIImage(data: data)?.byPreparingForDisplay()
        else { return nil }
        Self.images.setObject(image, forKey: reference as NSString)
        return image
    }

    func loadImage(_ reference: String, into view: UIImageView) {
        // Claim the view first: a slower load for a row it used to show
        // must not land on top of this one.
        view.accessibilityIdentifier = reference
        if let hit = Self.images.object(forKey: reference as NSString) {
            view.image = hit
            return
        }
        let read = reader(for: reference)
        let isChatImage = reference.hasPrefix(Self.chatImagePrefix)
        Task.detached(priority: .userInitiated) {
            guard let data = try? await read(),
                  let image = UIImage(data: data)?.preparingForDisplay()
            else {
                // A reply's image the host won't serve (outside the chat's
                // folder, moved, not an image) says so instead of an empty tile.
                guard isChatImage else { return }
                await MainActor.run {
                    guard view.accessibilityIdentifier == reference else { return }
                    Self.showUnavailable(in: view)
                }
                return
            }
            Self.images.setObject(image, forKey: reference as NSString)
            await MainActor.run {
                guard view.accessibilityIdentifier == reference else { return }
                UIView.transition(with: view, duration: 0.2, options: .transitionCrossDissolve) { view.image = image }
            }
        }
    }

    @MainActor
    private static func showUnavailable(in view: UIImageView) {
        let symbol = UIImage(systemName: "photo.badge.exclamationmark", withConfiguration: UIImage.SymbolConfiguration(pointSize: 28, weight: .regular))
        view.contentMode = .center
        view.tintColor = Palette.tertiary
        view.image = symbol
        view.isUserInteractionEnabled = false
        view.accessibilityLabel = "Image unavailable"
    }
}

/// Model options the composer offers as chips of their own, beside effort,
/// and only on models that carry them (the rest ride along untouched).
enum ModelOptionChips {
    static let shown: Set<String> = ["contextWindow"]
    private static let prefix = "option:"

    static func optionId(chip id: String) -> String? {
        id.hasPrefix(prefix) ? String(id.dropFirst(prefix.count)) : nil
    }

    static func chips(options: [ModelOption], picks: [String: String]) -> [ComposerChip] {
        options.filter { shown.contains($0.id) && $0.choices.count > 1 }.compactMap { option in
            current(option, picks).map { choice in
                ComposerChip(id: prefix + option.id, title: title(option, choice), symbol: symbol(option))
            }
        }
    }

    static func menu(_ option: ModelOption, picks: [String: String], pick: @escaping (String) -> Void) -> UIMenu {
        let selected = current(option, picks)?.id
        return UIMenu(title: option.label, children: option.choices.map { choice in
            UIAction(title: choice.label, subtitle: choice.id == option.defaultChoice ? "Default" : nil, state: choice.id == selected ? .on : .off) { _ in
                pick(choice.id)
            }
        })
    }

    /// Only the picks a model offers. Picks outlive the model they were made
    /// on, and harnesses apply some blindly (Claude appends `[1m]` to any
    /// model id when `contextWindow` is "1m").
    static func offered(_ picks: [String: String], by options: [ModelOption]) -> [String: String] {
        picks.filter { id, choice in
            options.contains { $0.id == id && $0.choices.contains { $0.id == choice } }
        }
    }

    /// The pick, else the option's default (what runs without one).
    private static func current(_ option: ModelOption, _ picks: [String: String]) -> ModelOptionChoice? {
        let id = picks[option.id] ?? option.defaultChoice
        return option.choices.first { $0.id == id }
            ?? option.choices.first { $0.id == option.defaultChoice }
            ?? option.choices.first
    }

    private static func title(_ option: ModelOption, _ choice: ModelOptionChoice) -> String {
        option.id == "contextWindow" ? "\(choice.label) context" : choice.label
    }

    private static func symbol(_ option: ModelOption) -> String? {
        option.id == "contextWindow" ? "memorychip" : "slider.horizontal.3"
    }
}
