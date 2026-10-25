//! `zeron update` — check for and apply a newer release, natively (the same
//! flow `edge/src/install.sh` performs: download → verify → symlink swap →
//! service restart). macOS app bundles swap the bundle instead; source builds
//! are report-only.

use anyhow::bail;
use zeron_update::{InstallKind, current_version, version_newer};

/// `--check` prints the verdict and exits (nonzero when an update is available,
/// so scripts can gate on it).
pub async fn update(edge_url: &str, check_only: bool) -> anyhow::Result<()> {
    if zeron_update::fork::enabled() {
        let config = super::engine_config_from_env();
        let token = zeron_rpc::ipc_auth::client_tokens(Some(&config.data_dir))
            .into_iter()
            .next()
            .ok_or_else(|| {
                anyhow::anyhow!("Open Zeron and sign in before checking for private updates")
            })?;
        let client =
            zeron_rpc::connect_ws(&format!("ws://127.0.0.1:{}", config.ipc_port), Some(&token))
                .await?;
        let client = std::sync::Arc::new(client);
        zeron_update::fork::set_token_provider(std::sync::Arc::new(move |refresh| {
            let client = client.clone();
            Box::pin(async move {
                let value = client
                    .call(
                        zeron_rpc::methods::RELEASE_ACCESS_TOKEN,
                        serde_json::json!({"refresh": refresh}),
                    )
                    .await?;
                value["accessToken"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("Sign in to Zeron to download private updates"))
            })
        }));
    }
    let manifest = zeron_update::fetch_latest(edge_url).await?;
    let current = current_version();
    if !version_newer(&manifest.version, current) {
        println!(
            "zeron {current} is up to date (latest: {}).",
            manifest.version
        );
        return Ok(());
    }
    println!("zeron {current} → {} available", manifest.version);
    if check_only {
        std::process::exit(1);
    }

    let install = zeron_update::detect_install();
    if let Some(blocker) = install.desktop_update_blocker() {
        bail!("{blocker}");
    }
    match install {
        InstallKind::Managed { app_root } => {
            println!(
                "downloading {}…",
                zeron_update::headless_artifact(&manifest.version)
            );
            zeron_update::stage_headless(edge_url, &manifest, &app_root).await?;
            zeron_update::apply_headless(&app_root, &manifest.version)?;
            println!(
                "installed {} (current → {})",
                app_root.join(&manifest.version).display(),
                manifest.version
            );
            match zeron_update::restart_service() {
                Ok(()) => println!("engine service restarted."),
                Err(err) => println!(
                    "note: service restart failed ({err:#}) — restart the engine manually to finish."
                ),
            }
            Ok(())
        }
        InstallKind::MacApp { bundle } => {
            println!(
                "downloading {}…",
                zeron_update::mac_app_artifact(&manifest.version)
            );
            let data_dir = super::paths::data_dir();
            let staged = zeron_update::stage_mac_app(edge_url, &manifest, &data_dir).await?;
            zeron_update::apply_mac_app(&staged, &bundle)?;
            println!("updated {} — relaunch Zeron to finish.", bundle.display());
            Ok(())
        }
        #[cfg(windows)]
        InstallKind::WindowsPortable { directory } => {
            let staged = zeron_update::windows::stage(edge_url, &manifest, &directory).await?;
            zeron_update::windows::apply(&staged, &directory, false)?;
            println!(
                "updated to {} — relaunch Zeron to finish.",
                manifest.version
            );
            Ok(())
        }
        InstallKind::Unmanaged => {
            bail!(
                "this binary is not update-managed (source build or hand-copied).\n\
                 Linux: curl -fsSL https://zeron.sh/install.sh | sh, or run install.sh from the release tarball\n\
                 macOS: download the new Zeron.app dmg, or rebuild from source.\n\
                 Windows: install with the Zeron setup .exe from {}, or rebuild from source.",
                zeron_update::LATEST_RELEASE_PAGE
            )
        }
    }
}
