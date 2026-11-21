//! The edge and sign-in client `zeron` resolves from its environment.

/// Production edge (Cloudflare Worker + Durable Objects on the zeron.sh zone).
/// `ZERON_EDGE_URL` overrides (local dev / self-hosting).
pub const DEFAULT_EDGE_URL: &str = fork_or("https://edge.zeron.sh", option_env!("ZERON_FORK_EDGE_URL"));

/// Production WorkOS AuthKit client id — public knowledge (it appears in every
/// authorize URL), so baking it in is safe. Overridden by `ZERON_WORKOS_CLIENT_ID`;
/// set it to the empty string — or set a dev bearer via `ZERON_EDGE_TOKEN` — to
/// force dev-mode auth instead.
pub const DEFAULT_WORKOS_CLIENT_ID: &str =
    fork_or("client_01KWD0EAKZKD50YCQJNYSRE4BY", option_env!("ZERON_FORK_CLIENT_ID"));

/// Fork: a self-hosted build bakes its own edge and sign-in client in at
/// compile time (scripts/install-fork-macos.sh), so `zeron` run without any
/// environment never reaches upstream's. Unset or empty keeps upstream's.
const fn fork_or(upstream: &'static str, fork: Option<&'static str>) -> &'static str {
    match fork {
        Some(value) if !value.is_empty() => value,
        _ => upstream,
    }
}

pub fn edge_url_from_env() -> String {
    std::env::var("ZERON_EDGE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_EDGE_URL.into())
}

/// WorkOS client id resolution: explicit env wins (empty string = dev mode);
/// otherwise a `ZERON_EDGE_TOKEN` dev bearer keeps dev mode (smoke tests,
/// local wrangler); otherwise the baked production client id makes optional
/// sync available while a bare start remains local-only.
pub fn workos_client_id_from_env(edge_token: &Option<String>) -> Option<String> {
    match std::env::var("ZERON_WORKOS_CLIENT_ID") {
        Ok(v) if v.trim().is_empty() => None,
        Ok(v) => Some(v),
        Err(_) if edge_token.is_some() => None,
        Err(_) => Some(DEFAULT_WORKOS_CLIENT_ID.into()),
    }
}

/// The engine's IPC port: `ZERON_IPC_PORT`, else 27654.
pub fn ipc_port_from_env() -> u16 {
    ipc_port_from(std::env::var("ZERON_IPC_PORT").ok().as_deref())
}

/// [`ipc_port_from_env`] from the variable's value.
pub fn ipc_port_from(value: Option<&str>) -> u16 {
    value.and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_IPC_PORT)
}

/// The production engine's IPC port.
pub const DEFAULT_IPC_PORT: u16 = 27654;
