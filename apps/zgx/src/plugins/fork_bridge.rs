//! Fork: what our own plugins (apps/zgx/plugins-fork/) may ask of Zeron
//! beyond the plugin API, each plugin its own ops alone:
//!
//!   `plugin.<id>.fork_card { op = "info", session, device? }`
//!       the card's details from the session's own device (the engine's
//!       `ForkSessionCard`, crates/engine/src/rpc/fork_session_card.rs):
//!       `{ here, deviceName?, branch?, base?, additions?, deletions?,
//!       scripts, scriptsError? }` (`here`: on this device)
//!   `plugin.<id>.fork_card { op = "run", session, project, device?, script }`
//!       npm script `script` in a terminal tab of the session's panel, as a
//!       run action opens one (ui/screens/panel/fork_terminal.luau): the
//!       engine resolves the name again and runs it in the chat's checkout.
//!   `plugin.<id>.fork_card { op = "providers", device? }` (the usage cards)
//!       the agents switched on and installed on that device (this one
//!       without it), as Settings → Providers lists them: `{ id }` (the
//!       engine's `ListHarnesses`).
//!
//! Kept in its own file so plugins/mod.rs carries only the hooks.

use serde_json::json;
use zgx_script::{ScriptHost, Value};

use super::{Bridge, grants, json_value, value_json};

/// The plugins this bridge serves, by id, and the ops each may ask.
const ALLOWED: &[(&str, &[&str])] = &[
    ("clemann-session-card", &["info", "run"]),
    ("clemann-usage-cards", &["providers"]),
];

fn ops(id: &str) -> Option<&'static [&'static str]> {
    ALLOWED.iter().find(|(plugin, _)| *plugin == id).map(|(_, ops)| *ops)
}

fn allowed(id: &str) -> bool {
    ops(id).is_some()
}

/// The action open to plugin `id`'s sandbox (`expose_own`'s part).
pub(super) fn expose_own(host: &ScriptHost, id: &str) {
    if allowed(id) {
        host.expose_action(format!("plugin.{id}.fork_card"), Some(grants::own(id)));
    }
}

impl Bridge {
    /// The action itself (`expose`'s part), for an allowed plugin.
    pub(super) fn expose_fork(&self, id: &str) {
        if !allowed(id) {
            return;
        }
        let me = std::rc::Rc::downgrade(&self.me());
        let plugin = id.to_string();
        self.host.async_action(format!("plugin.{id}.fork_card"), move |asked: Value| {
            let (me, plugin) = (me.clone(), plugin.clone());
            async move {
                let asked = value_json(&asked);
                let field = |key: &str| asked[key].as_str().map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned);
                let op = asked["op"].as_str().unwrap_or_default().to_owned();
                if !ops(&plugin).is_some_and(|ops| ops.contains(&op.as_str())) {
                    return Err(format!("unknown op {op}"));
                }
                if op == "providers" {
                    let mut params = json!({});
                    if let Some(device) = crate::files::route_to(field("device")).await {
                        params["targetDeviceId"] = json!(device);
                    }
                    let list = crate::engine::get().call("ListHarnesses", params).await?;
                    let on: Vec<_> = list
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|h| h["enabled"].as_bool().unwrap_or(true) && h["installed"].as_bool().unwrap_or(true))
                        .filter_map(|h| h["id"].as_str().map(|id| json!({ "id": id })))
                        .collect();
                    return Ok(json_value(&json!(on)));
                }
                let session = field("session").ok_or("no session")?;
                let device = crate::files::route_to(field("device")).await;
                match op.as_str() {
                    "info" => {
                        let mut params = json!({ "chatId": session });
                        let here = device.is_none();
                        if let Some(device) = device {
                            params["targetDeviceId"] = json!(device);
                        }
                        let mut card = crate::engine::get().call("ForkSessionCard", params).await?;
                        card["here"] = json!(here);
                        Ok(json_value(&card))
                    }
                    "run" => {
                        let project = field("project").ok_or("no project")?;
                        let script = field("script").ok_or("no script")?;
                        if script.chars().any(char::is_control) || script.len() > 200 {
                            return Err("not a script name".into());
                        }
                        let b = me.upgrade().ok_or("Zeron is closing")?;
                        let name = b.item(&plugin).map(|i| i.name().to_string()).unwrap_or_default();
                        b.request(
                            "fork_terminal",
                            &name,
                            json_value(&json!({
                                "session": session,
                                "project": project,
                                "device": field("device"),
                                "title": format!("npm run {script}"),
                                "action": format!("npm:{script}"),
                            })),
                        );
                        Ok(Value::from(true))
                    }
                    other => Err(format!("unknown op {other}")),
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_our_plugins_are_served_and_only_their_ops() {
        assert!(super::allowed("clemann-session-card"));
        assert!(super::allowed("clemann-usage-cards"));
        assert!(!super::allowed("github"));
        assert!(!super::allowed("clemann-session-card-x"));
        assert_eq!(super::ops("clemann-usage-cards"), Some(&["providers"][..]));
        assert!(!super::ops("clemann-usage-cards").unwrap().contains(&"run"));
    }
}
