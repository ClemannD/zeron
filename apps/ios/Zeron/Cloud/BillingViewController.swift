import SafariServices
import UIKit

/// Plans and credits: the three monthly plans, extra credits that never
/// expire, and the Stripe portal for changing or cancelling. Payment pages
/// open in a browser sheet; the page re-reads when it closes.
final class BillingViewController: UIViewController, SFSafariViewControllerDelegate {
    private let app: AppModel
    private var token: AnyObject?
    private let scroll = UIScrollView()
    private let stack = UIStackView()
    private var packs = 1
    private var busy = false

    init(app: AppModel) {
        self.app = app
        super.init(nibName: nil, bundle: nil)
        title = "Plans & Credits"
    }

    required init?(coder: NSCoder) { fatalError() }

    @MainActor private var cloud: CloudStore { app.cloud }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        navigationItem.largeTitleDisplayMode = .never
        scroll.alwaysBounceVertical = true
        scroll.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(scroll)
        stack.axis = .vertical
        stack.spacing = 14
        stack.translatesAutoresizingMaskIntoConstraints = false
        scroll.addSubview(stack)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: view.topAnchor),
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 12),
            stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -32),
            stack.leadingAnchor.constraint(equalTo: scroll.frameLayoutGuide.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: scroll.frameLayoutGuide.trailingAnchor, constant: -16),
        ])
        token = cloud.observe { [weak self] in self?.render() }
        render()
        Task { await cloud.refreshBilling() }
    }

    private func render() {
        stack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        let billing = cloud.billing
        let header = CreditsHeader()
        header.update(credits: cloud.credits, billing: billing, loading: billing == nil)
        header.onGetCredits = { [weak self] in self?.scrollToCredits() }
        stack.addArrangedSubview(card(header))

        guard let billing else { return }
        guard billing.available else {
            stack.addArrangedSubview(note("Payments aren't set up on this server yet."))
            return
        }
        stack.addArrangedSubview(sectionLabel("Monthly plans"))
        let current = billing.plan?.id
        for plan in billing.plans {
            let isCurrent = plan.id == current
            let row = CloudRow(tile: planTile(plan), title: "\(plan.name) · \(CloudUI.price(plan.priceCents)) a month", detail: "\(CloudUI.credits(plan.monthlyCredits)) credits every month")
            let action: UIView
            if isCurrent {
                let label = UILabel()
                label.text = "Current"
                label.font = Fonts.ui(.sansSemibold, 14)
                label.textColor = Palette.accent
                action = label
            } else if current != nil {
                action = CloudRow.rowButton("Switch", prominent: false) { [weak self] in self?.portal() }
            } else {
                action = CloudRow.rowButton("Choose") { [weak self] in self?.checkout(kind: "subscription", plan: plan.id) }
            }
            row.setAccessory(action)
            row.accessibilityIdentifier = "plan-\(plan.id)"
            stack.addArrangedSubview(card(row, highlighted: isCurrent))
        }
        if let plan = billing.plan {
            let renew: String
            if billing.cancelAtPeriodEnd {
                renew = billing.periodEndMs.map { "Ends \(CloudUI.shortDate($0)) — you keep its credits until then." } ?? "Won't renew."
            } else if billing.planStatus == "past_due" {
                renew = "The last payment didn't go through — update your card."
            } else {
                renew = billing.periodEndMs.map { "Renews \(CloudUI.shortDate($0))." } ?? "Renews monthly."
            }
            let manage = CloudRow(tile: CloudUI.tile(image: CloudUI.symbol("creditcard", 18)), title: "Manage subscription", detail: "\(plan.name) plan · \(renew)")
            manage.setAccessory(CloudRow.rowButton("Open", prominent: false) { [weak self] in self?.portal() })
            stack.addArrangedSubview(card(manage))
        }

        stack.addArrangedSubview(sectionLabel("Extra credits"))
        let total = Double(packs) * billing.packCredits
        let extra = CloudRow(tile: CloudUI.tile(image: CloudUI.symbol("plus.circle", 18)), title: "\(CloudUI.credits(total)) credits", detail: "\(CloudUI.price(billing.packPriceCents * UInt32(packs))) · never expire")
        let stepper = UIStepper()
        stepper.minimumValue = 1
        stepper.maximumValue = 20
        stepper.value = Double(packs)
        stepper.addAction(UIAction { [weak self, weak stepper] _ in
            self?.packs = Int(stepper?.value ?? 1)
            self?.render()
        }, for: .valueChanged)
        extra.setAccessory(stepper)
        stack.addArrangedSubview(card(extra))
        let buy = CloudUI.primaryButton("Buy for \(CloudUI.price(billing.packPriceCents * UInt32(packs)))") { [weak self] in
            guard let self else { return }
            self.checkout(kind: "credits", packs: self.packs)
        }
        buy.accessibilityIdentifier = "buy-credits"
        stack.addArrangedSubview(buy)
    }

    private func planTile(_ plan: BillingPlanInfo) -> UIView {
        let symbol = switch plan.id {
        case "max": "bolt.fill"
        case "pro": "sparkles"
        default: "cloud.fill"
        }
        return CloudUI.tile(image: CloudUI.symbol(symbol, 17), tint: Palette.accent)
    }

    private func card(_ content: UIView, highlighted: Bool = false) -> UIView {
        let card = UIView()
        card.backgroundColor = Palette.elevated
        card.layer.cornerRadius = 20
        card.layer.cornerCurve = .continuous
        if highlighted {
            card.layer.borderWidth = 1.5
            card.layer.borderColor = Palette.accent.resolvedColor(with: traitCollection).cgColor
        }
        content.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(content)
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: card.topAnchor),
            content.bottomAnchor.constraint(equalTo: card.bottomAnchor),
            content.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            content.trailingAnchor.constraint(equalTo: card.trailingAnchor),
        ])
        return card
    }

    private func sectionLabel(_ text: String) -> UIView {
        let label = UILabel()
        label.text = text
        label.font = Fonts.ui(.sansSemibold, 15)
        label.textColor = Palette.secondary
        let holder = UIView()
        label.translatesAutoresizingMaskIntoConstraints = false
        holder.addSubview(label)
        NSLayoutConstraint.activate([
            label.topAnchor.constraint(equalTo: holder.topAnchor, constant: 10),
            label.bottomAnchor.constraint(equalTo: holder.bottomAnchor),
            label.leadingAnchor.constraint(equalTo: holder.leadingAnchor, constant: 6),
            label.trailingAnchor.constraint(equalTo: holder.trailingAnchor),
        ])
        return holder
    }

    private func note(_ text: String) -> UILabel {
        let l = UILabel()
        l.text = text
        l.font = Fonts.ui(.sans, 14)
        l.textColor = Palette.secondary
        l.numberOfLines = 0
        return l
    }

    private func scrollToCredits() {
        scroll.setContentOffset(CGPoint(x: 0, y: max(0, scroll.contentSize.height - scroll.bounds.height + scroll.adjustedContentInset.bottom)), animated: true)
    }

    private func checkout(kind: String, plan: String? = nil, packs: Int? = nil) {
        open { client in try await client.billingCheckout(kind: kind, plan: plan, packs: packs.map(UInt32.init)) }
    }

    private func portal() {
        open { client in try await client.billingPortal() }
    }

    private func open(_ link: @escaping (CoreClient) async throws -> String) {
        guard !busy, let client = app.client else { return }
        busy = true
        Task { @MainActor in
            defer { self.busy = false }
            do {
                guard let url = URL(string: try await link(client)) else { return }
                let safari = SFSafariViewController(url: url)
                safari.delegate = self
                self.present(safari, animated: true)
            } catch {
                let a = UIAlertController(title: "Couldn't open payments", message: CloudStore.message(error), preferredStyle: .alert)
                a.addAction(UIAlertAction(title: "OK", style: .default))
                self.present(a, animated: true)
            }
        }
    }

    /// Back from Stripe: Checkout lands through a webhook, so re-read for a
    /// little while until the change shows.
    func safariViewControllerDidFinish(_ controller: SFSafariViewController) {
        let before = (cloud.billing?.plan?.id, cloud.billing?.creditsTotal)
        Task { @MainActor in
            for _ in 0..<6 {
                await self.cloud.refreshBilling()
                if (self.cloud.billing?.plan?.id, self.cloud.billing?.creditsTotal) != before { return }
                try? await Task.sleep(nanoseconds: 2_000_000_000)
            }
        }
    }
}
