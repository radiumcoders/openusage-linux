//! Port of `OpenCodeUsageClient` and the OpenCode branch of `APIKeyLogin`.

mod mapper;

use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Login, LoginMethod, Provider};

const USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";

pub fn provider() -> Box<dyn Provider> {
    Box::new(OpenCodeProvider)
}

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(super::ApiKeyLogin(ProviderKind::OpenCode))
}

struct OpenCodeProvider;

impl Provider for OpenCodeProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        self.read(ctx).map_err(rejected)
    }
}

impl OpenCodeProvider {
    fn read(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        let key = ctx.api_key()?;
        let response = fetch_usage(ctx.http, &key)?;
        if response.status == 403
            && mapper::error_type(&response.body).as_deref() == Some("EntitlementError")
        {
            return Err(ProviderError::Unavailable(
                "No OpenCode Go subscription on this account.".into(),
            ));
        }
        ProviderError::require_success(&response, ProviderKind::OpenCode)?;
        Ok(ProviderUsage {
            plan: Some(mapper::PLAN.into()),
            lines: mapper::meter_lines(&response, ctx.now)?,
            notice: None,
        })
    }
}

fn fetch_usage(http: &dyn HttpClient, api_key: &str) -> Result<HttpResponse, ProviderError> {
    http.send(
        HttpRequest::get(USAGE_URL)
            .bearer(api_key)
            .header("Accept", "application/json"),
    )
}

fn rejected(error: ProviderError) -> ProviderError {
    match error {
        ProviderError::SignInExpired(_) => ProviderError::KeyRejected(format!(
            "{} rejected the API key.",
            ProviderKind::OpenCode.display_name()
        )),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Account, Credential};
    use crate::provider::ProviderState;
    use crate::vault::MemoryVault;

    struct Script(u16, &'static str);

    impl HttpClient for Script {
        fn send(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
            assert_eq!(request.url, USAGE_URL);
            assert_eq!(
                request.headers.get("Authorization").map(String::as_str),
                Some("Bearer key")
            );
            Ok(HttpResponse {
                status: self.0,
                headers: Default::default(),
                body: self.1.as_bytes().to_vec(),
            })
        }
    }

    fn fetch(http: &dyn HttpClient) -> Result<ProviderUsage, ProviderError> {
        let account = Account::new(ProviderKind::OpenCode, "OpenCode", None);
        let vault = MemoryVault::with(account.id, Credential::ApiKey("key".into()));
        let mut state = ProviderState::default();
        let mut ctx = FetchContext {
            account: &account,
            http,
            vault: &vault,
            state: &mut state,
            now: chrono::Utc::now(),
            force: true,
        };
        provider().fetch(&mut ctx)
    }

    #[test]
    fn refresh_tags_the_account_with_the_go_plan() {
        let body = r#"{"usage":{"rolling":{"percent":12,"resetsAt":"2026-07-12T13:30:00Z"},"weekly":{"percent":8,"resetsAt":"2026-07-13T00:00:00Z"},"monthly":{"percent":40,"resetsAt":"2026-08-04T11:18:32Z"}}}"#;
        let usage = fetch(&Script(200, body)).unwrap();
        assert_eq!(usage.plan.as_deref(), Some("Go"));
        assert_eq!(
            usage
                .lines
                .iter()
                .map(|line| line.label())
                .collect::<Vec<_>>(),
            ["Session", "Weekly", "Monthly"]
        );
    }

    #[test]
    fn a_missing_subscription_is_not_a_rejected_key() {
        let body = r#"{"type":"error","error":{"type":"EntitlementError","message":"OpenCode Go subscription required."}}"#;
        assert_eq!(
            fetch(&Script(403, body)),
            Err(ProviderError::Unavailable(
                "No OpenCode Go subscription on this account.".into()
            ))
        );
        assert_eq!(
            fetch(&Script(
                401,
                r#"{"type":"error","error":{"type":"AuthError","message":"Unauthorized"}}"#
            )),
            Err(ProviderError::KeyRejected(
                "OpenCode rejected the API key.".into()
            ))
        );
    }
}
