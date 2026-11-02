use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};

use super::client::CLIENT_ID;
use super::events;
use super::mapper::{self, PlanFacts};
use super::summary;
use super::*;
use crate::account::{Account, Credential, OAuthTokens};
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::login::{LoginPrompt, LoginPrompter};
use crate::metric::{MetricKind, MetricLine, MetricValue, ProgressFormat, ProviderUsage};
use crate::provider::{FetchContext, ProviderState};
use crate::vault::{MemoryVault, Vault};

fn jwt(payload: &str) -> String {
    format!(
        "eyJhbGciOiJub25lIn0.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes())
    )
}

fn ok(body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: body.as_bytes().to_vec(),
    }
}

fn http_status(status: u16, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        headers: BTreeMap::new(),
        body: body.as_bytes().to_vec(),
    }
}

fn object(value: Value) -> Map<String, Value> {
    value.as_object().expect("object").clone()
}

fn meter(usage: &ProviderUsage, label: &str) -> (f64, f64, ProgressFormat) {
    match usage.lines.iter().find(|line| line.label() == label) {
        Some(MetricLine::Progress {
            used,
            limit,
            format,
            ..
        }) => (*used, *limit, format.clone()),
        other => panic!("{label} was {other:?}"),
    }
}

fn resets(usage: &ProviderUsage, label: &str) -> Option<DateTime<Utc>> {
    match usage.lines.iter().find(|line| line.label() == label) {
        Some(MetricLine::Progress { resets_at, .. }) => *resets_at,
        other => panic!("{label} was {other:?}"),
    }
}

fn figures(usage: &ProviderUsage, label: &str) -> Vec<MetricValue> {
    match usage.lines.iter().find(|line| line.label() == label) {
        Some(MetricLine::Values { values, .. }) => values.clone(),
        other => panic!("{label} was {other:?}"),
    }
}

struct Script {
    requests: Mutex<Vec<HttpRequest>>,
    handle: Mutex<Box<dyn FnMut(&HttpRequest) -> Result<HttpResponse, ProviderError> + Send>>,
}

impl HttpClient for Script {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        (self.handle.lock().unwrap())(&request)
    }
}

fn paths(http: &Script) -> Vec<String> {
    http.requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| url::Url::parse(&request.url).unwrap().path().to_string())
        .collect()
}

struct Lines {
    lines: Mutex<Vec<Option<String>>>,
    shown: Mutex<Vec<LoginPrompt>>,
}

impl LoginPrompter for Lines {
    fn show(&self, prompt: &LoginPrompt) {
        self.shown.lock().unwrap().push(prompt.clone());
    }
    fn read_line(&self, _secret: bool) -> Option<String> {
        self.lines.lock().unwrap().pop().flatten()
    }
    fn pasted_codes(&self) -> std::sync::mpsc::Receiver<String> {
        std::sync::mpsc::channel().1
    }
}

const INDIVIDUAL: &str = r#"{"billingCycleEnd":"1764547200000","billingCycleStart":"1761955200000","enabled":true,"planUsage":{"apiPercentUsed":40,"autoPercentUsed":20,"limit":40000,"remaining":30000,"totalPercentUsed":25},"spendLimitUsage":{"individualLimit":50000,"individualRemaining":38000,"limitType":"user"}}"#;
const PLAN_ULTRA: &str = r#"{"planInfo":{"planName":"Ultra"}}"#;
const STRIPE: &str = r#"{"customerBalance":-2500,"membershipType":"ultra"}"#;
const GRANTS: &str = r#"{"hasCreditGrants":true,"totalCents":10000,"usedCents":4000}"#;
const GROK: &str = r#"{"currentPeriodStart":"2026-10-20T00:00:00Z","hasNonZeroIncludedLimit":true,"nextResetTimestampUtc":"2026-10-27T00:00:00Z","usagePercent":30}"#;
const ENTERPRISE_USAGE: &str = r#"{"billingCycleEnd":"1764547200000","billingCycleStart":"1761955200000","enabled":true,"planUsage":{}}"#;
const REQUESTS: &str =
    r#"{"gpt-4":{"numRequests":120,"maxRequestUsage":500},"startOfMonth":"2026-10-01T00:00:00Z"}"#;
const ENTERPRISE_SUMMARY: &str = r#"{"billingCycleStart":"2026-10-01T00:00:00Z","billingCycleEnd":"2026-11-01T00:00:00Z","membershipType":"enterprise","individualUsage":{"onDemand":{"enabled":true,"used":2500,"limit":10000}}}"#;
const POOL_USAGE: &str = r#"{"billingCycleEnd":"1764547200000","billingCycleStart":"1761955200000","enabled":true,"planUsage":{"apiPercentUsed":100,"autoPercentUsed":75,"limit":2000,"totalSpend":1500,"totalPercentUsed":76}}"#;
const PERSONAL_SUMMARY: &str = r#"{"billingCycleStart":"2026-10-01T00:00:00Z","billingCycleEnd":"2026-11-01T00:00:00Z","membershipType":"pro"}"#;
const TEAM_USAGE: &str = r#"{"billingCycleEnd":"1764547200000","billingCycleStart":"1761955200000","enabled":true,"planUsage":{"apiPercentUsed":10,"autoPercentUsed":5,"limit":2000,"totalSpend":300,"totalPercentUsed":7},"spendLimitUsage":{"limitType":"team"}}"#;
const TEAM_SUMMARY: &str = r#"{"billingCycleStart":"2026-10-01T00:00:00Z","billingCycleEnd":"2026-11-01T00:00:00Z","membershipType":"team","individualUsage":{"onDemand":{"enabled":true,"used":1500,"limit":20000}}}"#;

struct Seat {
    usage: String,
    plan: String,
    summary: Option<String>,
    requests: Option<String>,
    events: Option<String>,
}

fn seat(usage: &str, plan: &str) -> Seat {
    Seat {
        usage: usage.into(),
        plan: plan.into(),
        summary: None,
        requests: None,
        events: None,
    }
}

fn refresh_seat(seat: Seat) -> (ProviderUsage, Vec<String>) {
    let token = jwt(r#"{"sub":"auth0|user_fake"}"#);
    let account = Account::new(ProviderKind::Cursor, "Cursor", None);
    let mut tokens = OAuthTokens::new(token);
    tokens.refresh_token = Some("refresh".into());
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens));
    let seat = Arc::new(seat);
    let http = Script {
        requests: Mutex::new(vec![]),
        handle: Mutex::new(Box::new(move |request: &HttpRequest| {
            let path = url::Url::parse(&request.url).unwrap().path().to_string();
            let body = match path.as_str() {
                "/aiserver.v1.DashboardService/GetCurrentPeriodUsage" => Some(seat.usage.clone()),
                "/aiserver.v1.DashboardService/GetPlanInfo" => {
                    Some(format!(r#"{{"planInfo":{{"planName":"{}"}}}}"#, seat.plan))
                }
                "/aiserver.v1.DashboardService/GetCreditGrantsBalance" => Some(GRANTS.into()),
                "/api/auth/stripe" => Some(STRIPE.into()),
                "/aiserver.v1.DashboardService/GetSandUsageStatus" => Some(GROK.into()),
                "/api/usage-summary" => seat.summary.clone(),
                "/api/usage" => seat.requests.clone(),
                "/api/dashboard/get-filtered-usage-events" => seat.events.clone(),
                _ => None,
            };
            Ok(match body {
                Some(body) => ok(&body),
                None => http_status(404, ""),
            })
        })),
    };
    let mut state = ProviderState::default();
    let usage = {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: Utc::now(),
            force: true,
        };
        provider().fetch(&mut ctx).unwrap()
    };
    (usage, paths(&http))
}

#[test]
fn an_individual_seat_shows_pools_on_demand_grok_and_credits() {
    let (usage, paths) = refresh_seat(seat(INDIVIDUAL, "Ultra"));
    assert_eq!(usage.plan.as_deref(), Some("Ultra"));
    assert_eq!(meter(&usage, "Total usage").0, 25.0);
    assert_eq!(meter(&usage, "Cursor Models").0, 20.0);
    assert_eq!(meter(&usage, "Other Models").0, 40.0);
    assert_eq!(
        meter(&usage, "On-demand"),
        (120.0, 500.0, ProgressFormat::Dollars)
    );
    assert_eq!(resets(&usage, "On-demand"), resets(&usage, "Total usage"));
    assert_eq!(
        resets(&usage, "Total usage"),
        DateTime::from_timestamp(1_764_547_200, 0)
    );
    assert_eq!(meter(&usage, "Grok Bot usage").0, 30.0);
    assert_eq!(
        resets(&usage, "Grok Bot usage"),
        Some(
            DateTime::parse_from_rfc3339("2026-10-27T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        )
    );
    assert_eq!(
        figures(&usage, "Credits"),
        vec![MetricValue::new(85.0, MetricKind::Dollars)]
    );
    assert_eq!(
        paths
            .iter()
            .filter(|path| path.as_str() == "/api/usage-summary")
            .count(),
        1
    );
    assert!(!paths.iter().any(|path| path == "/api/usage"));
}

#[test]
fn a_request_based_enterprise_seat_shows_requests_and_on_demand() {
    for plan in ["Enterprise", "Pro"] {
        let mut seat = seat(ENTERPRISE_USAGE, plan);
        seat.summary = Some(ENTERPRISE_SUMMARY.into());
        seat.requests = Some(REQUESTS.into());
        let (usage, _) = refresh_seat(seat);
        assert_eq!(usage.plan.as_deref(), Some("Enterprise"), "{plan}");
        assert_eq!(
            meter(&usage, "Requests"),
            (
                120.0,
                500.0,
                ProgressFormat::Count {
                    suffix: "requests".into()
                }
            )
        );
        assert_eq!(meter(&usage, "Total usage").0, 120.0);
        assert_eq!(
            meter(&usage, "On-demand"),
            (25.0, 100.0, ProgressFormat::Dollars)
        );
    }
}

#[test]
fn a_personal_seat_on_the_generic_request_fallback_shows_requests_as_total_usage() {
    let mut seat = seat(ENTERPRISE_USAGE, "Pro");
    seat.summary = Some(PERSONAL_SUMMARY.into());
    seat.requests = Some(REQUESTS.into());
    let (usage, _) = refresh_seat(seat);
    assert_eq!(usage.plan.as_deref(), Some("Pro"));
    assert_eq!(meter(&usage, "Requests").0, 120.0);
    assert_eq!(
        meter(&usage, "Total usage"),
        (
            120.0,
            500.0,
            ProgressFormat::Count {
                suffix: "requests".into()
            }
        )
    );
}

#[test]
fn a_pro_reported_enterprise_seat_takes_on_demand_from_the_summary() {
    let mut seat = seat(POOL_USAGE, "Pro");
    seat.summary = Some(ENTERPRISE_SUMMARY.into());
    let (usage, _) = refresh_seat(seat);
    assert_eq!(usage.plan.as_deref(), Some("Enterprise"));
    assert_eq!(meter(&usage, "Total usage").0, 76.0);
    assert_eq!(meter(&usage, "Cursor Models").0, 75.0);
    assert_eq!(meter(&usage, "Other Models").0, 100.0);
    assert_eq!(
        meter(&usage, "On-demand"),
        (25.0, 100.0, ProgressFormat::Dollars)
    );
}

#[test]
fn a_team_membership_relabels_a_pro_seat_without_changing_its_total() {
    for plan_usage in [
        r#"{"limit":2000,"totalSpend":400,"totalPercentUsed":20}"#,
        r#"{"totalPercentUsed":20}"#,
        r#"{"totalPercentUsed":20,"autoPercentUsed":10,"apiPercentUsed":30}"#,
        r#"{"limit":2000,"totalSpend":400,"autoPercentUsed":10,"apiPercentUsed":30}"#,
    ] {
        let mut seat = seat(
            &format!(r#"{{"enabled":true,"planUsage":{plan_usage}}}"#),
            " Pro ",
        );
        seat.summary = Some(TEAM_SUMMARY.into());
        let (usage, _) = refresh_seat(seat);
        assert_eq!(usage.plan.as_deref(), Some("Team"));
        assert_eq!(
            meter(&usage, "Total usage"),
            (20.0, 100.0, ProgressFormat::Percent)
        );
    }
}

#[test]
fn only_a_missing_or_pro_plan_name_gives_way_to_an_org_membership() {
    for plan in ["Ultra", "Business", "Enterprise", "Pro Plus"] {
        assert_eq!(
            mapper::resolved_plan_label(Some(plan), Some("team")).as_deref(),
            Some(plan)
        );
    }
    for plan in [None, Some(""), Some("  "), Some("Pro"), Some(" pro plan ")] {
        assert_eq!(
            mapper::resolved_plan_label(plan, Some("enterprise")).as_deref(),
            Some("Enterprise")
        );
    }
    assert_eq!(
        mapper::resolved_plan_label(Some("Ultra"), Some("pro")).as_deref(),
        Some("Ultra")
    );
}

#[test]
fn a_team_seat_takes_on_demand_and_membership_from_the_summary() {
    let mut seat = seat(TEAM_USAGE, "Pro");
    seat.summary = Some(TEAM_SUMMARY.into());
    let (usage, _) = refresh_seat(seat);
    assert_eq!(usage.plan.as_deref(), Some("Team"));
    assert_eq!(meter(&usage, "Total usage").0, 7.0);
    assert_eq!(
        meter(&usage, "On-demand"),
        (15.0, 200.0, ProgressFormat::Dollars)
    );
}

#[test]
fn quiet_history_and_request_rows_cover_the_cursor_labels() {
    let mut individual = seat(INDIVIDUAL, "Ultra");
    individual.events = Some(r#"{"totalUsageEventsCount":0}"#.into());
    let (individual, _) = refresh_seat(individual);
    let mut enterprise = seat(ENTERPRISE_USAGE, "Enterprise");
    enterprise.summary = Some(ENTERPRISE_SUMMARY.into());
    enterprise.requests = Some(REQUESTS.into());
    let (enterprise, _) = refresh_seat(enterprise);
    let mut labels: Vec<&str> = individual
        .lines
        .iter()
        .chain(enterprise.lines.iter())
        .map(MetricLine::label)
        .collect();
    labels.sort();
    labels.dedup();
    for label in [
        "Total usage",
        "Cursor Models",
        "Other Models",
        "On-demand",
        "Grok Bot usage",
        "Requests",
        "Credits",
        "Today",
        "Yesterday",
        "Last 30 Days",
    ] {
        assert!(labels.contains(&label), "{label} missing from {labels:?}");
    }
}

#[test]
fn summary_maps_provider_returned_on_demand_dollars() {
    let summary = object(json!({
        "membershipType": "enterprise",
        "individualUsage": {
            "plan": {"enabled": true, "limit": 0, "totalPercentUsed": 6.25},
            "onDemand": {"enabled": true, "used": 12345, "limit": 200000}
        }
    }));
    let mapped = summary::map(Some(&summary), None, Some("Enterprise"), "unavailable").unwrap();
    assert_eq!(mapped.plan.as_deref(), Some("Enterprise"));
    assert_eq!(
        meter(&mapped, "On-demand"),
        (123.45, 2000.0, ProgressFormat::Dollars)
    );
}

#[test]
fn summary_rejects_a_response_without_usable_metrics() {
    let summary = object(json!({"individualUsage": {"plan": {"limit": 0}}}));
    assert!(summary::map(Some(&summary), None, None, "unavailable").is_err());
}

fn map_team(plan: Value) -> ProviderUsage {
    let usage = json!({
        "enabled": true,
        "planUsage": plan,
        "billingCycleStart": 1_770_000_000_000u64,
        "billingCycleEnd": 1_772_592_000_000u64,
        "spendLimitUsage": {"limitType": "team", "pooledUsed": 0}
    });
    mapper::map_usage(&object(usage), Some("Team"), None, 0.0).unwrap()
}

#[test]
fn a_reported_standard_seat_uses_the_structured_total() {
    let mapped = map_team(json!({
        "totalSpend": 1022, "includedSpend": 1022, "remaining": 978, "limit": 2000,
        "autoPercentUsed": 2.6476190476190475, "apiPercentUsed": 11.65, "totalPercentUsed": 4.088
    }));
    assert_eq!(
        meter(&mapped, "Total usage"),
        (4.088, 100.0, ProgressFormat::Percent)
    );
    assert_eq!(meter(&mapped, "Cursor Models").0, 2.6476190476190475);
    assert_eq!(meter(&mapped, "Other Models").0, 11.65);
}

#[test]
fn a_premium_seat_without_a_total_does_not_invent_one() {
    let mapped = map_team(
        json!({"limit": 2000, "totalSpend": 666, "autoPercentUsed": 0.3728571428571429, "apiPercentUsed": 2.025}),
    );
    assert!(mapped
        .lines
        .iter()
        .all(|line| line.label() != "Total usage"));
    assert_eq!(meter(&mapped, "Cursor Models").0, 0.3728571428571429);
    assert_eq!(meter(&mapped, "Other Models").0, 2.025);
}

#[test]
fn pools_without_a_limit_do_not_trigger_the_request_fallback() {
    let usage = object(
        json!({"enabled": true, "planUsage": {"autoPercentUsed": 0, "apiPercentUsed": 0}, "spendLimitUsage": {"limitType": "team"}}),
    );
    let facts = PlanFacts::from_usage(&usage);
    for name in [Some("Team"), Some("Enterprise"), None] {
        assert!(!mapper::should_use_request_fallback(&facts, name).should);
    }
    assert!(!facts.should_try_generic_request_fallback());
    let mapped = mapper::map_usage(&usage, None, None, 0.0).unwrap();
    assert_eq!(meter(&mapped, "Cursor Models").0, 0.0);
    assert_eq!(meter(&mapped, "Other Models").0, 0.0);
}

#[test]
fn a_billing_cycle_too_long_for_an_integer_falls_back_to_a_month() {
    let mut usage = object(
        json!({"enabled": true, "planUsage": {"autoPercentUsed": 0, "apiPercentUsed": 0}, "billingCycleStart": 0}),
    );
    usage.insert(
        "billingCycleEnd".into(),
        Value::Number(serde_json::Number::from_f64(1e300).unwrap()),
    );
    let mapped = mapper::map_usage(&usage, None, None, 0.0).unwrap();
    match mapped
        .lines
        .iter()
        .find(|line| line.label() == "Cursor Models")
    {
        Some(MetricLine::Progress {
            period_duration_ms, ..
        }) => assert_eq!(*period_duration_ms, Some(super::parse::MONTH_MS)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_legacy_dollar_pool_with_zero_placeholders_keeps_the_dollar_meter() {
    let mapped = map_team(
        json!({"limit": 500_000, "totalSpend": 234_500, "autoPercentUsed": 0, "apiPercentUsed": 0, "totalPercentUsed": 0}),
    );
    assert_eq!(
        meter(&mapped, "Total usage"),
        (2345.0, 5000.0, ProgressFormat::Dollars)
    );
}

#[test]
fn incomplete_pools_keep_the_legacy_dollar_meter() {
    for plan in [
        json!({"autoPercentUsed": 1, "limit": 500_000, "totalSpend": 234_500}),
        json!({"autoPercentUsed": true, "apiPercentUsed": 2, "limit": 500_000, "totalSpend": 234_500}),
        json!({"autoPercentUsed": -1, "apiPercentUsed": 2, "limit": 500_000, "totalSpend": 234_500}),
    ] {
        assert_eq!(
            meter(&map_team(plan), "Total usage").2,
            ProgressFormat::Dollars
        );
    }
}

#[test]
fn a_team_name_alone_recognizes_pools_and_keeps_on_demand_and_credits() {
    let usage = object(json!({
        "enabled": true,
        "planUsage": {"limit": 2000, "autoPercentUsed": 0, "apiPercentUsed": 12},
        "spendLimitUsage": {"individualLimit": 5000, "individualRemaining": 4000}
    }));
    let mapped = mapper::map_usage(&usage, Some(" Team "), None, 1000.0).unwrap();
    assert!(mapped
        .lines
        .iter()
        .all(|line| line.label() != "Total usage"));
    assert!(mapped.lines.iter().any(|line| line.label() == "On-demand"));
    assert!(mapped.lines.iter().any(|line| line.label() == "Credits"));
}

const PERIOD: &str = r#"{"billingCycleEnd":"1764547200000","billingCycleStart":"1761955200000","enabled":true,"planUsage":{"apiPercentUsed":12.5,"autoPercentUsed":3.25,"limit":2000,"remaining":1500,"totalPercentUsed":25},"spendLimitUsage":{"individualLimit":5000,"individualRemaining":4000,"limitType":"user"}}"#;
const SAND: &str = r#"{"hasNonZeroIncludedLimit":true,"usagePercent":10}"#;

#[test]
fn current_period_usage_fills_the_cursor_lines() {
    let account = Account::new(ProviderKind::Cursor, "Cursor Account", None);
    let mut tokens = OAuthTokens::new("access");
    tokens.refresh_token = Some("refresh".into());
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens));
    let http = Script {
        requests: Mutex::new(vec![]),
        handle: Mutex::new(Box::new(|request: &HttpRequest| {
            Ok(match url::Url::parse(&request.url).unwrap().path() {
                "/aiserver.v1.DashboardService/GetCurrentPeriodUsage" => ok(PERIOD),
                "/oauth/token" => ok(r#"{"access_token":"renewed"}"#),
                "/aiserver.v1.DashboardService/GetPlanInfo" => {
                    ok(r#"{"planInfo":{"planName":"Pro"}}"#)
                }
                "/aiserver.v1.DashboardService/GetCreditGrantsBalance" => {
                    ok(r#"{"hasCreditGrants":false}"#)
                }
                "/aiserver.v1.DashboardService/GetSandUsageStatus" => ok(SAND),
                _ => http_status(404, ""),
            })
        })),
    };
    let mut state = ProviderState::default();
    let usage = {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: Utc::now(),
            force: true,
        };
        provider().fetch(&mut ctx).unwrap()
    };
    assert_eq!(usage.plan.as_deref(), Some("Pro"));
    assert_eq!(
        meter(&usage, "Total usage"),
        (25.0, 100.0, ProgressFormat::Percent)
    );
    assert_eq!(
        resets(&usage, "Total usage"),
        DateTime::from_timestamp(1_764_547_200, 0)
    );
    for label in [
        "Total usage",
        "Cursor Models",
        "Other Models",
        "On-demand",
        "Grok Bot usage",
    ] {
        assert!(
            usage.lines.iter().any(|line| line.label() == label),
            "{label}"
        );
    }
}

#[test]
fn a_rejected_token_is_refreshed_once_and_a_second_refusal_expires_the_sign_in() {
    let account = Account::new(ProviderKind::Cursor, "Cursor Account", None);
    let mut tokens = OAuthTokens::new("access");
    tokens.refresh_token = Some("refresh".into());
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens));
    let http = Script {
        requests: Mutex::new(vec![]),
        handle: Mutex::new(Box::new(|request: &HttpRequest| {
            Ok(if request.url.contains("/oauth/token") {
                ok(r#"{"access_token":"renewed"}"#)
            } else if request.url.contains("GetCurrentPeriodUsage") {
                http_status(401, "")
            } else {
                http_status(404, "")
            })
        })),
    };
    let mut state = ProviderState::default();
    let error = {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: Utc::now(),
            force: false,
        };
        provider().fetch(&mut ctx).unwrap_err()
    };
    assert_eq!(error, ProviderError::SignInExpired(ProviderKind::Cursor));
    let bearers: Vec<_> = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.contains("GetCurrentPeriodUsage"))
        .map(|request| request.headers.get("Authorization").cloned())
        .collect();
    assert_eq!(
        bearers,
        vec![Some("Bearer access".into()), Some("Bearer renewed".into())]
    );
}

const EXPIRY: i64 = 4_000_000_000;
fn expiry() -> DateTime<Utc> {
    DateTime::from_timestamp(EXPIRY, 0).unwrap()
}
fn new_expiry() -> DateTime<Utc> {
    expiry() + Duration::days(60)
}
fn old_token() -> String {
    jwt(&format!(r#"{{"sub":"auth0|user_old","exp":{EXPIRY}}}"#))
}
fn new_token() -> String {
    let exp = EXPIRY + 60 * 86_400;
    jwt(&format!(r#"{{"sub":"auth0|user_new","exp":{exp}}}"#))
}

struct RefreshHttp {
    requests: Mutex<Vec<HttpRequest>>,
    refresh: String,
    accepted: String,
}

impl HttpClient for RefreshHttp {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        if request.url.contains("/oauth/token") {
            return Ok(ok(&self.refresh));
        }
        if request.url.contains("GetCurrentPeriodUsage") {
            let bearer = request
                .headers
                .get("Authorization")
                .map(String::as_str)
                .unwrap_or("");
            let token = bearer.trim_start_matches("Bearer ");
            return Ok(if token == self.accepted {
                ok(
                    r#"{"enabled":true,"billingCycleStart":"1761955200000","billingCycleEnd":"1764547200000","planUsage":{"limit":2000,"remaining":1500,"totalPercentUsed":25}}"#,
                )
            } else {
                http_status(401, "")
            });
        }
        if request.url.contains("GetPlanInfo") {
            return Ok(ok(r#"{"planInfo":{"planName":"Pro"}}"#));
        }
        Ok(http_status(404, ""))
    }
}

fn run_refresh(
    stored: OAuthTokens,
    now: DateTime<Utc>,
    refresh_body: &str,
    accepted: &str,
) -> (Result<ProviderUsage, ProviderError>, RefreshHttp, Account) {
    let account = Account::new(ProviderKind::Cursor, "Cursor Account", None);
    let vault = MemoryVault::with(account.id, Credential::OAuth(stored));
    let http = RefreshHttp {
        requests: Mutex::new(vec![]),
        refresh: refresh_body.into(),
        accepted: accepted.into(),
    };
    let mut state = ProviderState::default();
    let result = {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now,
            force: false,
        };
        provider().fetch(&mut ctx)
    };
    (result, http, account)
}

#[test]
fn login_saves_the_access_token_expiry() {
    let token = old_token();
    let body = format!(r#"{{"accessToken":"{token}","refreshToken":"{token}"}}"#);
    let http = Script {
        requests: Mutex::new(vec![]),
        handle: Mutex::new(Box::new(move |_: &HttpRequest| Ok(ok(&body)))),
    };
    let prompt = Lines {
        lines: Mutex::new(vec![]),
        shown: Mutex::new(vec![]),
    };
    let (_, credential) = login(&crate::provider::LoginMethod::Default)
        .login(&prompt, &http)
        .unwrap();
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(tokens.expires_at, Some(expiry()));
}

#[test]
fn a_credential_without_expiry_takes_it_from_the_jwt() {
    let old = old_token();
    let mut stored = OAuthTokens::new(old.clone());
    stored.refresh_token = Some(old.clone());
    let (usage, http, account) = run_refresh(
        stored,
        expiry() - Duration::days(30),
        &format!(r#"{{"access_token":"{}"}}"#, new_token()),
        &old,
    );
    assert!(usage.is_ok());
    let vault = MemoryVault::with(account.id, Credential::OAuth(OAuthTokens::new("unused")));
    let _ = vault;
    let saved = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.url.contains("/oauth/token"));
    assert!(!saved);
}

#[test]
fn a_credential_without_expiry_is_saved_with_the_jwt_expiry() {
    let old = old_token();
    let account = Account::new(ProviderKind::Cursor, "Cursor Account", None);
    let mut stored = OAuthTokens::new(old.clone());
    stored.refresh_token = Some(old.clone());
    let vault = MemoryVault::with(account.id, Credential::OAuth(stored));
    let http = RefreshHttp {
        requests: Mutex::new(vec![]),
        refresh: format!(r#"{{"access_token":"{}"}}"#, new_token()),
        accepted: old.clone(),
    };
    let mut state = ProviderState::default();
    {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: expiry() - Duration::days(30),
            force: false,
        };
        provider().fetch(&mut ctx).unwrap();
    }
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(saved.expires_at, Some(expiry()));
    assert_eq!(saved.access_token, old);
    assert!(http
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|request| !request.url.contains("/oauth/token")));
}

#[test]
fn a_token_near_expiry_is_refreshed_before_usage_and_saved() {
    let old = old_token();
    let new = new_token();
    let mut stored = OAuthTokens::new(old.clone());
    stored.refresh_token = Some(old.clone());
    stored.expires_at = Some(expiry());
    let account = Account::new(ProviderKind::Cursor, "Cursor Account", None);
    let vault = MemoryVault::with(account.id, Credential::OAuth(stored));
    let http = RefreshHttp {
        requests: Mutex::new(vec![]),
        refresh: format!(r#"{{"access_token":"{new}","shouldLogout":false}}"#),
        accepted: new.clone(),
    };
    let mut state = ProviderState::default();
    {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: expiry() - Duration::days(1),
            force: false,
        };
        provider().fetch(&mut ctx).unwrap();
    }
    let usage_tokens: Vec<_> = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.contains("GetCurrentPeriodUsage"))
        .map(|request| {
            request
                .headers
                .get("Authorization")
                .unwrap()
                .trim_start_matches("Bearer ")
                .to_string()
        })
        .collect();
    assert_eq!(usage_tokens, vec![new.clone()]);
    let body: Value = serde_json::from_slice(
        http.requests
            .lock()
            .unwrap()
            .iter()
            .find(|request| request.url.contains("/oauth/token"))
            .unwrap()
            .body
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["grant_type"], "refresh_token");
    assert_eq!(body["client_id"], CLIENT_ID);
    assert_eq!(body["refresh_token"], old);
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(saved.access_token, new);
    assert_eq!(saved.refresh_token.as_deref(), Some(new.as_str()));
    assert_eq!(saved.expires_at, Some(new_expiry()));
}

#[test]
fn a_rejected_token_is_refreshed_once_when_it_is_not_near_expiry() {
    let old = old_token();
    let new = new_token();
    let mut stored = OAuthTokens::new(old.clone());
    stored.refresh_token = Some(old.clone());
    stored.expires_at = Some(expiry());
    let account = Account::new(ProviderKind::Cursor, "Cursor Account", None);
    let vault = MemoryVault::with(account.id, Credential::OAuth(stored));
    let http = RefreshHttp {
        requests: Mutex::new(vec![]),
        refresh: format!(r#"{{"access_token":"{new}"}}"#),
        accepted: new.clone(),
    };
    let mut state = ProviderState::default();
    {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: expiry() - Duration::days(30),
            force: false,
        };
        provider().fetch(&mut ctx).unwrap();
    }
    let usage_tokens: Vec<_> = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.contains("GetCurrentPeriodUsage"))
        .map(|request| {
            request
                .headers
                .get("Authorization")
                .unwrap()
                .trim_start_matches("Bearer ")
                .to_string()
        })
        .collect();
    assert_eq!(usage_tokens, vec![old, new.clone()]);
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(saved.access_token, new);
    assert_eq!(saved.refresh_token.as_deref(), Some(new.as_str()));
}

#[test]
fn a_logout_during_refresh_of_an_expired_token_is_an_expired_sign_in() {
    let old = old_token();
    let mut stored = OAuthTokens::new(old.clone());
    stored.refresh_token = Some(old.clone());
    let account = Account::new(ProviderKind::Cursor, "Cursor Account", None);
    let vault = MemoryVault::with(account.id, Credential::OAuth(stored));
    let http = RefreshHttp {
        requests: Mutex::new(vec![]),
        refresh: r#"{"access_token":"","shouldLogout":true}"#.into(),
        accepted: String::new(),
    };
    let mut state = ProviderState::default();
    let error = {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: expiry() + Duration::seconds(3600),
            force: false,
        };
        provider().fetch(&mut ctx).unwrap_err()
    };
    assert_eq!(error, ProviderError::SignInExpired(ProviderKind::Cursor));
    assert_eq!(
        http.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.contains("/oauth/token"))
            .count(),
        1
    );
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(saved.access_token, old);
}

#[test]
fn a_failed_early_refresh_keeps_the_still_valid_token() {
    let old = old_token();
    let mut stored = OAuthTokens::new(old.clone());
    stored.refresh_token = Some(old.clone());
    stored.expires_at = Some(expiry());
    let account = Account::new(ProviderKind::Cursor, "Cursor Account", None);
    let vault = MemoryVault::with(account.id, Credential::OAuth(stored));
    let accepted = old.clone();
    let http = Script {
        requests: Mutex::new(vec![]),
        handle: Mutex::new(Box::new(move |request: &HttpRequest| {
            if request.url.contains("/oauth/token") {
                return Ok(http_status(503, "{}"));
            }
            if request.url.contains("GetCurrentPeriodUsage") {
                let bearer = request
                    .headers
                    .get("Authorization")
                    .map(String::as_str)
                    .unwrap_or("");
                assert!(bearer.ends_with(&accepted));
                return Ok(ok(
                    r#"{"enabled":true,"billingCycleStart":"1761955200000","billingCycleEnd":"1764547200000","planUsage":{"limit":2000,"remaining":1500,"totalPercentUsed":25}}"#,
                ));
            }
            if request.url.contains("GetPlanInfo") {
                return Ok(ok(r#"{"planInfo":{"planName":"Pro"}}"#));
            }
            Ok(http_status(404, ""))
        })),
    };
    let mut state = ProviderState::default();
    {
        let mut ctx = FetchContext {
            account: &account,
            http: &http,
            vault: &vault,
            state: &mut state,
            now: expiry() - Duration::days(1),
            force: false,
        };
        provider().fetch(&mut ctx).unwrap();
    }
    assert_eq!(
        http.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.contains("/oauth/token"))
            .count(),
        1
    );
    let Credential::OAuth(saved) = vault.load(account.id).unwrap().unwrap() else {
        panic!("oauth")
    };
    assert_eq!(saved.access_token, old);
}

const MIXED_PAGE: &str = r#"{"totalUsageEventsCount":7,"usageEventsDisplay":[
 {"timestamp":"1793000000000","model":"claude-4.6-opus-high-thinking","kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":124.73,"isChargeable":true,"tokenUsage":{"inputTokens":3,"outputTokens":20525,"cacheWriteTokens":112151}},
 {"timestamp":"1793000001000","model":"composer-2","kind":"USAGE_EVENT_KIND_INCLUDED_IN_PRO","isChargeable":false,"chargedCents":40,"tokenUsage":{"inputTokens":3438,"outputTokens":76}},
 {"timestamp":"1793000002000","model":"gpt-5","kind":"USAGE_EVENT_KIND_USAGE_BASED","isChargeable":false,"chargedCents":50},
 {"timestamp":"1793000003000","model":"gpt-5","kind":"USAGE_EVENT_KIND_ERRORED_NOT_CHARGED"},
 {"timestamp":"1793000003500","model":"composer-2","kind":"USAGE_EVENT_KIND_FREE_CREDIT","isChargeable":false,"chargedCents":40},
 {"timestamp":1793000004000,"model":"composer-2","kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":"3.5"},
 {"timestamp":"1793000005000","model":"composer-2","kind":"USAGE_EVENT_KIND_USAGE_BASED","isChargeable":true}
]}"#;

#[test]
fn only_on_demand_events_count_what_cursor_charged() {
    let page = events::parse_page(MIXED_PAGE.as_bytes()).unwrap();
    assert_eq!(page.total, 7);
    let spends: Vec<f64> = page.events.iter().map(|event| event.spend).collect();
    let expected: Vec<f64> = [124.73, 0.0, 0.0, 0.0, 0.0, 3.5, 0.0]
        .into_iter()
        .map(|cents| cents / 100.0)
        .collect();
    assert_eq!(spends, expected);
    assert_eq!(
        page.events[0].date,
        DateTime::from_timestamp(1_793_000_000, 0).unwrap()
    );
    assert_eq!(
        page.events.last().unwrap().date,
        DateTime::from_timestamp(1_793_000_005, 0).unwrap()
    );
    let tokens: Vec<f64> = page.events.iter().map(|event| event.tokens).collect();
    assert_eq!(tokens, vec![132_679.0, 3_514.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(page.events[0].input_tokens, 3.0);
    assert_eq!(page.events[0].cache_write_tokens, 112_151.0);
    assert_eq!(page.events[0].output_tokens, 20_525.0);
    let mut events = page.events.clone();
    events[0].model = None;
    let day = events::day(&events);
    assert_eq!(day.by_model["composer-2"].tokens, 3_514.0);
    assert_eq!(day.tokens, 136_193.0);
    assert_eq!(day.events, 7.0);
    assert_eq!(events::day(&[]).events, 0.0);
}

#[test]
fn an_empty_page_keeps_its_count_and_invalid_pages_are_rejected() {
    assert!(events::parse_page(br#"{"totalUsageEventsCount":0}"#)
        .unwrap()
        .events
        .is_empty());
    for body in [
        r#"{"error":"unauthorized"}"#,
        r#"{"usageEventsDisplay":[]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":5}]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"timestamp":"1","kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":-5}]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"timestamp":"1","kind":"USAGE_EVENT_KIND_USAGE_BASED","chargedCents":"$0.05"}]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"timestamp":"1","tokenUsage":{"inputTokens":-1}}]}"#,
        r#"{"totalUsageEventsCount":1,"usageEventsDisplay":[{"timestamp":"1","tokenUsage":"lots"}]}"#,
    ] {
        assert!(events::parse_page(body.as_bytes()).is_none(), "{body}");
    }
}

fn history_http(events: Arc<Mutex<Option<u16>>>, retry_after: Option<&'static str>) -> Script {
    let token_cookie = format!(
        "WorkosCursorSessionToken=user_fake%3A%3A{}",
        jwt(r#"{"sub":"auth0|user_fake"}"#)
    );
    Script {
        requests: Mutex::new(vec![]),
        handle: Mutex::new(Box::new(move |request: &HttpRequest| {
            let path = url::Url::parse(&request.url).unwrap().path().to_string();
            if path.contains("get-filtered-usage-events") {
                assert_eq!(request.method, "POST");
                assert_eq!(
                    request.headers.get("Cookie").map(String::as_str),
                    Some(token_cookie.as_str())
                );
                assert_eq!(
                    request.headers.get("Origin").map(String::as_str),
                    Some("https://cursor.com")
                );
                if let Some(status) = *events.lock().unwrap() {
                    let mut headers = BTreeMap::new();
                    if let Some(retry) = retry_after {
                        headers.insert("retry-after".into(), retry.into());
                    }
                    return Ok(HttpResponse {
                        status,
                        headers,
                        body: Vec::new(),
                    });
                }
                let body: Value = serde_json::from_slice(request.body.as_deref().unwrap()).unwrap();
                let start: i64 = body["startDate"].as_str().unwrap().parse().unwrap();
                let end: i64 = body["endDate"].as_str().unwrap().parse().unwrap();
                assert_eq!(body["page"], 1);
                let timestamp = (start + 3_600_000).min(end);
                let page = format!(
                    r#"{{"totalUsageEventsCount":2,"usageEventsDisplay":[{{"timestamp":"{timestamp}","model":"claude-4.6-opus","kind":"USAGE_EVENT_KIND_USAGE_BASED","isChargeable":true,"chargedCents":125,"tokenUsage":{{"inputTokens":3,"outputTokens":200}}}},{{"timestamp":"{timestamp}","model":"composer-2","kind":"USAGE_EVENT_KIND_INCLUDED_IN_PRO","isChargeable":false,"chargedCents":999,"tokenUsage":{{"inputTokens":3000,"outputTokens":100}}}}]}}"#
                );
                return Ok(ok(&page));
            }
            Ok(match path.as_str() {
                "/aiserver.v1.DashboardService/GetCurrentPeriodUsage" => ok(INDIVIDUAL),
                "/aiserver.v1.DashboardService/GetPlanInfo" => ok(PLAN_ULTRA),
                _ => http_status(404, ""),
            })
        })),
    }
}

fn history_account() -> (Account, MemoryVault) {
    let account = Account::new(ProviderKind::Cursor, "Cursor", None);
    let mut tokens = OAuthTokens::new(jwt(r#"{"sub":"auth0|user_fake"}"#));
    tokens.refresh_token = Some("refresh".into());
    let vault = MemoryVault::with(account.id, Credential::OAuth(tokens));
    (account, vault)
}

fn history_fetch(
    account: &Account,
    vault: &MemoryVault,
    http: &Script,
    state: &mut ProviderState,
    now: DateTime<Utc>,
) -> ProviderUsage {
    let mut ctx = FetchContext {
        account,
        http,
        vault,
        state,
        now,
        force: true,
    };
    provider().fetch(&mut ctx).unwrap()
}

fn history_figure(usage: &ProviderUsage, label: &str) -> Option<(f64, f64)> {
    usage
        .lines
        .iter()
        .any(|line| line.label() == label)
        .then(|| {
            let values = figures(usage, label);
            (values[0].number, values[1].number)
        })
}

#[test]
fn history_rows_count_on_demand_charges_and_reread_today_and_yesterday() {
    let now = DateTime::from_timestamp(1_793_000_000, 0).unwrap();
    let (account, vault) = history_account();
    let http = history_http(Arc::new(Mutex::new(None)), None);
    let mut state = ProviderState::default();
    let usage = history_fetch(&account, &vault, &http, &mut state, now);
    assert_eq!(history_figure(&usage, "Today"), Some((1.25, 3303.0)));
    assert_eq!(history_figure(&usage, "Yesterday"), Some((1.25, 3303.0)));
    assert_eq!(
        history_figure(&usage, "Last 30 Days"),
        Some((37.5, 99_090.0))
    );
    let first = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.contains("get-filtered-usage-events"))
        .count();
    assert_eq!(first, 30);
    let usage = history_fetch(&account, &vault, &http, &mut state, now);
    let second = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.contains("get-filtered-usage-events"))
        .count();
    assert_eq!(second - first, 2);
    assert_eq!(
        history_figure(&usage, "Last 30 Days"),
        Some((37.5, 99_090.0))
    );
}

#[test]
fn a_failed_reread_drops_today_and_yesterday_and_keeps_earlier_days() {
    let now = DateTime::from_timestamp(1_793_000_000, 0).unwrap();
    let (account, vault) = history_account();
    let mode = Arc::new(Mutex::new(Some(500u16)));
    let http = history_http(mode.clone(), None);
    let mut state = ProviderState::default();
    let usage = history_fetch(&account, &vault, &http, &mut state, now);
    assert_eq!(meter(&usage, "Total usage").0, 25.0);
    assert!(history_figure(&usage, "Today").is_none());
    *mode.lock().unwrap() = None;
    history_fetch(&account, &vault, &http, &mut state, now);
    *mode.lock().unwrap() = Some(503);
    let usage = history_fetch(&account, &vault, &http, &mut state, now);
    assert!(history_figure(&usage, "Today").is_none());
    assert!(history_figure(&usage, "Yesterday").is_none());
    assert_eq!(history_figure(&usage, "Last 30 Days").unwrap().0, 35.0);
}

#[test]
fn a_rate_limit_stops_the_sweep_and_waits() {
    let now = DateTime::from_timestamp(1_793_000_000, 0).unwrap();
    for (retry, wait) in [(Some("120"), 120.0), (None, 3600.0)] {
        let (account, vault) = history_account();
        let mode = Arc::new(Mutex::new(Some(429u16)));
        let http = history_http(mode.clone(), retry);
        let mut state = ProviderState::default();
        history_fetch(&account, &vault, &http, &mut state, now);
        let first = http
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.contains("get-filtered-usage-events"))
            .count();
        assert!(first <= 4, "{first}");
        *mode.lock().unwrap() = None;
        history_fetch(
            &account,
            &vault,
            &http,
            &mut state,
            now + Duration::milliseconds(((wait - 1.0) * 1000.0) as i64),
        );
        assert_eq!(
            http.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.url.contains("get-filtered-usage-events"))
                .count(),
            first
        );
        let usage = history_fetch(
            &account,
            &vault,
            &http,
            &mut state,
            now + Duration::milliseconds(((wait + 1.0) * 1000.0) as i64),
        );
        let after = http
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.contains("get-filtered-usage-events"))
            .count();
        assert_eq!(after - first, 30);
        assert_eq!(history_figure(&usage, "Today"), Some((1.25, 3303.0)));
        assert_eq!(
            history_figure(&usage, "Last 30 Days"),
            Some((37.5, 99_090.0))
        );
    }
}

#[test]
fn cursor_login_polls_for_tokens_and_names_the_profile() {
    let access = jwt(r#"{"sub":"auth0|user_abc"}"#);
    let polled = access.clone();
    let http = Script {
        requests: Mutex::new(vec![]),
        handle: Mutex::new(Box::new(move |request: &HttpRequest| {
            Ok(if request.url.contains("/api/auth/me") {
                ok(r#"{"email":"dev@example.com","name":"Dev"}"#)
            } else {
                ok(&format!(
                    r#"{{"accessToken":"{polled}","refreshToken":"cursor-refresh"}}"#
                ))
            })
        })),
    };
    let prompt = Lines {
        lines: Mutex::new(vec![]),
        shown: Mutex::new(vec![]),
    };
    let (label, credential) = login(&crate::provider::LoginMethod::Default)
        .login(&prompt, &http)
        .unwrap();
    assert_eq!(label, "dev@example.com");
    let LoginPrompt::Browser {
        url,
        accepts_pasted_code,
    } = &prompt.shown.lock().unwrap()[0]
    else {
        panic!("browser")
    };
    assert!(!accepts_pasted_code);
    let url = url::Url::parse(url).unwrap();
    assert_eq!(url.host_str(), Some("cursor.com"));
    assert_eq!(url.path(), "/loginDeepControl");
    let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query.get("mode").map(String::as_str), Some("login"));
    assert_eq!(query.get("redirectTarget").map(String::as_str), Some("cli"));
    let poll = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.url.contains("/auth/poll"))
        .unwrap()
        .clone();
    let poll = url::Url::parse(&poll.url).unwrap();
    let poll_query: BTreeMap<_, _> = poll.query_pairs().into_owned().collect();
    assert!(!poll_query.get("uuid").unwrap().is_empty());
    assert!(!poll_query.get("verifier").unwrap().is_empty());
    let profile = http
        .requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.url.contains("/api/auth/me"))
        .unwrap()
        .clone();
    assert_eq!(
        profile.headers.get("Cookie").map(String::as_str),
        Some(format!("WorkosCursorSessionToken=user_abc%3A%3A{access}").as_str())
    );
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(
        tokens
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("cursor:user_abc")
    );
}

#[test]
fn cursor_login_keeps_the_default_name_when_the_profile_read_fails() {
    let access = jwt(r#"{"sub":"auth0|user_abc"}"#);
    let http = Script {
        requests: Mutex::new(vec![]),
        handle: Mutex::new(Box::new(move |request: &HttpRequest| {
            Ok(if request.url.contains("/api/auth/me") {
                http_status(500, "{}")
            } else {
                ok(&format!(
                    r#"{{"accessToken":"{access}","refreshToken":"cursor-refresh"}}"#
                ))
            })
        })),
    };
    let prompt = Lines {
        lines: Mutex::new(vec![]),
        shown: Mutex::new(vec![]),
    };
    let (label, credential) = login(&crate::provider::LoginMethod::Default)
        .login(&prompt, &http)
        .unwrap();
    assert_eq!(label, "Cursor Account");
    let Credential::OAuth(tokens) = credential else {
        panic!("oauth")
    };
    assert_eq!(
        tokens
            .extras
            .get(OAuthTokens::IDENTITY_KEY)
            .map(String::as_str),
        Some("cursor:user_abc")
    );
    let _ = LoginError::Cancelled;
}
