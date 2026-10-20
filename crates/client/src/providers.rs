//! A computer's agents from the phone: which harnesses it offers and the CLI
//! logins each one runs on — what desktop Settings → Providers shows for a
//! device picked in its switcher. Every call goes over the device relay to
//! that computer's engine, which owns its own CLI logins.
//!
//! Signing a computer's agent in needs a browser on a loopback that reaches
//! the computer (the desktop tunnels it); the phone lists, switches and
//! removes, and leaves new logins to Zeron on that computer.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use zeron_proto::{AgentAccount, AgentAccountsSnapshot, AgentAuthKind, HarnessId};

use crate::Client;
use crate::catalog::HarnessInfo;
use crate::client::Backend;
use crate::error::{ClientError, Result};
use crate::lock;

impl Client {
    /// `SetHarnessEnabled` on `device_id`: offer `harness` in its pickers or
    /// not. Replies with the device's fresh catalog.
    pub async fn set_harness_enabled(
        &self,
        device_id: &str,
        harness: &str,
        enabled: bool,
    ) -> Result<Vec<HarnessInfo>> {
        let list = match self.inner.backend() {
            Backend::Demo(demo) => {
                lock(&DEMO_DISABLED).insert(harness.to_owned(), !enabled);
                demo.list_harnesses(device_id).await
            }
            Backend::Live(live) => {
                let reply = live
                    .relay
                    .call(
                        device_id,
                        zeron_rpc::methods::SET_HARNESS_ENABLED,
                        serde_json::json!({ "harness": harness, "enabled": enabled }),
                    )
                    .await?;
                let list: Vec<HarnessInfo> = serde_json::from_value(reply)
                    .map_err(|e| ClientError::HostError(e.to_string()))?;
                list.into_iter().filter(|h| h.id != "mock").collect()
            }
        };
        let list = with_demo_toggles(self.inner.is_demo(), list);
        lock(&self.inner.harness_catalogs).insert(device_id.to_owned(), list.clone());
        self.inner.recompute_workspace();
        Ok(list)
    }

    /// `ListAgentAccounts` on `device_id`: its CLI logins per agent.
    pub async fn device_agent_accounts(&self, device_id: &str) -> Result<AgentAccountsSnapshot> {
        self.accounts_call(
            device_id,
            zeron_rpc::methods::LIST_AGENT_ACCOUNTS,
            serde_json::json!({}),
        )
        .await
    }

    /// `ActivateAgentAccount`: the login `harness` runs on from now on.
    pub async fn device_activate_account(
        &self,
        device_id: &str,
        harness: &str,
        account_id: &str,
    ) -> Result<AgentAccountsSnapshot> {
        if self.inner.is_demo() {
            lock(&DEMO_ACTIVE).insert(harness.to_owned(), account_id.to_owned());
        }
        self.accounts_call(
            device_id,
            zeron_rpc::methods::ACTIVATE_AGENT_ACCOUNT,
            serde_json::json!({ "harness": harness, "accountId": account_id }),
        )
        .await
    }

    /// `ForgetAgentAccount`: drop a saved login from that computer.
    pub async fn device_forget_account(
        &self,
        device_id: &str,
        harness: &str,
        account_id: &str,
    ) -> Result<AgentAccountsSnapshot> {
        if self.inner.is_demo() {
            lock(&DEMO_FORGOTTEN).push(account_id.to_owned());
        }
        self.accounts_call(
            device_id,
            zeron_rpc::methods::FORGET_AGENT_ACCOUNT,
            serde_json::json!({ "harness": harness, "accountId": account_id }),
        )
        .await
    }

    async fn accounts_call(
        &self,
        device_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<AgentAccountsSnapshot> {
        match self.inner.backend() {
            Backend::Demo(_) => Ok(demo_accounts()),
            Backend::Live(live) => {
                let reply = live.relay.call(device_id, method, params).await?;
                serde_json::from_value(reply).map_err(|e| ClientError::HostError(e.to_string()))
            }
        }
    }
}

/// Demo mode's toggles and switches, so the screens respond offline.
static DEMO_DISABLED: LazyLock<Mutex<HashMap<String, bool>>> = LazyLock::new(Default::default);
static DEMO_ACTIVE: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
static DEMO_FORGOTTEN: LazyLock<Mutex<Vec<String>>> = LazyLock::new(Default::default);

/// The demo catalog with the toggles applied.
pub(crate) fn with_demo_toggles(demo: bool, mut list: Vec<HarnessInfo>) -> Vec<HarnessInfo> {
    if demo {
        let disabled = lock(&DEMO_DISABLED);
        for harness in &mut list {
            if disabled.get(&harness.id).copied().unwrap_or(false) {
                harness.enabled = Some(false);
            }
        }
    }
    list
}

fn demo_accounts() -> AgentAccountsSnapshot {
    let account = |id: &str, harness: HarnessId, email: &str, plan: &str| AgentAccount {
        id: id.into(),
        harness,
        email: Some(email.into()),
        plan_label: Some(plan.into()),
        active: false,
        usage_windows: Vec::new(),
        usage_fetched_at: None,
        usage_error: None,
        display_name: None,
        organization: None,
        auth_kind: Some(AgentAuthKind::Oauth),
        switchable: true,
        saved_at: None,
        provider: None,
    };
    let mut accounts = vec![
        account(
            "demo-claude-1",
            HarnessId::ClaudeCode,
            "you@acme.dev",
            "Max",
        ),
        account(
            "demo-claude-2",
            HarnessId::ClaudeCode,
            "you@personal.dev",
            "Pro",
        ),
        account("demo-codex-1", HarnessId::Codex, "you@acme.dev", "Plus"),
    ];
    let forgotten = lock(&DEMO_FORGOTTEN);
    accounts.retain(|a| !forgotten.contains(&a.id));
    let active = lock(&DEMO_ACTIVE);
    for harness in [HarnessId::ClaudeCode, HarnessId::Codex] {
        let key = serde_json::to_value(harness)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let chosen = active.get(&key).cloned();
        let mut picked = false;
        for a in accounts.iter_mut().filter(|a| a.harness == harness) {
            a.active = match &chosen {
                Some(id) => *id == a.id,
                None => !picked,
            };
            picked |= a.active;
        }
    }
    AgentAccountsSnapshot {
        accounts,
        warnings: Vec::new(),
    }
}
