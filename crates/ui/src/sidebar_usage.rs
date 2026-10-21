//! Plan-usage cards at the foot of the chat sidebar: one per enabled provider
//! that reports usage, showing the live account's meters. The meters are the
//! composer ring popover's (`render_usage_meter`) and the data is the same
//! [`AccountsSnapshotCache`], so a switch or probe anywhere shows up here.
use std::time::{Duration, Instant};

use gpui::{Context, Entity, IntoElement, Render, Subscription, Task, Window, div, prelude::*, px};
use zeron_engine::registry::HarnessDescriptor;
use zeron_proto::{AgentAccount, AgentAccountsSnapshot, HarnessId};
use zeron_rpc::methods;

use crate::icons::icon;
use crate::pickers::{HarnessCatalogChanged, harness_brand_icon, offered_harnesses};
use crate::settings::accounts::{
    self, AccountsSnapshotCache, provider_name, render_usage_meter, reports_usage, signs_in,
};
use crate::settings::widgets;
use crate::state::AppState;
use crate::theme::Theme;

/// Forced probes hit the provider; the engine throttles them too, but don't
/// even ask more often than this.
const FORCE_MIN_INTERVAL: Duration = Duration::from_secs(30);
/// Background re-probe: usage moves as turns run.
const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Windows per card — the composer popover's cap, so a card stays compact.
const MAX_METERS: usize = 2;

/// The providers to show cards for, in Settings order: offered on this device
/// (enabled and installed) and able to report usage. Pure.
pub fn usage_providers(catalog: &[HarnessDescriptor]) -> Vec<HarnessId> {
    let offered: Vec<HarnessId> = offered_harnesses(catalog).iter().map(|d| d.id).collect();
    accounts::PROVIDERS
        .iter()
        .map(|(harness, _, _)| *harness)
        .filter(|harness| {
            offered.contains(harness) && signs_in(*harness) && reports_usage(*harness)
        })
        .collect()
}

/// The accounts to draw a card for: each provider's live account, when it has
/// usage windows to show. Pure.
pub fn card_accounts<'a>(
    snapshot: &'a AgentAccountsSnapshot,
    providers: &[HarnessId],
) -> Vec<&'a AgentAccount> {
    providers
        .iter()
        .filter_map(|harness| crate::account_usage::active_account(snapshot, *harness))
        .filter(|account| !account.usage_windows.is_empty())
        .collect()
}

/// Loads this device's harness catalog and accounts, and keeps them fresh.
pub struct SidebarUsage {
    state: Entity<AppState>,
    /// `None` until the catalog has loaded.
    providers: Option<Vec<HarnessId>>,
    requested: bool,
    last_forced: Option<Instant>,
    catalog_task: Option<Task<()>>,
    usage_task: Option<Task<()>>,
    _poll: Task<()>,
    _cache: Subscription,
    _catalog: Subscription,
    _state: Subscription,
}

impl SidebarUsage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this.update(cx, |usage, cx| usage.load_usage(cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            _state: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            providers: None,
            requested: false,
            last_forced: None,
            catalog_task: None,
            usage_task: None,
            _poll: poll,
            // Settings → Accounts and the composer ring write the same cache.
            _cache: cx.observe_global::<AccountsSnapshotCache>(|_, cx| cx.notify()),
            // A Settings → Providers toggle changes which cards belong.
            _catalog: cx.observe_global::<HarnessCatalogChanged>(|usage, cx| {
                usage.load_catalog(cx);
            }),
        }
    }

    /// First load, once the engine is up. Cheap enough to call every render.
    fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if self.requested || self.state.read(cx).engine().is_none() {
            return;
        }
        self.requested = true;
        self.load_catalog(cx);
        self.load_usage(cx);
    }

    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.catalog_task = Some(cx.spawn(async move |this, cx| {
            let list = engine
                .client()
                .call(methods::LIST_HARNESSES, serde_json::json!({}))
                .await
                .ok()
                .and_then(|value| serde_json::from_value::<Vec<HarnessDescriptor>>(value).ok());
            if let Some(list) = list {
                this.update(cx, |usage, cx| {
                    usage.providers = Some(usage_providers(&list));
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// The engine's persisted usage paints at once when nothing is cached;
    /// the forced probe then replaces it with live numbers.
    fn load_usage(&mut self, cx: &mut Context<Self>) {
        if self
            .last_forced
            .is_some_and(|at| at.elapsed() < FORCE_MIN_INTERVAL)
        {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        self.last_forced = Some(Instant::now());
        let paint_first = cx
            .try_global::<AccountsSnapshotCache>()
            .is_none_or(|cache| !cache.0.contains_key(&None));
        self.usage_task = Some(cx.spawn(async move |this, cx| {
            let fetch = |force_usage: bool| {
                let engine = engine.clone();
                async move {
                    engine
                        .client()
                        .call(
                            methods::LIST_AGENT_ACCOUNTS,
                            serde_json::json!({ "forceUsage": force_usage }),
                        )
                        .await
                        .ok()
                        .and_then(|value| {
                            serde_json::from_value::<AgentAccountsSnapshot>(value).ok()
                        })
                }
            };
            for force_usage in [false, true] {
                if !force_usage && !paint_first {
                    continue;
                }
                if let Some(snapshot) = fetch(force_usage).await {
                    this.update(cx, |_, cx| {
                        cx.default_global::<AccountsSnapshotCache>()
                            .0
                            .insert(None, snapshot);
                    })
                    .ok();
                }
            }
        }));
    }

    fn card(
        account: &AgentAccount,
        theme: &Theme,
        now: chrono::DateTime<chrono::Utc>,
    ) -> gpui::Stateful<gpui::Div> {
        let (mark, tint) = harness_brand_icon(account.harness);
        let windows = &account.usage_windows;
        let mut title = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .child(
                icon(mark)
                    .size(px(13.0))
                    .text_color(tint.unwrap_or(theme.text_muted)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(11.5))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(provider_name(account.harness)),
            );
        if let Some(plan) = account.plan_label.clone() {
            title = title.child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .child(plan),
            );
        }
        div()
            .id(("sidebar-usage-card", account.harness as usize))
            .px(px(Theme::SPACE_SM))
            .py(px(6.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .bg(theme.wash(0.05))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .tooltip(widgets::text_tooltip(accounts::usage_resets(windows, now)))
            .child(title)
            .children(
                windows
                    .iter()
                    .take(MAX_METERS)
                    .map(|window| render_usage_meter(window, theme)),
            )
    }
}

impl Render for SidebarUsage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_loaded(cx);
        let theme = Theme::of(cx).clone();
        let now = chrono::Utc::now();
        let cards: Vec<gpui::Stateful<gpui::Div>> = match (
            &self.providers,
            cx.try_global::<AccountsSnapshotCache>()
                .and_then(|cache| cache.0.get(&None)),
        ) {
            (Some(providers), Some(snapshot)) => card_accounts(snapshot, providers)
                .into_iter()
                .map(|account| Self::card(account, &theme, now))
                .collect(),
            _ => Vec::new(),
        };
        // Nothing to show takes no room: the padding rides with the cards.
        div()
            .id("sidebar-usage")
            .debug_selector(|| "sidebar-usage".into())
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .when(!cards.is_empty(), |el| {
                el.px(px(Theme::SPACE_SM)).pt(px(Theme::SPACE_SM))
            })
            .children(cards)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(id: HarnessId, installed: bool, enabled: Option<bool>) -> HarnessDescriptor {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": "x",
            "supportsSteering": false,
            "steeringMode": "turn-boundary",
            "reasoningLevels": [],
            "installed": installed,
            "enabled": enabled,
        }))
        .unwrap()
    }

    fn account(harness: HarnessId, active: bool, windows: usize) -> AgentAccount {
        serde_json::from_value(serde_json::json!({
            "id": format!("{harness:?}-{active}"),
            "harness": harness,
            "email": null,
            "planLabel": null,
            "active": active,
            "switchable": true,
            "usageWindows": (0..windows)
                .map(|_| serde_json::json!({ "label": "5h", "usedFraction": 0.4, "resetsAt": null }))
                .collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn providers_are_enabled_installed_and_report_usage() {
        let catalog = vec![
            descriptor(HarnessId::Codex, true, Some(true)),
            descriptor(HarnessId::ClaudeCode, true, Some(true)),
            descriptor(HarnessId::Cursor, true, Some(false)),
            descriptor(HarnessId::Grok, false, Some(true)),
            // Enabled and installed, but reports no plan usage.
            descriptor(HarnessId::Antigravity, true, Some(true)),
        ];
        // Settings order, not catalog order.
        assert_eq!(
            usage_providers(&catalog),
            vec![HarnessId::ClaudeCode, HarnessId::Codex]
        );
    }

    #[test]
    fn cards_show_live_accounts_that_have_usage() {
        let snapshot = AgentAccountsSnapshot {
            accounts: vec![
                account(HarnessId::ClaudeCode, true, 2),
                account(HarnessId::Codex, false, 2),
                account(HarnessId::Codex, true, 0),
                account(HarnessId::Cursor, true, 1),
            ],
            warnings: Vec::new(),
        };
        let providers = [HarnessId::ClaudeCode, HarnessId::Codex];
        let cards = card_accounts(&snapshot, &providers);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].harness, HarnessId::ClaudeCode);
    }
}
