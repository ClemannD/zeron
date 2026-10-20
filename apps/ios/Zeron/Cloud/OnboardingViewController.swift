import UIKit

/// First run after signing in: connect GitHub, sign in to the agents (for
/// Cloud), notifications, and a first Cloud session. Every step can be
/// skipped and redone from Settings → Cloud.
final class OnboardingViewController: UIViewController {
    enum Step: Int, CaseIterable {
        case github, agents, notifications, done
    }

    private let app: AppModel
    private var step: Step
    private let signIn = ProviderSignIn()
    private var signingIn: String?
    private var token: AnyObject?
    /// Called when the flow ends; `startSession` asks for the new-session
    /// page (opened on a Cloud project when one was picked).
    var onFinish: ((_ startSession: Bool) -> Void)?

    private let dots = UIStackView()
    private let page = UIView()
    private let footer = UIStackView()

    static func shouldShow(for app: AppModel) -> Bool {
        guard let client = app.client, !app.isDemo else { return false }
        return !UserDefaults.standard.bool(forKey: key(client))
    }

    static func markDone(for app: AppModel) {
        guard let client = app.client else { return }
        UserDefaults.standard.set(true, forKey: key(client))
    }

    private static func key(_ client: CoreClient) -> String {
        "onboarded:\(client.userId())"
    }

    init(app: AppModel, step: Step = .github) {
        self.app = app
        self.step = step
        super.init(nibName: nil, bundle: nil)
        modalPresentationStyle = .fullScreen
    }

    required init?(coder: NSCoder) { fatalError() }

    @MainActor private var cloud: CloudStore { app.cloud }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        dots.axis = .horizontal
        dots.spacing = 6
        dots.translatesAutoresizingMaskIntoConstraints = false
        page.translatesAutoresizingMaskIntoConstraints = false
        footer.axis = .vertical
        footer.spacing = 6
        footer.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(dots)
        view.addSubview(page)
        view.addSubview(footer)
        let guide = view.safeAreaLayoutGuide
        let width = page.widthAnchor.constraint(equalToConstant: 520)
        width.priority = .defaultHigh
        NSLayoutConstraint.activate([
            dots.topAnchor.constraint(equalTo: guide.topAnchor, constant: 18),
            dots.centerXAnchor.constraint(equalTo: guide.centerXAnchor),
            page.topAnchor.constraint(equalTo: dots.bottomAnchor, constant: 36),
            page.centerXAnchor.constraint(equalTo: guide.centerXAnchor),
            page.leadingAnchor.constraint(greaterThanOrEqualTo: guide.leadingAnchor, constant: 24),
            page.trailingAnchor.constraint(lessThanOrEqualTo: guide.trailingAnchor, constant: -24),
            width,
            footer.leadingAnchor.constraint(equalTo: page.leadingAnchor),
            footer.trailingAnchor.constraint(equalTo: page.trailingAnchor),
            footer.bottomAnchor.constraint(equalTo: guide.bottomAnchor, constant: -16),
            page.bottomAnchor.constraint(lessThanOrEqualTo: footer.topAnchor, constant: -16),
        ])
        token = cloud.observe { [weak self] in self?.refreshRows() }
        cloud.refresh()
        render(animated: false)
    }

    // MARK: Pages

    private var rowsRefresher: (() -> Void)?

    private func refreshRows() { rowsRefresher?() }

    private func render(animated: Bool) {
        rowsRefresher = nil
        dots.arrangedSubviews.forEach { $0.removeFromSuperview() }
        for s in Step.allCases where s != .done {
            let dot = UIView()
            dot.backgroundColor = s.rawValue <= step.rawValue ? Palette.accent : Palette.controlFill
            dot.layer.cornerRadius = 2
            dot.translatesAutoresizingMaskIntoConstraints = false
            NSLayoutConstraint.activate([dot.widthAnchor.constraint(equalToConstant: 26), dot.heightAnchor.constraint(equalToConstant: 4)])
            dots.addArrangedSubview(dot)
        }
        dots.isHidden = step == .done
        let content: UIView
        switch step {
        case .github: content = githubPage()
        case .agents: content = agentsPage()
        case .notifications: content = notificationsPage()
        case .done: content = donePage()
        }
        content.translatesAutoresizingMaskIntoConstraints = false
        let old = page.subviews
        page.addSubview(content)
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: page.topAnchor),
            content.leadingAnchor.constraint(equalTo: page.leadingAnchor),
            content.trailingAnchor.constraint(equalTo: page.trailingAnchor),
            content.bottomAnchor.constraint(lessThanOrEqualTo: page.bottomAnchor),
        ])
        guard animated, !UIAccessibility.isReduceMotionEnabled else {
            old.forEach { $0.removeFromSuperview() }
            return
        }
        content.alpha = 0
        content.transform = CGAffineTransform(translationX: 24, y: 0)
        UIView.animate(withDuration: 0.35, delay: 0, options: [.curveEaseOut]) {
            content.alpha = 1
            content.transform = .identity
            old.forEach { $0.alpha = 0 }
        } completion: { _ in
            old.forEach { $0.removeFromSuperview() }
        }
    }

    private func setFooter(primary: UIButton, secondary: UIButton?) {
        footer.arrangedSubviews.forEach { $0.removeFromSuperview() }
        footer.addArrangedSubview(primary)
        if let secondary { footer.addArrangedSubview(secondary) }
    }

    private func go(_ next: Step) {
        step = next
        render(animated: true)
    }

    private func pageStack(_ views: [UIView]) -> UIStackView {
        let stack = UIStackView(arrangedSubviews: views)
        stack.axis = .vertical
        stack.spacing = 28
        return stack
    }

    private func githubPage() -> UIView {
        let header = CloudUI.header("Connect GitHub", "Cloud agents work in your repositories on machines of their own — they keep going while your computer sleeps.")
        let row = CloudRow(tile: CloudUI.tile(image: CloudUI.githubMark), title: "GitHub", detail: nil)
        row.accessibilityIdentifier = "onboarding-github"
        let card = CloudCard(rows: [row])
        rowsRefresher = { [weak self, weak row] in
            guard let self, let row else { return }
            if self.cloud.githubConnected {
                row.titleLabel.text = self.cloud.githubAccount.map { "@\($0)" } ?? "GitHub"
                let n = self.cloud.repos?.count ?? 0
                row.detailLabel.text = n == 1 ? "1 repository" : "\(n) repositories"
                row.setAccessory(CloudRow.check())
            } else {
                row.titleLabel.text = "GitHub"
                row.detailLabel.text = "Clone and push from Cloud"
                row.setAccessory(CloudRow.rowButton("Connect") { [weak self] in self?.connectGitHub() })
            }
        }
        refreshRows()
        setFooter(
            primary: CloudUI.primaryButton("Continue") { [weak self] in self?.go(.agents) },
            secondary: CloudUI.plainButton("Skip for now") { [weak self] in self?.go(.agents) }
        )
        return pageStack([header, card])
    }

    private func agentsPage() -> UIView {
        let header = CloudUI.header("Sign in to your agents", "Cloud runs Claude Code and Codex with your own subscriptions. Cloud keeps the sign-in — not this phone.")
        let rows = CloudStore.agents.map { agent -> CloudRow in
            let row = CloudRow(tile: CloudUI.tile(image: BrandMarks.image(for: agent.harness, side: 20)), title: agent.name, detail: nil)
            row.accessibilityIdentifier = "onboarding-\(agent.harness)"
            return row
        }
        let card = CloudCard(rows: rows)
        rowsRefresher = { [weak self] in
            guard let self else { return }
            for (agent, row) in zip(CloudStore.agents, rows) {
                if self.signingIn == agent.login {
                    row.detailLabel.text = "Finishing sign-in…"
                    let spinner = UIActivityIndicatorView(style: .medium)
                    spinner.startAnimating()
                    row.setAccessory(spinner)
                } else if let active = self.cloud.activeAccount(agent) {
                    row.detailLabel.text = [active.email ?? (active.kind == "key" ? agent.keyName : nil), active.plan].compactMap { $0 }.joined(separator: " · ").nonEmpty ?? "Signed in"
                    row.setAccessory(CloudRow.check())
                } else {
                    row.detailLabel.text = agent.login == "claude" ? "Your Claude subscription" : "Your ChatGPT subscription"
                    row.setAccessory(CloudRow.rowButton("Sign In") { [weak self] in self?.signIn(agent) })
                }
            }
        }
        refreshRows()
        setFooter(
            primary: CloudUI.primaryButton("Continue") { [weak self] in self?.go(.notifications) },
            secondary: CloudUI.plainButton("Skip for now") { [weak self] in self?.go(.notifications) }
        )
        return pageStack([header, card])
    }

    private func notificationsPage() -> UIView {
        let header = CloudUI.header("Stay in the loop", "Get a notification when an agent finishes, needs an answer, or runs into trouble.")
        let rows = [
            ("checkmark.circle", "Finished", "A session is done"),
            ("questionmark.bubble", "Needs you", "An agent is waiting on an answer"),
            ("exclamationmark.triangle", "Something went wrong", "A run failed"),
        ].map { symbol, title, detail in
            CloudRow(tile: CloudUI.tile(image: CloudUI.symbol(symbol, 18)), title: title, detail: detail)
        }
        setFooter(
            primary: CloudUI.primaryButton("Turn On Notifications") { [weak self] in
                Task { @MainActor in
                    PushNotifications.shared.enabled = true
                    await PushNotifications.shared.requestPermission()
                    self?.go(.done)
                }
            },
            secondary: CloudUI.plainButton("Not now") { [weak self] in self?.go(.done) }
        )
        return pageStack([header, CloudCard(rows: rows)])
    }

    private func donePage() -> UIView {
        let check = UIImageView(image: CloudUI.symbol("checkmark.circle.fill", 54, weight: .regular))
        check.tintColor = Palette.accent
        check.contentMode = .scaleAspectFit
        let header = CloudUI.header("You're all set", "Start a session on Cloud, or on any computer running Zeron.", alignment: .center)
        let summary = CloudCard(rows: [])
        rowsRefresher = { [weak self, weak summary] in
            guard let self, let summary else { return }
            var rows: [UIView] = []
            let github = CloudRow(tile: CloudUI.tile(image: CloudUI.githubMark), title: "GitHub", detail: self.cloud.githubAccount.map { "@\($0)" } ?? "Not connected")
            github.setAccessory(self.cloud.githubConnected ? CloudRow.check() : nil)
            rows.append(github)
            for agent in CloudStore.agents {
                let active = self.cloud.activeAccount(agent)
                let row = CloudRow(tile: CloudUI.tile(image: BrandMarks.image(for: agent.harness, side: 20)), title: agent.name, detail: active.map { $0.email ?? "Signed in" } ?? "Not signed in")
                row.setAccessory(active != nil ? CloudRow.check() : nil)
                rows.append(row)
            }
            summary.setRows(rows)
        }
        refreshRows()
        let top = UIStackView(arrangedSubviews: [check, header])
        top.axis = .vertical
        top.alignment = .center
        top.spacing = 18
        setFooter(
            primary: CloudUI.primaryButton("Start a Session", symbol: "plus") { [weak self] in self?.finish(startSession: true) },
            secondary: CloudUI.plainButton("Done") { [weak self] in self?.finish(startSession: false) }
        )
        return pageStack([top, summary])
    }

    // MARK: Actions

    private func connectGitHub() {
        present(GitHubConnectViewController.sheet(app: app) { [weak self] _ in self?.refreshRows() }, animated: true)
    }

    private func signIn(_ agent: CloudStore.Agent) {
        guard let client = app.client, signingIn == nil else { return }
        signingIn = agent.login
        refreshRows()
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
            self.refreshRows()
        }
    }

    private func finish(startSession: Bool) {
        Self.markDone(for: app)
        let done = onFinish
        dismiss(animated: true) { done?(startSession) }
    }
}
