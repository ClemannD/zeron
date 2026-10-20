import AuthenticationServices
import UIKit

enum Endpoints {
    static let edgeURL = URL(string: "https://edge.zeron.sh")!
    static var workosClientId: String {
        #if DEBUG
        if let id = debugOverride("-workos", key: "debugWorkosClientId") { return id }
        #endif
        return "client_01KWD0EAKZKD50YCQJNYSRE4BY"
    }
    static let callbackScheme = "zeron"

    #if DEBUG
    /// A launch argument that sticks: `-edge <url>` / `-workos <client>` point
    /// the app at another stack (staging) and are remembered, so reopening it
    /// from the home screen stays there. `-edge prod` goes back.
    static func debugOverride(_ flag: String, key: String) -> String? {
        let args = ProcessInfo.processInfo.arguments
        if let i = args.firstIndex(of: flag), i + 1 < args.count {
            let value = args[i + 1]
            if value == "prod" {
                UserDefaults.standard.removeObject(forKey: key)
                return nil
            }
            UserDefaults.standard.set(value, forKey: key)
            return value
        }
        return UserDefaults.standard.string(forKey: key)
    }
    #endif

    static func authorizeURL(state: String) -> URL {
        var c = URLComponents(string: "https://api.workos.com/user_management/authorize")!
        c.queryItems = [
            URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "client_id", value: workosClientId),
            URLQueryItem(name: "redirect_uri", value: "\(callbackScheme)://callback"),
            URLQueryItem(name: "provider", value: "authkit"),
            URLQueryItem(name: "state", value: state),
        ]
        return c.url!
    }
}

/// Sign in with WorkOS (system web auth sheet).
final class SignInViewController: UIViewController, ASWebAuthenticationPresentationContextProviding {
    private let app: AppModel
    private let status = UILabel()
    private let signIn = UIButton(type: .system)
    private var session: ASWebAuthenticationSession?

    init(app: AppModel) {
        self.app = app
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError() }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        let mark = UIImageView(image: CloudUI.zeronMark)
        mark.tintColor = Palette.text
        mark.contentMode = .scaleAspectFit
        mark.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            mark.widthAnchor.constraint(equalToConstant: 46),
            mark.heightAnchor.constraint(equalToConstant: 53),
        ])
        let title = UILabel()
        title.text = "Welcome to Zeron"
        title.font = Fonts.ui(.sansSemibold, 32)
        title.textColor = Palette.text
        title.textAlignment = .center
        title.numberOfLines = 0
        let tagline = UILabel()
        tagline.text = "Your coding agents, from anywhere."
        tagline.font = Fonts.ui(.sans, 17)
        tagline.textColor = Palette.secondary
        tagline.textAlignment = .center
        tagline.numberOfLines = 0

        let features = [
            ("cloud", "Agents in the cloud", "Every session gets a machine of its own — it keeps going while you're away."),
            ("laptopcomputer", "And on your computers", "Start and follow sessions on any computer running Zeron."),
            ("bell.badge", "Know when it's done", "A notification when an agent finishes or needs you."),
        ].map { symbol, heading, body -> UIView in
            let icon = UIImageView(image: CloudUI.symbol(symbol, 18))
            icon.tintColor = Palette.accent
            icon.contentMode = .center
            icon.translatesAutoresizingMaskIntoConstraints = false
            NSLayoutConstraint.activate([icon.widthAnchor.constraint(equalToConstant: 28)])
            let h = UILabel()
            h.text = heading
            h.font = Fonts.ui(.sansSemibold, 16)
            h.textColor = Palette.text
            let b = UILabel()
            b.text = body
            b.font = Fonts.ui(.sans, 14)
            b.textColor = Palette.secondary
            b.numberOfLines = 0
            let text = UIStackView(arrangedSubviews: [h, b])
            text.axis = .vertical
            text.spacing = 2
            let row = UIStackView(arrangedSubviews: [icon, text])
            row.spacing = 14
            row.alignment = .top
            return row
        }
        let list = UIStackView(arrangedSubviews: features)
        list.axis = .vertical
        list.spacing = 20

        var primary = UIButton.Configuration.filled()
        primary.title = "Get Started"
        primary.baseBackgroundColor = Palette.text
        primary.baseForegroundColor = Palette.background
        primary.cornerStyle = .capsule
        primary.contentInsets = NSDirectionalEdgeInsets(top: 15, leading: 20, bottom: 15, trailing: 20)
        primary.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { a in
            var a = a
            a.font = Fonts.ui(.sansSemibold, 17)
            a.foregroundColor = Palette.background
            return a
        }
        signIn.configuration = primary
        signIn.accessibilityIdentifier = "sign-in"
        signIn.addAction(UIAction { [weak self] _ in self?.startWorkOS() }, for: .touchUpInside)

        status.font = Fonts.ui(.sans, 14)
        status.textColor = Palette.danger
        status.numberOfLines = 0
        status.textAlignment = .center

        let fine = UILabel()
        fine.text = "Sign in or create an account to continue."
        fine.font = Fonts.ui(.sans, 13)
        fine.textColor = Palette.tertiary
        fine.textAlignment = .center

        let top = UIStackView(arrangedSubviews: [mark, title, tagline])
        top.axis = .vertical
        top.alignment = .center
        top.spacing = 12
        top.setCustomSpacing(22, after: mark)
        let buttons = UIStackView(arrangedSubviews: [status, signIn, fine])
        buttons.axis = .vertical
        buttons.spacing = 12
        for v in [top, list, buttons] {
            v.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(v)
        }
        let readable = view.readableContentGuide
        let width = list.widthAnchor.constraint(equalToConstant: 440)
        width.priority = .defaultHigh
        NSLayoutConstraint.activate([
            top.centerXAnchor.constraint(equalTo: view.safeAreaLayoutGuide.centerXAnchor),
            top.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 64),
            top.leadingAnchor.constraint(greaterThanOrEqualTo: readable.leadingAnchor),
            list.topAnchor.constraint(equalTo: top.bottomAnchor, constant: 44),
            list.centerXAnchor.constraint(equalTo: view.safeAreaLayoutGuide.centerXAnchor),
            list.leadingAnchor.constraint(greaterThanOrEqualTo: view.safeAreaLayoutGuide.leadingAnchor, constant: 32),
            width,
            list.bottomAnchor.constraint(lessThanOrEqualTo: buttons.topAnchor, constant: -24),
            buttons.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor, constant: 24),
            buttons.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -24),
            buttons.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -16),
        ])
    }

    private func startWorkOS() {
        let state = UUID().uuidString
        let s = ASWebAuthenticationSession(url: Endpoints.authorizeURL(state: state), callback: .customScheme(Endpoints.callbackScheme)) { [weak self] url, error in
            guard let self else { return }
            if let error = error as? ASWebAuthenticationSessionError, error.code == .canceledLogin { return }
            guard let url, let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems,
                  let code = items.first(where: { $0.name == "code" })?.value,
                  items.first(where: { $0.name == "state" })?.value == state
            else {
                self.status.text = "Sign-in didn't complete. Try again."
                return
            }
            self.signIn.configuration?.showsActivityIndicator = true
            Task { @MainActor in
                do {
                    try await self.app.signIn(code: code) { orgs in await self.pickOrg(orgs) }
                } catch {
                    self.status.text = error.localizedDescription
                }
                self.signIn.configuration?.showsActivityIndicator = false
            }
        }
        s.presentationContextProvider = self
        s.prefersEphemeralWebBrowserSession = false
        session = s
        s.start()
    }

    /// Several organizations: let the user pick (nil = cancelled).
    @MainActor
    private func pickOrg(_ orgs: [AuthOrg]) async -> AuthOrg? {
        await withCheckedContinuation { cont in
            let sheet = UIAlertController(title: "Choose an organization", message: nil, preferredStyle: .actionSheet)
            for org in orgs {
                sheet.addAction(UIAlertAction(title: org.name, style: .default) { _ in cont.resume(returning: org) })
            }
            sheet.addAction(UIAlertAction(title: "Cancel", style: .cancel) { _ in cont.resume(returning: nil) })
            sheet.popoverPresentationController?.sourceView = signIn
            present(sheet, animated: true)
        }
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        view.window ?? ASPresentationAnchor()
    }
}
