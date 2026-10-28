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
        _ => None,
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
}
