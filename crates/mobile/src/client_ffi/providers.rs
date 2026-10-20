//! A computer's agents over FFI: which ones it offers and the CLI logins
//! each runs on (`zeron_client::providers`) — the phone's Settings →
//! Providers for a device picked in its switcher.

use super::{CoreClient, CoreResult, HarnessInfo, on_runtime};

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DeviceAccountInfo {
    pub id: String,
    /// `claude-code` · `codex` · …
    pub harness: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub plan_label: Option<String>,
    pub active: bool,
    /// Can be switched to / removed (false for logins Zeron can't read).
    pub switchable: bool,
    /// An API key rather than an account.
    pub api_key: bool,
}

impl From<zeron_proto::AgentAccount> for DeviceAccountInfo {
    fn from(a: zeron_proto::AgentAccount) -> Self {
        Self {
            id: a.id,
            harness: serde_json::to_value(a.harness)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            email: a.email,
            display_name: a.display_name,
            plan_label: a.plan_label,
            active: a.active,
            switchable: a.switchable,
            api_key: a.auth_kind == Some(zeron_proto::AgentAuthKind::ApiKey),
        }
    }
}

fn accounts(snapshot: zeron_proto::AgentAccountsSnapshot) -> Vec<DeviceAccountInfo> {
    snapshot.accounts.into_iter().map(Into::into).collect()
}

#[uniffi::export]
impl CoreClient {
    /// Offer `harness` on `device_id` or not; the device's fresh catalog.
    pub async fn set_harness_enabled(
        &self,
        device_id: String,
        harness: String,
        enabled: bool,
    ) -> CoreResult<Vec<HarnessInfo>> {
        let client = self.client.clone();
        on_runtime(async move {
            client
                .set_harness_enabled(&device_id, &harness, enabled)
                .await
        })
        .await
        .map(|list| list.into_iter().map(Into::into).collect())
    }

    /// The CLI logins on `device_id`, every agent's.
    pub async fn device_agent_accounts(
        &self,
        device_id: String,
    ) -> CoreResult<Vec<DeviceAccountInfo>> {
        let client = self.client.clone();
        on_runtime(async move { client.device_agent_accounts(&device_id).await })
            .await
            .map(accounts)
    }

    /// Run `harness` on `account_id` from now on.
    pub async fn device_activate_account(
        &self,
        device_id: String,
        harness: String,
        account_id: String,
    ) -> CoreResult<Vec<DeviceAccountInfo>> {
        let client = self.client.clone();
        on_runtime(async move {
            client
                .device_activate_account(&device_id, &harness, &account_id)
                .await
        })
        .await
        .map(accounts)
    }

    /// Remove a saved login from `device_id`.
    pub async fn device_forget_account(
        &self,
        device_id: String,
        harness: String,
        account_id: String,
    ) -> CoreResult<Vec<DeviceAccountInfo>> {
        let client = self.client.clone();
        on_runtime(async move {
            client
                .device_forget_account(&device_id, &harness, &account_id)
                .await
        })
        .await
        .map(accounts)
    }
}
