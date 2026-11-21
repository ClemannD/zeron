//! A subscription's plan usage as meters: the replies of Claude's
//! `/api/oauth/usage` and Codex's `/wham/usage` read into windows
//! ("Session", "Week", "Month") and the live plan label. Shared by every
//! reader: the engine probing a CLI login, and Cloud's logins, whose usage
//! only the vault can fetch (the desktop and the phone read its reply). Pure.

use chrono::{DateTime, Utc};

use crate::{AgentUsageSpend, AgentUsageWindow, HarnessId};

/// `harness`'s usage reply as windows and the live plan label; `None` when
/// it carries no window.
pub fn from_reply(
    harness: HarnessId,
    body: &serde_json::Value,
) -> Option<(Vec<AgentUsageWindow>, Option<String>)> {
    match harness {
        HarnessId::ClaudeCode => claude(body).map(|windows| (windows, None)),
        HarnessId::Codex => codex(body),
        HarnessId::Antigravity => antigravity(body),
        _ => None,
    }
}

/// Antigravity's usage: Google's `retrieveUserQuotaSummary` (each group of
/// models with its own limits, each limit a `remainingFraction` with a
/// `resetTime`), with the `paidTier` and `currentTier` of `loadCodeAssist`
/// beside its `groups`. One window per limit, named by its group and span
/// ("Gemini Session", "Claude & GPT Week"); the plan is the paid tier's name
/// ("Google AI Ultra"). `None` with no `groups` at all; none in it is a tier
/// with no limits, and no windows.
pub fn antigravity(body: &serde_json::Value) -> Option<(Vec<AgentUsageWindow>, Option<String>)> {
    let groups = body.get("groups")?.as_array()?;
    let mut ranked = Vec::new();
    for group in groups {
        let name = group
            .get("displayName")
            .or_else(|| group.get("display_name"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let (group_rank, group_label) = antigravity_group(name);
        for bucket in group.get("buckets").and_then(|b| b.as_array()).into_iter().flatten() {
            if bucket.get("disabled").and_then(|d| d.as_bool()) == Some(true) {
                continue;
            }
            // (A count with no fraction can't be a meter.)
            let Some(remaining) = bucket
                .get("remainingFraction")
                .or_else(|| bucket.get("remaining_fraction"))
                .and_then(|v| v.as_f64())
            else {
                continue;
            };
            let (span_rank, span) = antigravity_span(bucket);
            ranked.push((
                (group_rank, group_label.clone(), span_rank),
                AgentUsageWindow {
                    label: format!("{group_label} {span}"),
                    used_fraction: (1.0 - remaining).clamp(0.0, 1.0) as f32,
                    resets_at: parse_when(bucket.get("resetTime").or_else(|| bucket.get("reset_time"))),
                    spend: None,
                },
            ));
        }
    }
    // Gemini's first, each group's shortest span first: the card's session
    // and week are its first window and its weekly one.
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    let plan = ["paidTier", "currentTier"]
        .iter()
        .filter_map(|tier| body.get(*tier))
        .find(|tier| tier.get("id").and_then(|id| id.as_str()).is_some_and(|id| id != "free-tier"))
        .and_then(|tier| tier.get("name")?.as_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    Some((ranked.into_iter().map(|(_, window)| window).collect(), plan))
}

/// A quota group's rank and label: Gemini's, the Claude and GPT one, or
/// another by its own name ("Imagen models" → "Imagen").
fn antigravity_group(name: &str) -> (u8, String) {
    let lower = name.to_ascii_lowercase();
    if lower.contains("gemini") {
        (0, crate::ANTIGRAVITY_USAGE_GEMINI.to_owned())
    } else if lower.contains("claude") || lower.contains("gpt") {
        (1, crate::ANTIGRAVITY_USAGE_OTHER.to_owned())
    } else {
        let short = name.trim().trim_end_matches(" models").trim_end_matches(" Models").trim();
        (2, if short.is_empty() { "Models".to_owned() } else { short.to_owned() })
    }
}

/// A limit's span: its `window` ("5h", "weekly", …), else its name.
fn antigravity_span(bucket: &serde_json::Value) -> (u8, String) {
    let window = bucket.get("window").and_then(|w| w.as_str()).unwrap_or_default();
    match window.to_ascii_lowercase().as_str() {
        "5h" => (0, "Session".to_owned()),
        "daily" | "day" | "1d" => (1, "Day".to_owned()),
        "weekly" | "week" | "7d" => (2, "Week".to_owned()),
        "monthly" | "month" | "30d" => (3, "Month".to_owned()),
        _ => {
            let name = bucket
                .get("displayName")
                .or_else(|| bucket.get("display_name"))
                .and_then(|n| n.as_str())
                .map(|n| n.trim_end_matches(" Remaining").trim_end_matches(" Limit").trim())
                .filter(|n| !n.is_empty())
                .unwrap_or("Limit");
            (4, name.to_owned())
        }
    }
}

/// Claude's `/api/oauth/usage`: the 5-hour session and weekly buckets, each
/// a 0-100 `utilization` with an RFC3339 `resets_at`, plus monthly spend.
pub fn claude(body: &serde_json::Value) -> Option<Vec<AgentUsageWindow>> {
    let mut windows = Vec::new();
    for (key, label) in [("five_hour", "Session"), ("seven_day", "Week")] {
        if let Some(w) = body.get(key)
            && let Some(utilization) = w.get("utilization").and_then(|v| v.as_f64())
        {
            windows.push(AgentUsageWindow {
                spend: None,
                label: label.to_string(),
                used_fraction: (utilization / 100.0) as f32,
                resets_at: parse_when(w.get("resets_at")),
            });
        }
    }
    if let Some(spend) = claude_spend(body) {
        let used_fraction = match spend.limit {
            Some(limit) if limit > 0.0 => (spend.used / limit).min(f32::MAX as f64) as f32,
            Some(_) if spend.used > 0.0 => 1.0,
            _ => 0.0,
        };
        windows.push(AgentUsageWindow {
            label: "Month".into(),
            used_fraction,
            // The legacy payload supplies no billing reset. Don't invent one.
            resets_at: parse_when(body.get("spend").and_then(|s| s.get("resets_at"))),
            spend: Some(spend),
        });
    }
    (!windows.is_empty()).then_some(windows)
}

fn nonnegative(value: &serde_json::Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite() && *n >= 0.0)
}

fn currency(value: Option<&serde_json::Value>) -> Option<String> {
    let code = value.and_then(|v| v.as_str()).unwrap_or("USD");
    (code.len() == 3 && code.bytes().all(|c| c.is_ascii_alphabetic()))
        .then(|| code.to_ascii_uppercase())
}

/// Current spend amounts carry their own exponent; legacy extra_usage uses
/// minor units (USD cents). Keep uncapped spend and zero caps distinguishable.
fn claude_spend(body: &serde_json::Value) -> Option<AgentUsageSpend> {
    if let Some(spend) = body.get("spend").filter(|s| !s.is_null()) {
        let amount = |v: &serde_json::Value| {
            let exponent = v.get("exponent")?.as_u64()?;
            if exponent > 6 {
                return None;
            }
            Some((
                nonnegative(v.get("amount_minor")?)? / 10_f64.powi(exponent as i32),
                currency(v.get("currency"))?,
            ))
        };
        let (used, currency) = amount(spend.get("used")?)?;
        if spend.get("enabled").and_then(|v| v.as_bool()) != Some(true) && used == 0.0 {
            return None;
        }
        let limit = match spend.get("limit")? {
            serde_json::Value::Null => None,
            value => {
                let (limit, limit_currency) = amount(value)?;
                if limit_currency != currency {
                    return None;
                }
                Some(limit)
            }
        };
        return Some(AgentUsageSpend {
            used,
            limit,
            currency,
        });
    }
    let extra = body.get("extra_usage")?;
    let used = nonnegative(extra.get("used_credits")?)?;
    if extra.get("is_enabled").and_then(|v| v.as_bool()) != Some(true) && used == 0.0 {
        return None;
    }
    let exponent = match extra.get("decimal_places") {
        None => 2,
        Some(value) => value.as_u64()?,
    };
    if exponent > 6 {
        return None;
    }
    let divisor = 10_f64.powi(exponent as i32);
    let limit = match extra.get("monthly_limit")? {
        serde_json::Value::Null => None,
        value => Some(nonnegative(value)? / divisor),
    };
    Some(AgentUsageSpend {
        used: used / divisor,
        limit,
        currency: currency(extra.get("currency"))?,
    })
}

/// Codex `/wham/usage`: primary/secondary windows + the live plan ("free"/
/// "plus"/"pro"…), which beats the login-time JWT claim.
pub fn codex(body: &serde_json::Value) -> Option<(Vec<AgentUsageWindow>, Option<String>)> {
    let rl = body.get("rate_limit")?;
    let mut windows = Vec::new();
    for key in ["primary_window", "secondary_window"] {
        if let Some(w) = rl.get(key)
            && let Some(used) = w.get("used_percent").and_then(|v| v.as_f64())
        {
            let span = w
                .get("limit_window_seconds")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            windows.push(AgentUsageWindow {
                spend: None,
                label: codex_window_label(span).to_string(),
                used_fraction: (used / 100.0) as f32,
                resets_at: parse_when(w.get("reset_at")),
            });
        }
    }
    if windows.is_empty() {
        return None;
    }
    let plan = body
        .get("plan_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    Some((windows, codex_plan(plan)))
}

/// "ChatGPT Plus" from Codex's `plan_type`.
pub fn codex_plan(plan: Option<&str>) -> Option<String> {
    let plan = plan?;
    let mut chars = plan.chars();
    let first = chars.next()?;
    Some(format!(
        "ChatGPT {}{}",
        first.to_uppercase(),
        chars.as_str()
    ))
}

/// Meter label for a Codex rate-limit window from its `limit_window_seconds`:
/// the free tier's window is a 30-day month (2_592_000s), Plus runs a 5-hour
/// primary (~18_000s) with a weekly secondary (604_800s). A bare "> 1 day =
/// week" rule mislabeled the monthly window "Week"; thresholds in seconds
/// leave the middle gaps to the nearest label rather than guessing a plan.
pub fn codex_window_label(span_seconds: i64) -> &'static str {
    const DAY: i64 = 86_400;
    if span_seconds >= 28 * DAY {
        "Month"
    } else if span_seconds >= 5 * DAY {
        "Week"
    } else {
        "Session"
    }
}

/// A reset time as Unix seconds or RFC 3339.
pub fn parse_when(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    match value? {
        serde_json::Value::Number(n) => DateTime::<Utc>::from_timestamp(n.as_i64()?, 0),
        serde_json::Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn enterprise(used: f64, limit: serde_json::Value) -> serde_json::Value {
        json!({
            "five_hour": null,
            "seven_day": null,
            "extra_usage": {
                "is_enabled": true,
                "used_credits": used,
                "monthly_limit": limit,
                "currency": "USD",
            }
        })
    }

    #[test]
    fn enterprise_spend_without_quota_windows_is_a_reading() {
        let (windows, plan) =
            from_reply(HarnessId::ClaudeCode, &enterprise(12345.0, json!(100000)))
                .expect("enterprise spend");
        assert_eq!(plan, None);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Month");
        assert_eq!(windows[0].value_label(), "$123.45 / $1000.00");
        assert!((windows[0].used_fraction - 0.12345).abs() < 1e-6);
        assert!(windows[0].has_limit());
        assert_eq!(windows[0].resets_at, None);
        // Remote account snapshots preserve the amounts as well as the fraction.
        let wire = serde_json::to_value(&windows[0]).unwrap();
        assert_eq!(
            serde_json::from_value::<AgentUsageWindow>(wire).unwrap(),
            windows[0]
        );
    }

    #[test]
    fn spend_can_coexist_with_subscription_windows() {
        let mut body = enterprise(100.0, json!(5000));
        body["five_hour"] = json!({"utilization": 25, "resets_at": "2026-11-01T00:00:00Z"});
        body["seven_day"] = json!({"utilization": 10});
        let windows = claude(&body).unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].value_label(), "25%");
        assert_eq!(windows[1].value_label(), "10%");
        assert_eq!(windows[2].value_label(), "$1.00 / $50.00");
    }

    #[test]
    fn unlimited_zero_and_exceeded_caps_remain_distinct() {
        let unlimited = claude(&enterprise(12345.0, json!(null))).unwrap().remove(0);
        assert_eq!(unlimited.value_label(), "$123.45 / No limit");
        assert!(!unlimited.has_limit());
        let zero = claude(&enterprise(100.0, json!(0))).unwrap().remove(0);
        assert_eq!(zero.value_label(), "$1.00 / $0.00");
        assert_eq!(zero.used_fraction, 1.0);
        assert!(zero.has_limit());
        let exceeded = claude(&enterprise(6000.0, json!(5000))).unwrap().remove(0);
        assert_eq!(exceeded.value_label(), "$60.00 / $50.00");
        assert_eq!(exceeded.used_fraction, 1.2);
    }

    #[test]
    fn current_spend_uses_explicit_currency_exponents_and_takes_precedence() {
        let mut body = enterprise(999.0, json!(999));
        body["spend"] = json!({
            "enabled": true,
            "used": {"amount_minor": 12345, "exponent": 2, "currency": "usd"},
            "limit": {"amount_minor": 1000, "exponent": 0, "currency": "USD"},
            "resets_at": "2026-11-01T00:00:00Z",
        });
        let window = claude(&body).unwrap().remove(0);
        assert_eq!(window.value_label(), "$123.45 / $1000.00");
        assert!(window.resets_at.is_some());
        body["spend"]["limit"] = json!(null);
        assert!(!claude(&body).unwrap()[0].has_limit());
        body["spend"]["limit"] = json!({"amount_minor": 1000, "exponent": 2, "currency": "EUR"});
        assert!(claude(&body).is_none());
    }

    #[test]
    fn missing_or_invalid_spend_does_not_invent_a_reading() {
        for extra in [
            json!({}),
            json!({"is_enabled": true}),
            json!({"is_enabled": true, "used_credits": null, "monthly_limit": 100}),
            json!({"is_enabled": true, "used_credits": -1, "monthly_limit": 100}),
            json!({"is_enabled": true, "used_credits": 100, "monthly_limit": -1}),
            json!({"is_enabled": false, "used_credits": 0, "monthly_limit": 0}),
            json!({"is_enabled": true, "used_credits": 0}),
        ] {
            assert!(claude(&json!({"extra_usage": extra})).is_none());
        }
        // Hitting a cap can disable sends; don't hide the already incurred spend.
        let mut body = enterprise(5000.0, json!(5000));
        body["extra_usage"]["is_enabled"] = json!(false);
        assert_eq!(claude(&body).unwrap()[0].value_label(), "$50.00 / $50.00");
    }

    #[test]
    fn older_hosts_still_decode_percent_windows() {
        let window: AgentUsageWindow = serde_json::from_value(json!({
            "label": "Week", "usedFraction": 0.42, "resetsAt": null,
        }))
        .unwrap();
        assert_eq!(window.value_label(), "42%");
        assert!(window.spend.is_none());
        assert!(window.has_limit());
    }

    /// A Google AI Ultra account's reply (live, `loadCodeAssist`'s tiers
    /// beside it): only the Claude and GPT group is limited, its 5-hour limit
    /// first; the paid tier names the plan.
    #[test]
    fn antigravity_limits_by_group_and_span() {
        let body = serde_json::json!({
            "groups": [{
                "displayName": "Claude and GPT models",
                "description": "Models within this group: Claude Opus, Claude Sonnet, GPT-OSS",
                "buckets": [
                    { "bucketId": "3p-weekly", "displayName": "Weekly Limit Remaining", "window": "weekly",
                      "resetTime": "2026-11-26T21:17:50Z", "remainingFraction": 0.75 },
                    { "bucketId": "3p-5h", "displayName": "Five Hour Limit Remaining", "window": "5h",
                      "resetTime": "2026-11-20T02:17:50Z", "remainingFraction": 1 }
                ]
            }],
            "currentTier": { "id": "free-tier", "name": "Antigravity" },
            "paidTier": { "id": "g1-ultra-tier", "name": "Google AI Ultra" }
        });
        let (windows, plan) = antigravity(&body).unwrap();
        let labels: Vec<&str> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Claude & GPT Session", "Claude & GPT Week"]);
        assert_eq!(windows[0].used_fraction, 0.0);
        assert!((windows[1].used_fraction - 0.25).abs() < 1e-6);
        assert_eq!(windows[1].resets_at.unwrap().to_rfc3339(), "2026-11-26T21:17:50+00:00");
        assert_eq!(plan.as_deref(), Some("Google AI Ultra"));
        assert_eq!(from_reply(HarnessId::Antigravity, &body).unwrap().0.len(), 2);
    }

    /// Gemini's group goes first; a disabled limit, or one that's only a
    /// count, isn't a meter; a free account names no plan; no `groups` is no
    /// reply this reads, an empty one a tier with no limits.
    #[test]
    fn antigravity_orders_gemini_first_and_skips_what_isnt_a_meter() {
        let body = serde_json::json!({
            "groups": [
                { "displayName": "Claude and GPT models", "buckets": [{ "window": "weekly", "remainingFraction": 0.5 }] },
                { "displayName": "Gemini Models", "buckets": [
                    { "window": "weekly", "remainingFraction": 0.9 },
                    { "window": "5h", "remainingFraction": 0.2 },
                    { "window": "daily", "disabled": true, "remainingFraction": 0.0 },
                    { "displayName": "Requests", "remainingAmount": "100" }
                ] },
                { "displayName": "Imagen models", "buckets": [{ "displayName": "Daily Limit Remaining", "remainingFraction": 1.4 }] }
            ],
            "currentTier": { "id": "free-tier", "name": "Antigravity" }
        });
        let (windows, plan) = antigravity(&body).unwrap();
        let labels: Vec<&str> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Gemini Session", "Gemini Week", "Claude & GPT Week", "Imagen Daily"]);
        assert!((windows[0].used_fraction - 0.8).abs() < 1e-6);
        assert_eq!(windows[3].used_fraction, 0.0, "clamped");
        assert_eq!(plan, None);
        assert_eq!(antigravity(&serde_json::json!({ "error": "x" })), None);
        assert_eq!(antigravity(&serde_json::json!({ "groups": [] })), Some((Vec::new(), None)));
    }
}
