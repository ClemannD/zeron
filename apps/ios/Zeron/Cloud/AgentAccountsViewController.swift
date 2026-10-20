import AuthenticationServices
import UIKit

/// One agent's accounts on one device — a computer or Cloud — as desktop
/// Settings → Providers lists them: every login with its plan and usage, the
/// one in use, switching and removing, and adding more ("Add account", per
/// login option). A computer's sign-in runs on that computer; the phone
/// opens the page and relays the browser's return to it. Cloud's are
/// signed in by the phone itself and kept in Cloud's vault, which also
/// takes an API key (as on the desktop, Cloud only).
final class AgentAccountsViewController: UITableViewController {
    enum Target {
        case cloud(CloudStore.Agent)
        case device(id: String, name: String, harness: String, label: String)
    }

    /// One account, whichever device keeps it.
    private struct Account {
        let id: String
        let title: String
        let detail: String?
        let usage: [UsageWindowInfo]
        let usageNote: String?
        let active: Bool
        let switchable: Bool
        let isKey: Bool
        /// Cloud's vault provider (`claude`, `anthropic-key`, …).
        let cloudProvider: String?
    }

    private enum Section: Int, CaseIterable { case accounts, add }

    private let app: AppModel
    private let target: Target
    private let rules: AgentLoginRules
    private var deviceAccounts: [DeviceAccountInfo]?
    private var loadError: String?
    private var busy: String?
    private var signIn: SignInState?
    private let cloudSignIn = ProviderSignIn()
    private let deviceSignIn = DeviceSignIn()
    private var token: AnyObject?
    var onChange: (([DeviceAccountInfo]) -> Void)?

    /// A sign-in under way: which option, and what to show while it waits.
    private struct SignInState {
        let option: String
        var message: String?
    }

    init(app: AppModel, target: Target, accounts: [DeviceAccountInfo]? = nil) {
        self.app = app
        self.target = target
        switch target {
        case let .cloud(agent):
            rules = agentLoginRules(harness: agent.harness)
        case let .device(_, _, harness, _):
            rules = agentLoginRules(harness: harness)
        }
        deviceAccounts = accounts
        super.init(style: .insetGrouped)
        switch target {
        case let .cloud(agent): title = agent.name
        case let .device(_, _, _, label): title = label
        }
    }

    required init?(coder: NSCoder) { fatalError() }

    @MainActor private var cloud: CloudStore { app.cloud }

    private var harness: String {
        switch target {
        case let .cloud(agent): agent.harness
        case let .device(_, _, harness, _): harness
        }
    }

    private var deviceName: String {
        switch target {
        case .cloud: "Cloud"
        case let .device(_, name, _, _): name
        }
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        tableView.backgroundColor = .clear
        navigationItem.largeTitleDisplayMode = .never
        tableView.register(AccountCell.self, forCellReuseIdentifier: "account")
        tableView.register(UITableViewCell.self, forCellReuseIdentifier: "row")
        refreshControl = UIRefreshControl(frame: .zero, primaryAction: UIAction { [weak self] _ in
            self?.load()
        })
        if case .cloud = target {
            token = cloud.observe { [weak self] in self?.tableView.reloadData() }
        }
        load()
    }

    // MARK: Data

    private func load() {
        switch target {
        case .cloud:
            cloud.refreshAccounts()
            refreshControl?.endRefreshing()
        case let .device(id, _, _, _):
            Task { @MainActor in
                defer { self.refreshControl?.endRefreshing() }
                guard let client = self.app.client else { return }
                do {
                    self.apply(try await client.deviceAgentAccounts(deviceId: id))
                    self.loadError = nil
                } catch {
                    self.loadError = CloudStore.message(error)
                }
                self.tableView.reloadData()
            }
        }
    }

    private func apply(_ list: [DeviceAccountInfo]) {
        deviceAccounts = list
        onChange?(list)
    }

    private var accounts: [Account] {
        switch target {
        case let .cloud(agent):
            let logins = (cloud.accounts[agent.login] ?? []).map { a in
                Account(id: "\(agent.login):\(a.slot)", title: a.email ?? a.displayName ?? "\(agent.name) account",
                        detail: [a.plan, a.organization].compactMap { $0 }.joined(separator: " · ").nonEmpty,
                        usage: [], usageNote: a.needsReconnect ? "Sign in again" : nil,
                        active: a.active, switchable: true, isKey: false, cloudProvider: agent.login)
            }
            let keys = (cloud.accounts[agent.key] ?? []).map { a in
                Account(id: "\(agent.key):\(a.slot)", title: agent.keyName, detail: nil, usage: [], usageNote: nil,
                        active: a.active, switchable: true, isKey: true, cloudProvider: agent.key)
            }
            return logins + keys
        case .device:
            return (deviceAccounts ?? []).filter { $0.harness == harness }.map { a in
                Account(id: a.id, title: a.email ?? a.displayName ?? (a.apiKey ? "API key" : "Account"),
                        detail: [a.planLabel, a.organization, a.provider.flatMap(optionLabel)].compactMap { $0 }.joined(separator: " · ").nonEmpty,
                        usage: rules.reportsUsage ? a.usage : [], usageNote: a.usageError,
                        active: a.active, switchable: a.switchable && rules.switches, isKey: a.apiKey, cloudProvider: nil)
            }
        }
    }

    private func optionLabel(_ provider: String) -> String? {
        rules.options.count > 1 ? rules.options.first { $0.provider == provider }?.label : nil
    }

    private struct AddRow {
        let id: String
        let title: String
        let option: LoginOptionInfo?
    }

    /// The list's last rows: connect the first account or add another, one
    /// per login option; Cloud also takes an API key.
    private var addRows: [AddRow] {
        let empty = accounts.isEmpty
        var rows: [AddRow] = []
        if empty || !rules.keepsOneLogin {
            rows += rules.options.map { option in
                AddRow(id: "add:\(option.provider ?? "")", title: empty ? option.firstTitle : option.moreTitle, option: option)
            }
        }
        if case let .cloud(agent) = target {
            rows.append(AddRow(id: "add-key", title: "Add \(agent.keyName)", option: nil))
        }
        return rows
    }

    // MARK: Table

    override func numberOfSections(in tableView: UITableView) -> Int { Section.allCases.count }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        switch Section(rawValue: section)! {
        case .accounts:
            if case .device = target, deviceAccounts == nil { return 1 }
            return max(accounts.count, loadError == nil ? 0 : 1)
        case .add:
            return addRows.count + (signIn == nil ? 0 : 1)
        }
    }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        Section(rawValue: section) == .accounts ? "Accounts on \(deviceName)" : nil
    }

    override func tableView(_ tableView: UITableView, titleForFooterInSection section: Int) -> String? {
        switch Section(rawValue: section)! {
        case .accounts:
            return rules.note
        case .add:
            switch target {
            case .cloud:
                return "Cloud sessions use the account in use. Sign-ins and keys are kept in Cloud, never on this phone."
            case let .device(_, name, _, _):
                return "Account changes affect new sessions on \(name). Signing in runs on \(name); you finish in the browser here."
            }
        }
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        switch Section(rawValue: indexPath.section)! {
        case .accounts:
            if case .device = target, deviceAccounts == nil {
                return plainCell(text: loadError ?? "Loading…", detail: nil, symbol: loadError == nil ? "ellipsis" : "exclamationmark.triangle", tint: Palette.secondary)
            }
            if accounts.isEmpty, let loadError {
                return plainCell(text: "Couldn't load accounts", detail: loadError, symbol: "exclamationmark.triangle", tint: Palette.secondary)
            }
            let account = accounts[indexPath.row]
            let cell = tableView.dequeueReusableCell(withIdentifier: "account", for: indexPath) as! AccountCell
            cell.configure(
                icon: account.isKey ? CloudUI.symbol("key.fill", 15) : BrandMarks.image(for: harness, side: 20),
                title: account.title,
                detail: account.detail,
                usage: account.usage,
                note: account.usageNote,
                state: busy == account.id ? .busy : account.active ? .inUse : account.switchable ? .switchable : .fixed
            ) { [weak self] in self?.activate(account) }
            cell.accessibilityIdentifier = "account-\(account.id)"
            return cell
        case .add:
            let rows = addRows
            if indexPath.row < rows.count {
                let row = rows[indexPath.row]
                let disabled = signIn != nil || busy != nil
                let cell = plainCell(text: row.title, detail: nil, symbol: "plus.circle", tint: disabled ? Palette.tertiary : Palette.accent)
                var c = cell.contentConfiguration as! UIListContentConfiguration
                c.textProperties.color = disabled ? Palette.tertiary : Palette.accent
                cell.contentConfiguration = c
                cell.accessibilityIdentifier = row.id
                return cell
            }
            let cell = plainCell(text: "Waiting for you to finish in the browser…", detail: signIn?.message, symbol: nil, tint: Palette.secondary)
            let spinner = UIActivityIndicatorView(style: .medium)
            spinner.startAnimating()
            var c = cell.contentConfiguration as! UIListContentConfiguration
            c.image = nil
            cell.contentConfiguration = c
            cell.accessoryView = spinner
            cell.accessibilityIdentifier = "sign-in-waiting"
            return cell
        }
    }

    private func plainCell(text: String, detail: String?, symbol: String?, tint: UIColor) -> UITableViewCell {
        let cell = UITableViewCell(style: .subtitle, reuseIdentifier: nil)
        var c = UIListContentConfiguration.subtitleCell()
        c.text = text
        c.textProperties.font = Fonts.ui(.sansMedium, 16)
        c.textProperties.color = Palette.text
        c.secondaryText = detail
        c.secondaryTextProperties.font = Fonts.ui(.sans, 13)
        c.secondaryTextProperties.color = Palette.secondary
        c.secondaryTextProperties.numberOfLines = 0
        if let symbol {
            c.image = CloudUI.symbol(symbol, 17)
            c.imageProperties.tintColor = tint
        }
        c.imageProperties.reservedLayoutSize = CGSize(width: 28, height: 28)
        cell.contentConfiguration = c
        cell.backgroundColor = Palette.elevated
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        guard Section(rawValue: indexPath.section) == .add, signIn == nil, busy == nil else { return }
        let rows = addRows
        guard indexPath.row < rows.count else { return }
        let row = rows[indexPath.row]
        if row.id == "add-key" {
            askForKey()
        } else if let option = row.option {
            startSignIn(option)
        }
    }

    override func tableView(_ tableView: UITableView, trailingSwipeActionsConfigurationForRowAt indexPath: IndexPath) -> UISwipeActionsConfiguration? {
        guard Section(rawValue: indexPath.section) == .accounts, busy == nil, indexPath.row < accounts.count else { return nil }
        let account = accounts[indexPath.row]
        guard account.switchable else { return nil }
        let remove = UIContextualAction(style: .destructive, title: "Remove") { [weak self] _, _, done in
            self?.remove(account)
            done(true)
        }
        return UISwipeActionsConfiguration(actions: [remove])
    }

    override func tableView(_ tableView: UITableView, contextMenuConfigurationForRowAt indexPath: IndexPath, point: CGPoint) -> UIContextMenuConfiguration? {
        guard Section(rawValue: indexPath.section) == .accounts, busy == nil, indexPath.row < accounts.count else { return nil }
        let account = accounts[indexPath.row]
        guard account.switchable else { return nil }
        return UIContextMenuConfiguration(actionProvider: { [weak self] _ in
            var items: [UIMenuElement] = []
            if !account.active {
                items.append(UIAction(title: "Switch to This Account", image: UIImage(systemName: "arrow.left.arrow.right")) { _ in self?.activate(account) })
            }
            items.append(UIAction(title: "Remove Account", image: UIImage(systemName: "trash"), attributes: .destructive) { _ in self?.remove(account) })
            return UIMenu(children: items)
        })
    }

    // MARK: Actions

    private func activate(_ account: Account) {
        guard !account.active, account.switchable, busy == nil else { return }
        run(account.id) { [self] client in
            switch target {
            case .cloud:
                guard let provider = account.cloudProvider, let info = cloudAccount(account) else { return }
                try await cloud.activate(info, provider: provider)
            case let .device(id, _, harness, _):
                apply(try await client.deviceActivateAccount(deviceId: id, harness: harness, accountId: account.id))
            }
        }
    }

    private func remove(_ account: Account) {
        guard busy == nil else { return }
        run(account.id) { [self] client in
            switch target {
            case .cloud:
                guard let provider = account.cloudProvider, let info = cloudAccount(account) else { return }
                try await cloud.remove(info, provider: provider)
            case let .device(id, _, harness, _):
                apply(try await client.deviceForgetAccount(deviceId: id, harness: harness, accountId: account.id))
            }
        }
    }

    private func cloudAccount(_ account: Account) -> CloudAccountInfo? {
        guard let provider = account.cloudProvider else { return nil }
        let slot = String(account.id.dropFirst(provider.count + 1))
        return cloud.accounts[provider]?.first { $0.slot == slot }
    }

    private func run(_ key: String, _ work: @escaping (CoreClient) async throws -> Void) {
        guard let client = app.client else { return }
        busy = key
        tableView.reloadData()
        Task { @MainActor in
            do {
                try await work(client)
                UISelectionFeedbackGenerator().selectionChanged()
            } catch {
                self.alert("Couldn't update the account", CloudStore.message(error))
            }
            self.busy = nil
            self.tableView.reloadData()
        }
    }

    private func startSignIn(_ option: LoginOptionInfo) {
        guard let client = app.client else { return }
        signIn = SignInState(option: option.provider ?? "")
        tableView.reloadData()
        Task { @MainActor in
            do {
                switch target {
                case let .cloud(agent):
                    try await self.cloudSignIn.run(agent, client: client, from: self.view)
                    self.cloud.refreshAccounts()
                case let .device(id, _, harness, _):
                    let list = try await self.deviceSignIn.run(
                        client: client, device: id, harness: harness, provider: option.provider, host: self
                    ) { [weak self] message in
                        self?.signIn?.message = message
                        self?.tableView.reloadData()
                    }
                    if let list { self.apply(list) } else { self.load() }
                }
                UINotificationFeedbackGenerator().notificationOccurred(.success)
            } catch is CancellationError {
            } catch {
                self.alert("Couldn't sign in", CloudStore.message(error))
            }
            self.signIn = nil
            self.tableView.reloadData()
        }
    }

    private func askForKey() {
        guard case let .cloud(agent) = target else { return }
        let alert = UIAlertController(title: agent.keyName, message: "Cloud sessions use this key instead of a subscription sign-in.", preferredStyle: .alert)
        alert.addTextField { field in
            field.placeholder = agent.key == "anthropic-key" ? "sk-ant-…" : "sk-…"
            field.isSecureTextEntry = true
            field.autocorrectionType = .no
            field.autocapitalizationType = .none
        }
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        alert.addAction(UIAlertAction(title: "Save Key", style: .default) { [weak self, weak alert] _ in
            guard let self, let key = alert?.textFields?.first?.text, !key.isEmpty else { return }
            self.run("api-key") { _ in try await self.cloud.saveKey(key, for: agent) }
        })
        present(alert, animated: true)
    }

    private func alert(_ title: String, _ message: String) {
        let a = UIAlertController(title: title, message: message, preferredStyle: .alert)
        a.addAction(UIAlertAction(title: "OK", style: .default))
        present(a, animated: true)
    }
}

/// Signs a computer's agent in: the computer starts the sign-in, the page
/// opens here, and the browser's return reaches the computer through the
/// phone (the core relays it). Device-code sign-ins finish on their own
/// once the code is entered; Claude's paste fallback takes the code back.
@MainActor
final class DeviceSignIn: NSObject, ASWebAuthenticationPresentationContextProviding {
    private var session: ASWebAuthenticationSession?
    private weak var host: UIViewController?
    private var cancelled = false

    /// Run to completion; the device's fresh accounts when the computer sent
    /// them back (paste flow), `nil` to re-read them.
    func run(
        client: CoreClient,
        device: String,
        harness: String,
        provider: String?,
        host: UIViewController,
        message: @escaping (String?) -> Void
    ) async throws -> [DeviceAccountInfo]? {
        self.host = host
        cancelled = false
        let done = ProviderSignIn.doneURL
        let start = try await client.deviceLoginStart(deviceId: device, harness: harness, provider: provider, doneUrl: done)
        if start.pasteCode {
            open(start.url)
            guard let code = await askForCode() else {
                client.deviceLoginCancel(deviceId: device, loginId: start.loginId)
                throw CancellationError()
            }
            return try await client.deviceLoginComplete(deviceId: device, loginId: start.loginId, code: code)
        }
        var opened = false
        if !start.url.isEmpty {
            open(start.url)
            opened = true
        }
        let deadline = Date().addingTimeInterval(10 * 60)
        while Date() < deadline {
            if cancelled {
                client.deviceLoginCancel(deviceId: device, loginId: start.loginId)
                throw CancellationError()
            }
            try await Task.sleep(nanoseconds: 1_500_000_000)
            let poll = try await client.deviceLoginPoll(deviceId: device, loginId: start.loginId, doneUrl: done)
            switch poll.status {
            case "done":
                session?.cancel()
                return nil
            case "error":
                session?.cancel()
                throw CoreError.HostError(message: poll.message ?? "The sign-in failed.")
            default:
                if let url = poll.url, !opened {
                    open(url)
                    opened = true
                }
                if let note = poll.message {
                    message(note)
                    // "Enter the code ABCD-1234 when asked": have it ready.
                    if let code = note.split(separator: " ").first(where: { $0.contains("-") && $0.count >= 7 }) {
                        UIPasteboard.general.string = String(code)
                    }
                }
            }
        }
        client.deviceLoginCancel(deviceId: device, loginId: start.loginId)
        throw CoreError.HostError(message: "The sign-in timed out.")
    }

    /// The provider's page in the system auth sheet; it closes by itself
    /// when the relayed return redirects to the app (`zeron://signed-in`).
    /// Closing it by hand doesn't end the sign-in — a device code entered
    /// in it still finishes on the computer.
    private func open(_ urlString: String) {
        guard let url = URL(string: urlString) else { return }
        let s = ASWebAuthenticationSession(url: url, callback: .customScheme("zeron")) { [weak self] _, _ in
            self?.session = nil
        }
        s.presentationContextProvider = self
        s.prefersEphemeralWebBrowserSession = false
        session = s
        s.start()
    }

    private func askForCode() async -> String? {
        guard let host else { return nil }
        return await withCheckedContinuation { cont in
            let alert = UIAlertController(title: "Paste the code", message: "Copy the code the sign-in page shows, then paste it here.", preferredStyle: .alert)
            alert.addTextField { field in
                field.placeholder = "Authorization code"
                field.autocorrectionType = .no
                field.autocapitalizationType = .none
            }
            alert.addAction(UIAlertAction(title: "Cancel", style: .cancel) { _ in cont.resume(returning: nil) })
            alert.addAction(UIAlertAction(title: "Continue", style: .default) { [weak alert] _ in
                let code = alert?.textFields?.first?.text?.trimmingCharacters(in: .whitespacesAndNewlines)
                cont.resume(returning: code?.nonEmpty)
            })
            (host.presentedViewController ?? host).present(alert, animated: true)
        }
    }

    func cancel() {
        cancelled = true
        session?.cancel()
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        host?.view.window ?? ASPresentationAnchor()
    }
}

/// One account row: mark, name, plan, usage meters, and "In use" or Switch.
final class AccountCell: UITableViewCell {
    enum State { case inUse, switchable, fixed, busy }

    private let icon = UIImageView()
    private let titleLabel = UILabel()
    private let detailLabel = UILabel()
    private let meters = UIStackView()
    private let trailing = UIView()
    private var onSwitch: (() -> Void)?

    override init(style: UITableViewCell.CellStyle, reuseIdentifier: String?) {
        super.init(style: style, reuseIdentifier: reuseIdentifier)
        backgroundColor = Palette.elevated
        selectionStyle = .none
        icon.contentMode = .scaleAspectFit
        icon.tintColor = Palette.text
        titleLabel.font = Fonts.ui(.sansMedium, 16)
        titleLabel.textColor = Palette.text
        detailLabel.font = Fonts.ui(.sans, 13)
        detailLabel.textColor = Palette.secondary
        detailLabel.numberOfLines = 2
        meters.axis = .vertical
        meters.spacing = 4
        let text = UIStackView(arrangedSubviews: [titleLabel, detailLabel, meters])
        text.axis = .vertical
        text.spacing = 2
        text.setCustomSpacing(8, after: detailLabel)
        // The text takes the row's spare width; the badge or button keeps its own.
        text.setContentHuggingPriority(UILayoutPriority(1), for: .horizontal)
        let row = UIStackView(arrangedSubviews: [icon, text, trailing])
        row.alignment = .center
        row.spacing = 12
        row.translatesAutoresizingMaskIntoConstraints = false
        trailing.setContentHuggingPriority(.required, for: .horizontal)
        trailing.setContentCompressionResistancePriority(.required, for: .horizontal)
        contentView.addSubview(row)
        NSLayoutConstraint.activate([
            icon.widthAnchor.constraint(equalToConstant: 24),
            icon.heightAnchor.constraint(equalToConstant: 24),
            row.topAnchor.constraint(equalTo: contentView.topAnchor, constant: 12),
            row.bottomAnchor.constraint(equalTo: contentView.bottomAnchor, constant: -12),
            row.leadingAnchor.constraint(equalTo: contentView.layoutMarginsGuide.leadingAnchor),
            row.trailingAnchor.constraint(equalTo: contentView.layoutMarginsGuide.trailingAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    func configure(icon image: UIImage?, title: String, detail: String?, usage: [UsageWindowInfo], note: String?, state: State, onSwitch: @escaping () -> Void) {
        icon.image = image
        titleLabel.text = title
        titleLabel.textColor = state == .fixed ? Palette.tertiary : Palette.text
        detailLabel.text = detail
        detailLabel.isHidden = detail == nil
        self.onSwitch = onSwitch
        meters.arrangedSubviews.forEach { $0.removeFromSuperview() }
        for window in usage.prefix(3) {
            meters.addArrangedSubview(UsageMeter(window: window))
        }
        if let note {
            let label = UILabel()
            label.text = note
            label.font = Fonts.ui(.sans, 12)
            label.textColor = Palette.warning
            label.numberOfLines = 0
            meters.addArrangedSubview(label)
        }
        meters.isHidden = meters.arrangedSubviews.isEmpty
        trailing.subviews.forEach { $0.removeFromSuperview() }
        let accessory: UIView?
        switch state {
        case .inUse:
            let badge = PaddedLabel(insets: UIEdgeInsets(top: 3, left: 8, bottom: 3, right: 8))
            badge.text = "In use"
            badge.font = Fonts.ui(.sansMedium, 12)
            badge.textColor = Palette.success
            badge.backgroundColor = Palette.success.withAlphaComponent(0.12)
            badge.layer.cornerRadius = 8
            badge.clipsToBounds = true
            badge.accessibilityIdentifier = "account-in-use"
            accessory = badge
        case .switchable:
            let button = CloudRow.rowButton("Switch", prominent: false) { [weak self] in self?.onSwitch?() }
            button.accessibilityIdentifier = "account-switch"
            accessory = button
        case .busy:
            let spinner = UIActivityIndicatorView(style: .medium)
            spinner.startAnimating()
            accessory = spinner
        case .fixed:
            accessory = nil
        }
        if let accessory {
            accessory.setContentHuggingPriority(.required, for: .horizontal)
            accessory.setContentCompressionResistancePriority(.required, for: .horizontal)
            accessory.translatesAutoresizingMaskIntoConstraints = false
            trailing.addSubview(accessory)
            NSLayoutConstraint.activate([
                accessory.topAnchor.constraint(equalTo: trailing.topAnchor),
                accessory.bottomAnchor.constraint(equalTo: trailing.bottomAnchor),
                accessory.leadingAnchor.constraint(equalTo: trailing.leadingAnchor),
                accessory.trailingAnchor.constraint(equalTo: trailing.trailingAnchor),
            ])
        }
    }
}

/// "Session  ▓▓▓░░░  34% · 3h" (time until the window resets).
final class UsageMeter: UIView {
    init(window: UsageWindowInfo) {
        super.init(frame: .zero)
        let label = UILabel()
        label.text = window.label
        label.font = Fonts.ui(.sans, 12)
        label.textColor = Palette.secondary
        label.widthAnchor.constraint(equalToConstant: 58).isActive = true
        let fraction = max(0, min(1, CGFloat(window.usedFraction)))
        let bar = SplitBar(parts: [(Double(fraction), 0.6)])
        bar.widthAnchor.constraint(equalToConstant: 64).isActive = true
        // The bar draws shares of its whole: the unused remainder stays track.
        bar.subviews.first?.backgroundColor = fraction > 0.9 ? Palette.danger : Palette.text.withAlphaComponent(0.6)
        let value = UILabel()
        var text = "\(Int((fraction * 100).rounded()))%"
        if let resets = window.resetsAtMs {
            // Until the window resets: "45m", "3h", "4d".
            let left = max(0, TimeInterval(resets) / 1000 - Date().timeIntervalSince1970)
            let span = left < 3600 ? "\(Int(left / 60))m" : left < 86_400 ? "\(Int(left / 3600))h" : "\(Int(left / 86_400))d"
            text += " · \(span)"
        }
        value.text = text
        value.font = Fonts.ui(.sans, 12).withMonospacedDigits()
        value.textColor = Palette.secondary
        let row = UIStackView(arrangedSubviews: [label, bar, value])
        row.alignment = .center
        row.spacing = 8
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.topAnchor.constraint(equalTo: topAnchor),
            row.bottomAnchor.constraint(equalTo: bottomAnchor),
            row.leadingAnchor.constraint(equalTo: leadingAnchor),
            row.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }
}
