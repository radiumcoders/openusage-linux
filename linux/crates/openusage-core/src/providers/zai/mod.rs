//! Port of `ZAIUsageClient`, `ZAIUsageMapper`, and the Z.ai branch of `APIKeyLogin`.

mod mapper;

use serde_json::Value;

use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Login, LoginMethod, Provider};
use crate::refresh::SECONDARY_READ_INTERVAL_SECONDS;

const QUOTA_URL: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const SUBSCRIPTION_URL: &str = "https://api.z.ai/api/biz/subscription/list";
const PLAN: &str = "subscription";

pub fn provider() -> Box<dyn Provider> {
    Box::new(ZaiProvider)
}

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(super::ApiKeyLogin(ProviderKind::Zai))
}

struct ZaiProvider;

impl Provider for ZaiProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        self.read(ctx).map_err(rejected)
    }
}

impl ZaiProvider {
    fn read(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        let key = ctx.api_key()?;
        let quota = get(ctx.http, QUOTA_URL, &key)?;
        ProviderError::require_success(&quota, ProviderKind::Zai)?;
        if ctx
            .state
            .is_due(PLAN, SECONDARY_READ_INTERVAL_SECONDS, ctx.now, ctx.force)
        {
            if let Ok(response) = get(ctx.http, SUBSCRIPTION_URL, &key) {
                if response.is_success() {
                    ctx.state.set(PLAN, &response.json().unwrap_or(Value::Null));
                }
            }
            ctx.state.mark_read(PLAN, ctx.now);
        }
        let subscription = ctx.state.get::<Value>(PLAN);
        mapper::map(&quota.body, subscription.as_ref())
    }
}

fn get(http: &dyn HttpClient, url: &str, api_key: &str) -> Result<HttpResponse, ProviderError> {
    http.send(
        HttpRequest::get(url)
            .bearer(api_key)
            .header("Accept", "application/json"),
    )
}

fn rejected(error: ProviderError) -> ProviderError {
    match error {
        ProviderError::SignInExpired(_) => ProviderError::KeyRejected(format!(
            "{} rejected the API key.",
            ProviderKind::Zai.display_name()
        )),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::account::{Account, Credential};
    use crate::metric::MetricLine;
    use crate::provider::ProviderState;
    use crate::vault::MemoryVault;

    struct Route {
        seen: Mutex<Vec<HttpRequest>>,
    }

    impl HttpClient for Route {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
            self.seen.lock().unwrap().push(request.clone());
            let body = if request.url.contains("/quota/limit") {
                r#"{"code":200,"success":true,"data":{"limits":[
                    {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":17,"nextResetTime":1782724971179},
                    {"type":"TIME_LIMIT","usage":1000,"currentValue":2}
                ]}}"#
            } else {
                r#"{"data":[{"productName":"GLM Coding Pro"}]}"#
            };
            Ok(HttpResponse {
                status: 200,
                headers: Default::default(),
                body: body.as_bytes().to_vec(),
            })
        }
    }

    struct HttpOnce(u16, &'static [u8]);

    impl HttpClient for HttpOnce {
        fn send(&self, _request: HttpRequest) -> Result<HttpResponse, ProviderError> {
            Ok(HttpResponse {
                status: self.0,
                headers: Default::default(),
                body: self.1.to_vec(),
            })
        }
    }

    #[test]
    fn fetch_reads_quota_and_keeps_the_plan_for_an_hour() {
        let account = Account::new(ProviderKind::Zai, "Z.ai", None);
        let vault = MemoryVault::with(account.id, Credential::ApiKey("zai-key".into()));
        let http = Route {
            seen: Mutex::new(vec![]),
        };
        let provider = provider();
        let mut state = ProviderState::default();
        let now = Utc.with_ymd_and_hms(2026, 8, 13, 12, 0, 0).unwrap();
        let usage = {
            let mut ctx = FetchContext {
                account: &account,
                http: &http,
                vault: &vault,
                state: &mut state,
                now,
                force: false,
            };
            provider.fetch(&mut ctx).unwrap()
        };
        assert_eq!(usage.plan.as_deref(), Some("GLM Coding Pro"));
        assert_eq!(
            usage
                .lines
                .iter()
                .map(MetricLine::label)
                .collect::<Vec<_>>(),
            ["Session", "Web Searches"]
        );
        let seen = http.seen.lock().unwrap();
        assert!(seen[0].url.contains("/quota/limit"));
        assert!(seen[1].url.contains("/subscription/list"));
        assert_eq!(
            seen[0].headers.get("Authorization").map(String::as_str),
            Some("Bearer zai-key")
        );
        drop(seen);

        let later = now + chrono::Duration::minutes(5);
        let again = {
            let mut ctx = FetchContext {
                account: &account,
                http: &http,
                vault: &vault,
                state: &mut state,
                now: later,
                force: false,
            };
            provider.fetch(&mut ctx).unwrap()
        };
        assert_eq!(again.plan.as_deref(), Some("GLM Coding Pro"));
        assert_eq!(
            http.seen.lock().unwrap().len(),
            3,
            "the plan stays read for an hour"
        );
    }

    #[test]
    fn a_refused_key_and_a_missing_plan_use_the_provider_messages() {
        let account = Account::new(ProviderKind::Zai, "Z.ai", None);
        let vault = MemoryVault::with(account.id, Credential::ApiKey("bad".into()));
        let refused = HttpOnce(401, br#"{"code":401,"success":false}"#);
        let mut state = ProviderState::default();
        let mut ctx = FetchContext {
            account: &account,
            http: &refused,
            vault: &vault,
            state: &mut state,
            now: Utc::now(),
            force: true,
        };
        assert_eq!(
            provider().fetch(&mut ctx),
            Err(ProviderError::KeyRejected(
                "Z.ai rejected the API key.".into()
            ))
        );

        let no_plan = HttpOnce(
            200,
            r#"{"code":500,"msg":"当前用户不存在coding plan","success":false}"#.as_bytes(),
        );
        let mut state = ProviderState::default();
        let mut ctx = FetchContext {
            account: &account,
            http: &no_plan,
            vault: &vault,
            state: &mut state,
            now: Utc::now(),
            force: true,
        };
        assert_eq!(
            provider().fetch(&mut ctx),
            Err(ProviderError::Unavailable(
                "No active GLM Coding Plan. Subscribe at z.ai/subscribe to see usage.".into()
            ))
        );
    }
}
