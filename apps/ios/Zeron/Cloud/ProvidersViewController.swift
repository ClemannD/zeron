import UIKit

/// Settings → Providers, as on the desktop: pick a device (a computer or
/// Cloud), then turn its agents on or off and choose the account each runs
/// on. A computer's agents and logins live on that computer; Cloud's live in
/// its vault.
final class ProvidersViewController: UIViewController, UICollectionViewDelegate {
    private let app: AppModel
    private var collectionView: UICollectionView!
    private var dataSource: UICollectionViewDiffableDataSource<String, Row>!
    private var appToken: AnyObject?
    private var cloudToken: AnyObject?

    /// The device shown: a computer's id, or `cloud`.
    private var target: String
    private var harnesses: [String: [HarnessInfo]] = [:]
    private var accounts: [String: [DeviceAccountInfo]] = [:]
    private var failed: [String: String] = [:]
    private var toggling: Set<String> = []
    private let signIn = ProviderSignIn()
    private var signingIn: String?
    /// Where it's looking and the agents found there (`nil` while looking),
    /// for a host's heading (onboarding's "Your agents").
    var onSummary: ((_ place: String, _ cloud: Bool, _ names: [String]?) -> Void)?

    static let cloud = "cloud"
    private static let lastTargetKey = "providersDevice"

    struct Row: Hashable {
        let id: String
        let title: String
        var subtitle: String?
        var harness: String?
        var symbol: String?
        var toggle: Bool?
        var toggleEnabled = true
        var dimmed = false
        var disclosure = false
        var busy = false
        var value: String?
        /// Cloud, nothing signed in: a "Sign in" button on the row.
        var signIn = false
    }

    init(app: AppModel, target: String? = nil) {
        self.app = app
        let computers = app.hostOptions
        let remembered = UserDefaults.standard.string(forKey: Self.lastTargetKey)
        self.target = target
            ?? remembered.flatMap { r in r == Self.cloud || computers.contains { $0.id == r } ? r : nil }
            ?? computers.first(where: \.online)?.id
            ?? computers.first?.id
            ?? Self.cloud
        super.init(nibName: nil, bundle: nil)
        title = "Providers"
    }

    required init?(coder: NSCoder) { fatalError() }

    @MainActor private var cloud: CloudStore { app.cloud }

    private var onCloud: Bool { target == Self.cloud }

    private var targetName: String {
        onCloud ? "Cloud" : app.hostOptions.first { $0.id == target }?.name ?? app.deviceName(target)
    }

    private var targetOnline: Bool {
        onCloud || app.hostOptions.first { $0.id == target }?.online == true
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        navigationItem.largeTitleDisplayMode = .never
        var config = UICollectionLayoutListConfiguration(appearance: .insetGrouped)
        config.backgroundColor = .clear
        config.headerMode = .supplementary
        config.footerMode = .supplementary
        collectionView = UICollectionView(frame: view.bounds, collectionViewLayout: UICollectionViewCompositionalLayout.list(using: config))
        collectionView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        collectionView.backgroundColor = .clear
        collectionView.delegate = self
        collectionView.refreshControl = UIRefreshControl(frame: .zero, primaryAction: UIAction { [weak self] _ in
            self?.load()
        })
        view.addSubview(collectionView)

        let cell = UICollectionView.CellRegistration<UICollectionViewListCell, Row> { [weak self] cell, _, row in
            guard let self else { return }
            var c = UIListContentConfiguration.subtitleCell()
            c.text = row.title
            c.textProperties.font = Fonts.ui(.sansMedium, 16)
            c.textProperties.color = row.dimmed ? Palette.tertiary : Palette.text
            c.secondaryText = row.subtitle
            c.secondaryTextProperties.font = Fonts.ui(.sans, 13)
            c.secondaryTextProperties.color = Palette.secondary
            if let harness = row.harness {
                let mark = BrandMarks.image(for: harness, side: 20)
                c.image = row.dimmed ? mark.flatMap { $0.withAlpha(0.45) } : mark
            } else if let symbol = row.symbol {
                c.image = UIImage(systemName: symbol)
                c.imageProperties.tintColor = Palette.secondary
            }
            c.imageProperties.maximumSize = CGSize(width: 22, height: 22)
            c.imageProperties.reservedLayoutSize = CGSize(width: 28, height: 28)
            cell.contentConfiguration = c
            var bg = UIBackgroundConfiguration.listGroupedCell()
            bg.backgroundColor = Palette.elevated
            cell.backgroundConfiguration = bg
            var accessories: [UICellAccessory] = []
            if row.id == "device" {
                accessories.append(.popUpMenu(self.deviceMenu(), displayed: .always, options: .init(tintColor: Palette.text)))
                let name = UILabel()
                name.text = self.targetName
                name.font = Fonts.ui(.sansMedium, 15)
                name.textColor = Palette.secondary
                accessories.insert(.customView(configuration: .init(customView: name, placement: .trailing(displayed: .always), reservedLayoutWidth: .actual)), at: 0)
            } else if row.signIn, !row.busy {
                let button = CloudRow.rowButton("Sign in") { [weak self] in
                    guard let harness = row.harness, let agent = CloudStore.agents.first(where: { $0.harness == harness }) else { return }
                    self?.startSignIn(agent)
                }
                button.accessibilityIdentifier = "providers-sign-in-\(row.harness ?? "")"
                accessories.append(.customView(configuration: .init(customView: button, placement: .trailing())))
            } else if row.busy {
                let spinner = UIActivityIndicatorView(style: .medium)
                spinner.startAnimating()
                accessories.append(.customView(configuration: .init(customView: spinner, placement: .trailing())))
            } else if let on = row.toggle {
                let toggle = UISwitch()
                toggle.isOn = on
                toggle.isEnabled = row.toggleEnabled
                toggle.onTintColor = Palette.accent
                toggle.accessibilityIdentifier = "toggle-\(row.id)"
                toggle.addAction(UIAction { [weak self, weak toggle] _ in
                    guard let harness = row.harness else { return }
                    self?.setEnabled(harness, toggle?.isOn ?? false)
                }, for: .valueChanged)
                accessories.append(.customView(configuration: .init(customView: toggle, placement: .trailing())))
            }
            if row.disclosure { accessories.append(.disclosureIndicator()) }
            cell.accessories = accessories
            cell.accessibilityIdentifier = "providers-\(row.id)"
        }
        let header = UICollectionView.SupplementaryRegistration<UICollectionViewListCell>(elementKind: UICollectionView.elementKindSectionHeader) { [weak self] view, _, path in
            var c = UIListContentConfiguration.groupedHeader()
            let id = self?.dataSource.snapshot().sectionIdentifiers[path.section]
            c.text = id == "device" ? nil : id
            view.contentConfiguration = c
        }
        let footer = UICollectionView.SupplementaryRegistration<UICollectionViewListCell>(elementKind: UICollectionView.elementKindSectionFooter) { [weak self] view, _, path in
            guard let self else { return }
            var c = UIListContentConfiguration.groupedFooter()
            let id = self.dataSource.snapshot().sectionIdentifiers[path.section]
            c.text = id == "Agents" ? self.footerText : nil
            view.contentConfiguration = c
        }
        dataSource = UICollectionViewDiffableDataSource(collectionView: collectionView) { cv, path, row in
            cv.dequeueConfiguredReusableCell(using: cell, for: path, item: row)
        }
        dataSource.supplementaryViewProvider = { cv, kind, path in
            kind == UICollectionView.elementKindSectionHeader
                ? cv.dequeueConfiguredReusableSupplementary(using: header, for: path)
                : cv.dequeueConfiguredReusableSupplementary(using: footer, for: path)
        }
        appToken = app.observe { [weak self] in self?.reload() }
        cloudToken = cloud.observe { [weak self] in self?.reload() }
        reload()
        load()
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        // Back from an agent's accounts: they may have switched or removed one.
        if dataSource != nil, !onCloud { loadAccounts() }
    }

    private var footerText: String {
        if onCloud {
            return "Cloud agents use the account marked in use. Signing in here stores the login in Cloud, not on this phone."
        }
        if !targetOnline {
            return "\(targetName) is offline — showing what it last reported."
        }
        return "Agents run with the logins on \(targetName). Switch accounts here; to add one, sign in from Zeron on that computer."
    }

    // MARK: Loading

    private func load() {
        if onCloud {
            cloud.refreshAccounts()
            collectionView.refreshControl?.endRefreshing()
            return
        }
        let device = target
        Task { @MainActor in
            guard let client = self.app.client else { return }
            let list = await client.listHarnesses(deviceId: device)
            self.harnesses[device] = list
            self.reload()
            self.loadAccounts()
            self.collectionView.refreshControl?.endRefreshing()
        }
    }

    private func loadAccounts() {
        let device = target
        Task { @MainActor in
            guard let client = self.app.client, self.targetOnline else { return }
            do {
                self.accounts[device] = try await client.deviceAgentAccounts(deviceId: device)
                self.failed[device] = nil
            } catch {
                self.failed[device] = CloudStore.message(error)
            }
            self.reload()
        }
    }

    private func deviceMenu() -> UIMenu {
        var actions: [UIMenuElement] = app.hostOptions.map { host in
            let action = UIAction(title: host.name, subtitle: host.online ? nil : "Offline", image: UIImage(systemName: "laptopcomputer")) { [weak self] _ in
                self?.select(host.id)
            }
            action.state = host.id == target ? .on : .off
            return action
        }
        if !app.isDemo || ProcessInfo.processInfo.arguments.contains("-cloud") {
            let cloud = UIAction(title: "Cloud", image: UIImage(systemName: "cloud")) { [weak self] _ in
                self?.select(Self.cloud)
            }
            cloud.state = onCloud ? .on : .off
            actions.append(cloud)
        }
        return UIMenu(title: "Devices", children: actions)
    }

    private func select(_ device: String) {
        guard device != target else { return }
        target = device
        UserDefaults.standard.set(device, forKey: Self.lastTargetKey)
        reload()
        load()
    }

    // MARK: Rows

    private func reload() {
        guard dataSource != nil else { return }
        var s = NSDiffableDataSourceSnapshot<String, Row>()
        s.appendSections(["device"])
        // The device in the row's value: switching re-renders it.
        s.appendItems([Row(id: "device", title: "Device", subtitle: nil, symbol: onCloud ? "cloud" : "laptopcomputer", value: target)])
        s.appendSections(["Agents"])
        s.appendItems(onCloud ? cloudRows() : computerRows())
        dataSource.apply(s, animatingDifferences: false)
        let names: [String]? = onCloud
            ? CloudStore.agents.map(\.name)
            : harnesses[target].map { $0.filter(\.installed).map(\.label) }
        onSummary?(targetName, onCloud, names)
    }

    private func cloudRows() -> [Row] {
        CloudStore.agents.map { agent in
            let count = (cloud.accounts[agent.login] ?? []).count + (cloud.accounts[agent.key] ?? []).count
            let busy = signingIn == agent.login
            return Row(
                id: "cloud:\(agent.harness)",
                title: agent.name,
                subtitle: busy ? "Finishing sign-in…" : Self.summary(active: cloud.activeAccount(agent).map { [$0.email ?? (($0.kind == "key") ? agent.keyName : nil), $0.plan].compactMap { $0 }.joined(separator: " · ") }, count: count),
                harness: agent.harness,
                disclosure: true,
                busy: busy,
                signIn: count == 0
            )
        }
    }

    private func computerRows() -> [Row] {
        guard let list = harnesses[target] else {
            return [Row(id: "loading", title: "Loading…", symbol: "ellipsis")]
        }
        let online = targetOnline
        let enabledCount = list.filter(\.offered).count
        let logins = accounts[target] ?? []
        return list.sorted { ($0.installed ? 0 : 1, $0.label) < ($1.installed ? 0 : 1, $1.label) }.map { h in
            let mine = logins.filter { $0.harness == h.id }
            let on = h.offered
            let subtitle: String?
            if !h.installed {
                subtitle = "Not installed"
            } else if accounts[target] == nil {
                subtitle = failed[target] ?? (online ? "Installed" : "Offline")
            } else {
                subtitle = Self.summary(active: mine.first(where: \.active).map(Self.accountTitle), count: mine.count, showsNone: Self.signsIn(h.id))
            }
            return Row(
                id: "\(target):\(h.id)",
                title: h.label,
                subtitle: subtitle,
                harness: h.id,
                toggle: h.installed ? on : false,
                // The last agent standing stays on: new sessions need one.
                toggleEnabled: online && h.installed && !(on && enabledCount <= 1),
                dimmed: !h.installed,
                disclosure: h.installed && !mine.isEmpty,
                busy: toggling.contains(h.id)
            )
        }
    }

    /// Agents whose logins Zeron lists (an account summary means something).
    private static func signsIn(_ harness: String) -> Bool {
        ["claude-code", "codex", "cursor", "opencode", "pi", "hermes", "grok", "devin"].contains(harness)
    }

    static func accountTitle(_ a: DeviceAccountInfo) -> String {
        a.email ?? a.displayName ?? (a.apiKey ? "API key" : "Account")
    }

    private static func summary(active: String?, count: Int, showsNone: Bool = true) -> String? {
        switch (active, count) {
        case let (active?, n) where n > 1: return "\(active) · \(n) accounts"
        case let (active?, _): return active
        case (nil, 0): return showsNone ? "No account" : nil
        case (nil, 1): return "1 account"
        case let (nil, n): return "\(n) accounts"
        }
    }

    // MARK: Actions

    private func setEnabled(_ harness: String, _ on: Bool) {
        let device = target
        toggling.insert(harness)
        reload()
        Task { @MainActor in
            defer {
                self.toggling.remove(harness)
                self.reload()
            }
            guard let client = self.app.client else { return }
            do {
                self.harnesses[device] = try await client.setHarnessEnabled(deviceId: device, harness: harness, enabled: on)
                UISelectionFeedbackGenerator().selectionChanged()
            } catch {
                let a = UIAlertController(title: on ? "Couldn't turn it on" : "Couldn't turn it off", message: CloudStore.message(error), preferredStyle: .alert)
                a.addAction(UIAlertAction(title: "OK", style: .default))
                self.present(a, animated: true)
            }
        }
    }

    private func startSignIn(_ agent: CloudStore.Agent) {
        guard let client = app.client, signingIn == nil else { return }
        signingIn = agent.login
        reload()
        Task { @MainActor in
            do {
                try await self.signIn.run(agent, client: client, from: self.view)
                UINotificationFeedbackGenerator().notificationOccurred(.success)
            } catch is CancellationError {
            } catch {
                let a = UIAlertController(title: "Couldn't sign in", message: CloudStore.message(error), preferredStyle: .alert)
                a.addAction(UIAlertAction(title: "OK", style: .default))
                self.present(a, animated: true)
            }
            self.signingIn = nil
            self.cloud.refreshAccounts()
            self.reload()
        }
    }

    /// Pushed in Settings; over the page as a sheet where there's no stack
    /// (onboarding).
    private func show(_ vc: UIViewController) {
        if let nav = navigationController {
            return nav.pushViewController(vc, animated: true)
        }
        vc.navigationItem.rightBarButtonItem = UIBarButtonItem(systemItem: .done, primaryAction: UIAction { [weak vc] _ in
            vc?.dismiss(animated: true)
        })
        present(UINavigationController(rootViewController: vc), animated: true)
    }

    func collectionView(_ cv: UICollectionView, didSelectItemAt path: IndexPath) {
        cv.deselectItem(at: path, animated: true)
        guard let row = dataSource.itemIdentifier(for: path), row.disclosure, let harness = row.harness else { return }
        if onCloud {
            guard let agent = CloudStore.agents.first(where: { $0.harness == harness }) else { return }
            show(AgentAccountsViewController(app: app, agent: agent))
        } else {
            let vc = DeviceAccountsViewController(app: app, device: target, deviceName: targetName, harness: harness, label: row.title, accounts: accounts[target] ?? [])
            vc.onChange = { [weak self] list in
                guard let self else { return }
                self.accounts[vc.device] = list
                self.reload()
            }
            show(vc)
        }
    }
}

/// One agent's logins on a computer: the one in use, switching, removing.
/// New logins are made on that computer (its browser reaches its loopback).
final class DeviceAccountsViewController: UITableViewController {
    private let app: AppModel
    let device: String
    private let deviceName: String
    private let harness: String
    private var all: [DeviceAccountInfo]
    private var busy: String?
    var onChange: (([DeviceAccountInfo]) -> Void)?

    init(app: AppModel, device: String, deviceName: String, harness: String, label: String, accounts: [DeviceAccountInfo]) {
        self.app = app
        self.device = device
        self.deviceName = deviceName
        self.harness = harness
        self.all = accounts
        super.init(style: .insetGrouped)
        title = label
    }

    required init?(coder: NSCoder) { fatalError() }

    private var rows: [DeviceAccountInfo] { all.filter { $0.harness == harness } }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        tableView.backgroundColor = .clear
        navigationItem.largeTitleDisplayMode = .never
        tableView.register(UITableViewCell.self, forCellReuseIdentifier: "account")
    }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int { rows.count }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        "Accounts on \(deviceName)"
    }

    override func tableView(_ tableView: UITableView, titleForFooterInSection section: Int) -> String? {
        "New sessions on \(deviceName) use the account marked in use. Tap another to switch; swipe to remove. To add one, sign in from Zeron on that computer."
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let a = rows[indexPath.row]
        let cell = tableView.dequeueReusableCell(withIdentifier: "account", for: indexPath)
        var c = UIListContentConfiguration.subtitleCell()
        c.text = ProvidersViewController.accountTitle(a)
        c.textProperties.font = Fonts.ui(.sansMedium, 16)
        c.textProperties.color = a.switchable || a.active ? Palette.text : Palette.tertiary
        c.secondaryText = [a.planLabel, a.active ? "In use" : nil, a.switchable ? nil : "Can't switch from here"].compactMap { $0 }.joined(separator: " · ").nonEmpty
        c.secondaryTextProperties.font = Fonts.ui(.sans, 13)
        c.secondaryTextProperties.color = Palette.secondary
        c.image = a.apiKey ? CloudUI.symbol("key.fill", 15) : BrandMarks.image(for: harness, side: 20)
        c.imageProperties.tintColor = Palette.text
        c.imageProperties.reservedLayoutSize = CGSize(width: 28, height: 28)
        cell.contentConfiguration = c
        cell.backgroundColor = Palette.elevated
        if busy == a.id {
            let spinner = UIActivityIndicatorView(style: .medium)
            spinner.startAnimating()
            cell.accessoryView = spinner
            cell.accessoryType = .none
        } else {
            cell.accessoryView = nil
            cell.accessoryType = a.active ? .checkmark : .none
        }
        cell.tintColor = Palette.accent
        cell.accessibilityIdentifier = "device-account-\(a.id)"
        return cell
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        let a = rows[indexPath.row]
        guard busy == nil, !a.active, a.switchable else { return }
        run(a.id) { client in try await client.deviceActivateAccount(deviceId: self.device, harness: self.harness, accountId: a.id) }
    }

    override func tableView(_ tableView: UITableView, trailingSwipeActionsConfigurationForRowAt indexPath: IndexPath) -> UISwipeActionsConfiguration? {
        let a = rows[indexPath.row]
        guard a.switchable, busy == nil else { return nil }
        let remove = UIContextualAction(style: .destructive, title: "Remove") { [weak self] _, _, done in
            guard let self else { return done(false) }
            self.run(a.id) { client in try await client.deviceForgetAccount(deviceId: self.device, harness: self.harness, accountId: a.id) }
            done(true)
        }
        return UISwipeActionsConfiguration(actions: [remove])
    }

    private func run(_ id: String, _ work: @escaping (CoreClient) async throws -> [DeviceAccountInfo]) {
        guard let client = app.client else { return }
        busy = id
        tableView.reloadData()
        Task { @MainActor in
            do {
                self.all = try await work(client)
                self.onChange?(self.all)
                UISelectionFeedbackGenerator().selectionChanged()
            } catch {
                let a = UIAlertController(title: "Couldn't update the account", message: CloudStore.message(error), preferredStyle: .alert)
                a.addAction(UIAlertAction(title: "OK", style: .default))
                self.present(a, animated: true)
            }
            self.busy = nil
            self.tableView.reloadData()
        }
    }
}
