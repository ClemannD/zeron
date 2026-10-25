//! Private fork policy. Upstream builds retain the anonymous release feed.
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{Context as _, bail};

pub const RELEASES_URL: &str = "https://zeron.clemann.app/fork-updates";
pub const TEAM_ID: &str = "7C4DAG233S";

/// Only release builds made by our packager enable this channel. Environment
/// variables alone cannot turn an upstream build into a private fork release.
pub fn enabled() -> bool {
    option_env!("ZERON_FORK_BUILD").is_some_and(|build| !build.is_empty())
}

pub fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(
        || match option_env!("ZERON_FORK_BUILD").filter(|b| !b.is_empty()) {
            Some(build) => format!("{}.{build}", env!("CARGO_PKG_VERSION")),
            None => env!("CARGO_PKG_VERSION").to_owned(),
        },
    )
}

pub type TokenFuture = Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>;
/// `true` requests one refresh after a 401. The engine remains the single
/// owner of rotating refresh credentials; UI processes use its local IPC.
pub type TokenProvider = Arc<dyn Fn(bool) -> TokenFuture + Send + Sync>;
static TOKENS: RwLock<Option<TokenProvider>> = RwLock::new(None);

pub fn set_token_provider(provider: TokenProvider) {
    *TOKENS.write().unwrap_or_else(|e| e.into_inner()) = Some(provider);
}

pub fn validate_url(url: &str) -> anyhow::Result<()> {
    let url = reqwest::Url::parse(url)?;
    let base = reqwest::Url::parse(RELEASES_URL)?;
    anyhow::ensure!(
        url.origin() == base.origin()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && (url.path() == base.path() || url.path().starts_with("/fork-updates/")),
        "private updates must use the fork's own authenticated release service"
    );
    Ok(())
}

pub async fn send(client: &reqwest::Client, url: &str) -> anyhow::Result<reqwest::Response> {
    if !enabled() {
        return client
            .get(url)
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .await
            .context("requesting release");
    }
    validate_url(url)?;
    let provider = TOKENS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .context("Sign in to Zeron to check for private updates")?;
    send_authorized(client, url, provider).await
}

async fn send_authorized(
    client: &reqwest::Client,
    url: &str,
    provider: TokenProvider,
) -> anyhow::Result<reqwest::Response> {
    for refresh in [false, true] {
        let token = provider(refresh)
            .await
            .context("Sign in to Zeron to download private updates")?;
        let response = client
            .get(url)
            .bearer_auth(token)
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .await?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED || refresh {
            return Ok(response);
        }
    }
    unreachable!()
}

pub fn verify_bundle(bundle: &std::path::Path) -> anyhow::Result<()> {
    if !enabled() {
        return Ok(());
    }
    let engine = bundle.join("Contents/MacOS/Zeron Engine");
    anyhow::ensure!(
        engine.is_file(),
        "fork update is missing its background engine"
    );
    for (path, identifier) in [
        (engine.as_path(), "sh.zeron.app.engine"),
        (bundle, "sh.zeron.app"),
    ] {
        let requirement = format!(
            "=anchor apple generic and identifier \"{identifier}\" and certificate leaf[subject.OU] = \"{TEAM_ID}\" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
        );
        super::run(
            "/usr/bin/codesign",
            &[
                "--verify",
                "--deep",
                "--strict",
                "-R",
                &requirement,
                &path.to_string_lossy(),
            ],
        )
        .context("update must be signed by our Developer ID")?;
    }
    super::run(
        "/usr/sbin/spctl",
        &["--assess", "--type", "execute", &bundle.to_string_lossy()],
    )
    .context("update must pass Apple's notarization assessment")
}

pub fn validate_manifest(manifest: &super::Manifest) -> anyhow::Result<()> {
    if !enabled() {
        return Ok(());
    }
    validate_private_manifest(manifest)
}

fn validate_private_manifest(manifest: &super::Manifest) -> anyhow::Result<()> {
    let file = super::mac_app_artifact(&manifest.version);
    let hash = manifest
        .files
        .get(&file)
        .and_then(|meta| meta.sha256.as_deref());
    if !hash.is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())) {
        bail!("private release manifest must contain the app's SHA-256 checksum");
    }
    anyhow::ensure!(
        super::parse_version(&manifest.version).is_some(),
        "invalid private release version"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Release acceptance test against our real Worker/R2 and Apple-signed
    /// packages. Installs only into a temporary directory, never Applications.
    #[tokio::test]
    #[ignore = "requires a published newer release, owner token, and signed bootstrap app"]
    async fn private_feed_live_stage_verify_swap_and_backup() {
        assert!(
            enabled(),
            "compile with ZERON_FORK_BUILD set to the bootstrap build number"
        );
        let token = std::env::var("ZERON_UPDATE_TEST_TOKEN").expect("owner access token");
        let bootstrap = std::path::PathBuf::from(
            std::env::var("ZERON_UPDATE_TEST_APP").expect("signed bootstrap app"),
        );
        set_token_provider(Arc::new(move |_| {
            let token = token.clone();
            Box::pin(async move { Ok(token) })
        }));
        let manifest = crate::fetch_latest("https://zeron.clemann.app")
            .await
            .unwrap();
        assert!(crate::version_newer(
            &manifest.version,
            crate::current_version()
        ));
        let temp = tempfile::tempdir().unwrap();
        let installed = temp.path().join("Applications/Zeron.app");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        crate::run(
            "/usr/bin/ditto",
            &[&bootstrap.to_string_lossy(), &installed.to_string_lossy()],
        )
        .unwrap();
        verify_bundle(&installed).unwrap();
        let before = crate::installed_version(&crate::InstallKind::MacApp {
            bundle: installed.clone(),
        })
        .unwrap();

        let mut damaged = manifest.clone();
        damaged
            .files
            .get_mut(&crate::mac_app_artifact(&manifest.version))
            .unwrap()
            .sha256 = Some("0".repeat(64));
        let failed = crate::stage_mac_app(
            "https://zeron.clemann.app",
            &damaged,
            &temp.path().join("bad-download"),
        )
        .await;
        assert!(
            failed
                .unwrap_err()
                .to_string()
                .contains("checksum mismatch")
        );
        assert_eq!(
            crate::installed_version(&crate::InstallKind::MacApp {
                bundle: installed.clone()
            })
            .as_deref(),
            Some(before.as_str())
        );

        let staged = crate::stage_mac_app(
            "https://zeron.clemann.app",
            &manifest,
            &temp.path().join("data"),
        )
        .await
        .unwrap();
        verify_bundle(&staged).unwrap();
        crate::apply_mac_app(&staged, &installed).unwrap();
        assert_eq!(
            crate::installed_version(&crate::InstallKind::MacApp {
                bundle: installed.clone()
            })
            .as_deref(),
            Some(manifest.version.as_str())
        );
        let previous = installed.parent().unwrap().join(".Zeron.app.previous");
        assert_eq!(
            crate::installed_version(&crate::InstallKind::MacApp { bundle: previous }).as_deref(),
            Some(before.as_str())
        );
    }
    #[test]
    fn credentials_are_confined_to_our_release_origin_and_path() {
        assert!(validate_url(RELEASES_URL).is_ok());
        assert!(validate_url(&format!("{RELEASES_URL}/manifest.json")).is_ok());
        for url in [
            "https://evil.test/fork-updates/manifest.json",
            "http://zeron.clemann.app/fork-updates/a",
            "https://zeron.clemann.app/releases/a",
            "https://zeron.clemann.app/fork-updates-other/a",
            "https://user@zeron.clemann.app/fork-updates/a",
            "https://zeron.clemann.app/fork-updates/a?token=x",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn private_manifests_require_the_correct_artifact_and_a_real_checksum() {
        let mut manifest = crate::Manifest {
            version: "0.2.124.7".into(),
            ..Default::default()
        };
        assert!(validate_private_manifest(&manifest).is_err());
        let file = crate::mac_app_artifact(&manifest.version);
        manifest.files.insert(
            file.clone(),
            crate::FileMeta {
                sha256: Some("g".repeat(64)),
            },
        );
        assert!(validate_private_manifest(&manifest).is_err());
        manifest.files.get_mut(&file).unwrap().sha256 = Some("a".repeat(64));
        assert!(validate_private_manifest(&manifest).is_ok());
        assert!(crate::version_newer("0.2.124.8", "0.2.124.7"));
        assert!(crate::version_newer("0.2.125.1", "0.2.124.8"));
    }

    #[tokio::test]
    async fn expired_bearer_gets_exactly_one_refresh_without_following_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/manifest.json", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for expected in ["Bearer stale", "Bearer fresh"] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut chunk = [0; 512];
                    let length = socket.read(&mut chunk).await.unwrap();
                    assert!(length > 0 && bytes.len() < 4096);
                    bytes.extend_from_slice(&chunk[..length]);
                }
                let request = String::from_utf8_lossy(&bytes);
                assert!(request.contains(expected));
                let response = if expected.ends_with("stale") {
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = calls.clone();
        let provider: TokenProvider = Arc::new(move |refresh| {
            record.lock().unwrap().push(refresh);
            Box::pin(async move { Ok(if refresh { "fresh" } else { "stale" }.into()) })
        });
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let response = send_authorized(&client, &url, provider).await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        assert_eq!(*calls.lock().unwrap(), vec![false, true]);
        server.await.unwrap();
    }
}
