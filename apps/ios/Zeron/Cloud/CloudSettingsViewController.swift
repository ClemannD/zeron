import SafariServices
import UIKit

/// Settings → Cloud: credits and plan, GitHub, the agents' accounts, billing.
final class CloudSettingsViewController: UIViewController, UICollectionViewDelegate {
    private let app: AppModel
    private var collectionView: UICollectionView!
    private var dataSource: UICollectionViewDiffableDataSource<String, Row>!
    private var token: AnyObject?
    private let header = CreditsHeader()

    struct Row: Hashable {
        let id: String
        let title: String
        var subtitle: String?
        var image: UIImage?
        var tint: UIColor = Palette.text
        var accessory: String? = nil
    }

    init(app: AppModel) {
        self.app = app
        super.init(nibName: nil, bundle: nil)
        title = "Cloud"
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
        collectionView = UICollectionView(frame: view.bounds, collectionViewLayout: UICollectionViewCompositionalLayout.list(using: config))
        collectionView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        collectionView.backgroundColor = .clear
        collectionView.delegate = self
        collectionView.refreshControl = UIRefreshControl(frame: .zero, primaryAction: UIAction { [weak self] _ in
            self?.cloud.refresh()
        })
        view.addSubview(collectionView)

        let cell = UICollectionView.CellRegistration<UICollectionViewListCell, Row> { cell, _, row in
            var c = UIListContentConfiguration.subtitleCell()
            c.text = row.title
            c.textProperties.font = Fonts.ui(.sansMedium, 16)
            c.textProperties.color = Palette.text
            c.secondaryText = row.subtitle
            c.secondaryTextProperties.font = Fonts.ui(.sans, 13)
            c.secondaryTextProperties.color = Palette.secondary
            c.image = row.image
            c.imageProperties.tintColor = row.tint
            c.imageProperties.maximumSize = CGSize(width: 22, height: 22)
            c.imageProperties.reservedLayoutSize = CGSize(width: 28, height: 28)
            cell.contentConfiguration = c
            var bg = UIBackgroundConfiguration.listGroupedCell()
            bg.backgroundColor = Palette.elevated
            cell.backgroundConfiguration = bg
            if let accessory = row.accessory {
                let label = UILabel()
                label.text = accessory
                label.font = Fonts.ui(.sansMedium, 14)
                label.textColor = Palette.accent
                cell.accessories = [.customView(configuration: .init(customView: label, placement: .trailing())), .disclosureIndicator()]
            } else {
                cell.accessories = [.disclosureIndicator()]
            }
            cell.accessibilityIdentifier = "cloud-\(row.id)"
        }
        let headerReg = UICollectionView.SupplementaryRegistration<UICollectionViewListCell>(elementKind: UICollectionView.elementKindSectionHeader) { [weak self] view, _, path in
            var c = UIListContentConfiguration.groupedHeader()
            let id = self?.dataSource.snapshot().sectionIdentifiers[path.section] ?? ""
            c.text = id == "credits" ? nil : id
            view.contentConfiguration = c
        }
        let footerReg = UICollectionView.SupplementaryRegistration<UICollectionViewListCell>(elementKind: UICollectionView.elementKindSectionFooter) { [weak self] view, _, path in
            var c = UIListContentConfiguration.groupedFooter()
            switch self?.dataSource.snapshot().sectionIdentifiers[path.section] {
            case "GitHub": c.text = "Cloud agents clone and push to the repositories the Zeron GitHub App is installed on."
            case "Agents": c.text = "Cloud agents use the account marked in use. Signing in here stores the login in Cloud, not on this phone."
            default: c.text = nil
            }
            view.contentConfiguration = c
        }
        let creditsReg = UICollectionView.CellRegistration<UICollectionViewListCell, Row> { [weak self] cell, _, _ in
            guard let self else { return }
            cell.contentConfiguration = nil
            cell.contentView.subviews.forEach { $0.removeFromSuperview() }
            self.header.translatesAutoresizingMaskIntoConstraints = false
            cell.contentView.addSubview(self.header)
            NSLayoutConstraint.activate([
                self.header.topAnchor.constraint(equalTo: cell.contentView.topAnchor),
                self.header.bottomAnchor.constraint(equalTo: cell.contentView.bottomAnchor),
                self.header.leadingAnchor.constraint(equalTo: cell.contentView.leadingAnchor),
                self.header.trailingAnchor.constraint(equalTo: cell.contentView.trailingAnchor),
            ])
            var bg = UIBackgroundConfiguration.listGroupedCell()
            bg.backgroundColor = Palette.elevated
            cell.backgroundConfiguration = bg
            cell.accessories = []
        }
        dataSource = UICollectionViewDiffableDataSource(collectionView: collectionView) { cv, path, row in
            if row.id == "credits" {
                return cv.dequeueConfiguredReusableCell(using: creditsReg, for: path, item: row)
            }
            return cv.dequeueConfiguredReusableCell(using: cell, for: path, item: row)
        }
        dataSource.supplementaryViewProvider = { cv, kind, path in
            kind == UICollectionView.elementKindSectionHeader
                ? cv.dequeueConfiguredReusableSupplementary(using: headerReg, for: path)
                : cv.dequeueConfiguredReusableSupplementary(using: footerReg, for: path)
        }
        header.onGetCredits = { [weak self] in self?.openBilling() }
        token = cloud.observe { [weak self] in self?.reload() }
        reload()
        cloud.refresh()
    }

    private func reload() {
        if !cloud.loading { collectionView.refreshControl?.endRefreshing() }
        header.update(credits: cloud.credits, billing: cloud.billing, loading: cloud.info == nil)
        var s = NSDiffableDataSourceSnapshot<String, Row>()
        s.appendSections(["credits"])
        // Rebuilt with the header's content, so the cell re-lays out.
        s.appendItems([Row(id: "credits", title: "\(cloud.credits ?? -1)|\(cloud.billing?.plan?.name ?? "")")])
        guard cloud.info == nil || cloud.available else {
            s.appendSections(["Cloud"])
            s.appendItems([Row(id: "unavailable", title: "Cloud isn't available", subtitle: cloud.error ?? "This account can't use Cloud yet.", image: CloudUI.symbol("cloud"), tint: Palette.secondary)])
            dataSource.apply(s, animatingDifferences: false)
            return
        }
        s.appendSections(["GitHub"])
        let repoCount = cloud.repos?.count ?? 0
        s.appendItems([Row(
            id: "github",
            title: cloud.githubAccount.map { "@\($0)" } ?? "GitHub",
            subtitle: cloud.githubConnected ? (repoCount == 1 ? "1 repository" : "\(repoCount) repositories") : "Connect so Cloud agents can clone and push",
            image: CloudUI.githubMark,
            accessory: cloud.githubConnected ? nil : "Connect"
        )])
        s.appendSections(["Agents"])
        s.appendItems(CloudStore.agents.map { agent in
            let active = cloud.activeAccount(agent)
            let detail: String
            if let active {
                detail = [active.email ?? (active.kind == "key" ? agent.keyName : nil), active.plan].compactMap { $0 }.joined(separator: " · ")
            } else {
                detail = "Not signed in"
            }
            return Row(id: "agent:\(agent.harness)", title: agent.name, subtitle: detail, image: BrandMarks.image(for: agent.harness, side: 20), accessory: active == nil ? "Sign in" : nil)
        })
        s.appendSections(["Billing"])
        let plan = cloud.billing?.plan
        s.appendItems([Row(
            id: "billing",
            title: plan.map { "\($0.name) plan" } ?? "Plans & credits",
            subtitle: plan.map { "\(CloudUI.price($0.priceCents)) a month · \(CloudUI.credits($0.monthlyCredits)) credits" } ?? "Choose a plan or buy extra credits",
            image: CloudUI.symbol("creditcard"),
            tint: Palette.text
        )])
        dataSource.apply(s, animatingDifferences: false)
    }

    func collectionView(_ cv: UICollectionView, didSelectItemAt path: IndexPath) {
        cv.deselectItem(at: path, animated: true)
        guard let row = dataSource.itemIdentifier(for: path) else { return }
        switch row.id {
        case "github":
            if cloud.githubConnected {
                githubActions(from: cv.cellForItem(at: path))
            } else {
                present(GitHubConnectViewController.sheet(app: app), animated: true)
            }
        case "billing":
            openBilling()
        case let id where id.hasPrefix("agent:"):
            let harness = String(id.dropFirst("agent:".count))
            guard let agent = CloudStore.agents.first(where: { $0.harness == harness }) else { return }
            navigationController?.pushViewController(AgentAccountsViewController(app: app, agent: agent), animated: true)
        default:
            break
        }
    }

    private func githubActions(from source: UIView?) {
        let sheet = UIAlertController(title: cloud.githubAccount.map { "@\($0)" }, message: "Cloud reaches the repositories the Zeron GitHub App is installed on.", preferredStyle: .actionSheet)
        if let url = cloud.vault?.githubInstallUrl.flatMap(URL.init(string:)) {
            sheet.addAction(UIAlertAction(title: "Choose Repositories on GitHub", style: .default) { [weak self] _ in
                self?.present(SFSafariViewController(url: url), animated: true)
            })
        }
        sheet.addAction(UIAlertAction(title: "Reconnect", style: .default) { [weak self] _ in
            guard let self else { return }
            self.present(GitHubConnectViewController.sheet(app: self.app), animated: true)
        })
        sheet.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        sheet.popoverPresentationController?.sourceView = source
        present(sheet, animated: true)
    }

    private func openBilling() {
        navigationController?.pushViewController(BillingViewController(app: app), animated: true)
    }
}

/// The page's top: credits left, the plan, a way to get more.
final class CreditsHeader: UIView {
    private let value = UILabel()
    private let caption = UILabel()
    private let bar = UIProgressView(progressViewStyle: .bar)
    var onGetCredits: (() -> Void)?

    override init(frame: CGRect) {
        super.init(frame: frame)
        let icon = UIImageView(image: CloudUI.symbol("cloud.fill", 15, weight: .semibold))
        icon.tintColor = Palette.accent
        let eyebrow = UILabel()
        eyebrow.text = "Cloud credits"
        eyebrow.font = Fonts.ui(.sansMedium, 14)
        eyebrow.textColor = Palette.secondary
        let top = UIStackView(arrangedSubviews: [icon, eyebrow])
        top.spacing = 6
        top.alignment = .center
        value.font = Fonts.ui(.sansSemibold, 40)
        value.textColor = Palette.text
        value.accessibilityIdentifier = "cloud-credits"
        caption.font = Fonts.ui(.sans, 13)
        caption.textColor = Palette.secondary
        caption.numberOfLines = 0
        bar.trackTintColor = Palette.controlFill
        bar.progressTintColor = Palette.accent
        bar.layer.cornerRadius = 3
        bar.clipsToBounds = true
        let get = CloudRow.rowButton("Get credits", prominent: false) { [weak self] in self?.onGetCredits?() }
        let stack = UIStackView(arrangedSubviews: [top, value, bar, caption, get])
        stack.axis = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.setCustomSpacing(4, after: top)
        stack.setCustomSpacing(14, after: bar)
        stack.setCustomSpacing(14, after: caption)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 18),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -18),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 18),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -18),
            bar.widthAnchor.constraint(equalTo: stack.widthAnchor),
            bar.heightAnchor.constraint(equalToConstant: 6),
        ])
    }

    required init?(coder: NSCoder) { fatalError() }

    func update(credits: Double?, billing: BillingInfo?, loading: Bool) {
        value.text = credits.map(CloudUI.credits) ?? (loading ? "—" : "0")
        if let billing, let plan = billing.plan {
            let total = max(billing.creditsMonthlyTotal, 1)
            bar.progress = Float(min(1, billing.creditsMonthly / total))
            var parts = ["\(CloudUI.credits(billing.creditsMonthly)) of \(CloudUI.credits(billing.creditsMonthlyTotal)) left this month on \(plan.name)"]
            if billing.creditsExtra > 0 { parts.append("\(CloudUI.credits(billing.creditsExtra)) extra that never expire") }
            caption.text = parts.joined(separator: " · ")
        } else {
            bar.progress = credits.map { $0 > 0 ? 1 : 0 } ?? 0
            caption.text = "No plan yet — choose one for monthly credits."
        }
    }
}
