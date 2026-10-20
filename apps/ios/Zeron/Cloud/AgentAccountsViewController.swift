import UIKit

/// One agent's accounts on Cloud (Claude Code, Codex): every login and key
/// Cloud keeps, the one in use, and signing in from the phone.
final class AgentAccountsViewController: UIViewController, UICollectionViewDelegate {
    private let app: AppModel
    private let agent: CloudStore.Agent
    private var collectionView: UICollectionView!
    private var dataSource: UICollectionViewDiffableDataSource<String, Row>!
    private var token: AnyObject?
    private let signIn = ProviderSignIn()
    private var busy: String?

    struct Row: Hashable {
        let id: String
        let provider: String
        let slot: String?
        let title: String
        var subtitle: String?
        var active = false
        var kind: Kind = .account
        var busy = false

        enum Kind: Hashable { case account, signIn, apiKey }
    }

    init(app: AppModel, agent: CloudStore.Agent) {
        self.app = app
        self.agent = agent
        super.init(nibName: nil, bundle: nil)
        title = agent.name
    }

    required init?(coder: NSCoder) { fatalError() }

    @MainActor private var cloud: CloudStore { app.cloud }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        navigationItem.largeTitleDisplayMode = .never
        var config = UICollectionLayoutListConfiguration(appearance: .insetGrouped)
        config.backgroundColor = .clear
        config.headerMode = .supplementary
        config.footerMode = .supplementary
        config.trailingSwipeActionsConfigurationProvider = { [weak self] path in
            guard let self, let row = self.dataSource.itemIdentifier(for: path), row.kind == .account, row.slot != nil else { return nil }
            let remove = UIContextualAction(style: .destructive, title: "Remove") { [weak self] _, _, done in
                self?.remove(row)
                done(true)
            }
            return UISwipeActionsConfiguration(actions: [remove])
        }
        collectionView = UICollectionView(frame: view.bounds, collectionViewLayout: UICollectionViewCompositionalLayout.list(using: config))
        collectionView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        collectionView.backgroundColor = .clear
        collectionView.delegate = self
        view.addSubview(collectionView)

        let agent = self.agent
        let cell = UICollectionView.CellRegistration<UICollectionViewListCell, Row> { cell, _, row in
            var c = UIListContentConfiguration.subtitleCell()
            c.text = row.title
            c.secondaryText = row.subtitle
            c.secondaryTextProperties.font = Fonts.ui(.sans, 13)
            c.secondaryTextProperties.color = Palette.secondary
            switch row.kind {
            case .account:
                c.textProperties.font = Fonts.ui(.sansMedium, 16)
                c.textProperties.color = Palette.text
                c.image = row.provider == agent.key ? CloudUI.symbol("key.fill", 15) : BrandMarks.image(for: agent.harness, side: 20)
                c.imageProperties.tintColor = Palette.text
            case .signIn:
                c.textProperties.font = Fonts.ui(.sansSemibold, 16)
                c.textProperties.color = Palette.accent
                c.image = CloudUI.symbol("person.crop.circle.badge.plus", 18)
                c.imageProperties.tintColor = Palette.accent
            case .apiKey:
                c.textProperties.font = Fonts.ui(.sansMedium, 16)
                c.textProperties.color = Palette.text
                c.image = CloudUI.symbol("key", 16)
                c.imageProperties.tintColor = Palette.secondary
            }
            c.imageProperties.reservedLayoutSize = CGSize(width: 28, height: 28)
            cell.contentConfiguration = c
            var bg = UIBackgroundConfiguration.listGroupedCell()
            bg.backgroundColor = Palette.elevated
            cell.backgroundConfiguration = bg
            if row.busy {
                let spinner = UIActivityIndicatorView(style: .medium)
                spinner.startAnimating()
                cell.accessories = [.customView(configuration: .init(customView: spinner, placement: .trailing()))]
            } else if row.kind == .account {
                cell.accessories = row.active ? [.checkmark(options: .init(tintColor: Palette.accent))] : []
            } else {
                cell.accessories = []
            }
            cell.accessibilityIdentifier = "account-\(row.id)"
        }
        let header = UICollectionView.SupplementaryRegistration<UICollectionViewListCell>(elementKind: UICollectionView.elementKindSectionHeader) { [weak self] view, _, path in
            var c = UIListContentConfiguration.groupedHeader()
            let id = self?.dataSource.snapshot().sectionIdentifiers[path.section]
            c.text = id == "Accounts" ? "Accounts on Cloud" : nil
            view.contentConfiguration = c
        }
        let footer = UICollectionView.SupplementaryRegistration<UICollectionViewListCell>(elementKind: UICollectionView.elementKindSectionFooter) { [weak self] view, _, path in
            guard let self else { return }
            var c = UIListContentConfiguration.groupedFooter()
            let id = self.dataSource.snapshot().sectionIdentifiers[path.section]
            c.text = id == "Accounts"
                ? "Cloud agents use the account marked in use. Tap another to switch; swipe to remove."
                : "Signing in opens \(agent.login == "claude" ? "Claude" : "ChatGPT") in a browser sheet. The sign-in is stored in Cloud — never on this phone."
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
        token = cloud.observe { [weak self] in self?.reload() }
        reload()
        cloud.refreshAccounts()
    }

    private func reload() {
        var s = NSDiffableDataSourceSnapshot<String, Row>()
        let logins = (cloud.accounts[agent.login] ?? []).map { a in
            Row(id: "\(agent.login):\(a.slot)", provider: agent.login, slot: a.slot, title: a.email ?? a.displayName ?? "\(agent.name) account",
                subtitle: [a.plan, a.organization, a.needsReconnect ? "Sign in again" : nil, a.active ? "In use" : nil].compactMap { $0 }.joined(separator: " · ").nonEmpty,
                active: a.active, busy: busy == a.slot)
        }
        let keys = (cloud.accounts[agent.key] ?? []).map { a in
            Row(id: "\(agent.key):\(a.slot)", provider: agent.key, slot: a.slot, title: agent.keyName,
                subtitle: [a.active ? "In use" : nil].compactMap { $0 }.joined().nonEmpty,
                active: a.active, busy: busy == a.slot)
        }
        if !(logins + keys).isEmpty {
            s.appendSections(["Accounts"])
            s.appendItems(logins + keys)
        }
        s.appendSections(["Add"])
        s.appendItems([
            Row(id: "sign-in", provider: agent.login, slot: nil, title: agent.signInTitle, kind: .signIn, busy: busy == "sign-in"),
            Row(id: "api-key", provider: agent.key, slot: nil, title: "Use an API Key", subtitle: agent.keyName, kind: .apiKey),
        ])
        dataSource.apply(s, animatingDifferences: true)
    }

    func collectionView(_ cv: UICollectionView, didSelectItemAt path: IndexPath) {
        cv.deselectItem(at: path, animated: true)
        guard let row = dataSource.itemIdentifier(for: path), busy == nil else { return }
        switch row.kind {
        case .signIn: startSignIn()
        case .apiKey: askForKey()
        case .account:
            guard !row.active, let slot = row.slot,
                  let account = cloud.accounts[row.provider]?.first(where: { $0.slot == slot }) else { return }
            run(slot) { try await self.cloud.activate(account, provider: row.provider) }
        }
    }

    private func run(_ key: String, _ work: @escaping () async throws -> Void) {
        busy = key
        reload()
        Task { @MainActor in
            do { try await work() } catch is CancellationError {} catch {
                self.alert("Couldn't update the account", CloudStore.message(error))
            }
            self.busy = nil
            self.reload()
        }
    }

    private func startSignIn() {
        guard let client = app.client else { return }
        run("sign-in") {
            try await self.signIn.run(self.agent, client: client, from: self.view)
            UINotificationFeedbackGenerator().notificationOccurred(.success)
            self.cloud.refreshAccounts()
        }
    }

    private func askForKey() {
        let alert = UIAlertController(title: agent.keyName, message: "Cloud agents use this key instead of a subscription sign-in.", preferredStyle: .alert)
        alert.addTextField { field in
            field.placeholder = self.agent.key == "anthropic-key" ? "sk-ant-…" : "sk-…"
            field.isSecureTextEntry = true
            field.autocorrectionType = .no
            field.autocapitalizationType = .none
        }
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        alert.addAction(UIAlertAction(title: "Save", style: .default) { [weak self, weak alert] _ in
            guard let self, let key = alert?.textFields?.first?.text else { return }
            self.run("api-key") { try await self.cloud.saveKey(key, for: self.agent) }
        })
        present(alert, animated: true)
    }

    private func remove(_ row: Row) {
        guard let slot = row.slot, let account = cloud.accounts[row.provider]?.first(where: { $0.slot == slot }) else { return }
        run(slot) { try await self.cloud.remove(account, provider: row.provider) }
    }

    private func alert(_ title: String, _ message: String) {
        let a = UIAlertController(title: title, message: message, preferredStyle: .alert)
        a.addAction(UIAlertAction(title: "OK", style: .default))
        present(a, animated: true)
    }
}
