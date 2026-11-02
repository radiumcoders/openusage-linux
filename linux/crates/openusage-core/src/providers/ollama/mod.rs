//! Port of `OllamaUsageClient`, Ollama history, and the account-email naming in `APIKeyLogin`.

mod history;
mod mapper;

use serde_json::Value;

use crate::account::Credential;
use crate::error::{LoginError, ProviderError};
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::log;
use crate::login::LoginPrompter;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Login, LoginMethod, Provider};
use crate::providers::ApiKeyLogin;
use crate::refresh::SECONDARY_READ_INTERVAL_SECONDS;

const BALANCE_URL: &str = "https://ollama.com/api/balance";
const ACCOUNT_URL: &str = "https://ollama.com/api/me";
const ACCOUNT: &str = "account_body";

pub fn provider() -> Box<dyn Provider> {
    Box::new(OllamaProvider)
}

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(OllamaLogin)
}

struct OllamaProvider;

impl Provider for OllamaProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        self.read(ctx).map_err(rejected)
    }
}

impl OllamaProvider {
    fn read(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        let key = ctx.api_key()?;
        let balance = send(ctx.http, "GET", BALANCE_URL, &key)?;
        ProviderError::require_success(&balance, ProviderKind::Ollama)?;
        if ctx
            .state
            .is_due(ACCOUNT, SECONDARY_READ_INTERVAL_SECONDS, ctx.now, ctx.force)
        {
            if let Ok(response) = send(ctx.http, "POST", ACCOUNT_URL, &key) {
                if response.is_success() {
                    ctx.state
                        .set(ACCOUNT, &response.json().unwrap_or(Value::Null));
                }
            }
            ctx.state.mark_read(ACCOUNT, ctx.now);
        }
        let account = ctx.state.get::<Value>(ACCOUNT);
        let mut usage = mapper::map(&balance.body, account.as_ref())?;
        history::append(ctx, &key, &mut usage.lines);
        Ok(usage)
    }
}

struct OllamaLogin;

impl Login for OllamaLogin {
    fn login(
        &self,
        prompter: &dyn LoginPrompter,
        http: &dyn HttpClient,
    ) -> Result<(String, Credential), LoginError> {
        let (key, _, state) = ApiKeyLogin(ProviderKind::Ollama).ask_and_check(prompter, http)?;
        let email = state.get::<Value>(ACCOUNT).as_ref().and_then(mapper::email);
        let label = match email {
            Some(email) => email,
            None => {
                log::warn(
                    "auth:ollama",
                    "account read reported no email, naming the account Ollama",
                );
                ProviderKind::Ollama.display_name().to_string()
            }
        };
        Ok((label, Credential::ApiKey(key)))
    }
}

fn send(
    http: &dyn HttpClient,
    method: &str,
    url: &str,
    api_key: &str,
) -> Result<HttpResponse, ProviderError> {
    http.send(
        HttpRequest::new(method, url)
            .bearer(api_key)
            .header("Accept", "application/json"),
    )
}

fn rejected(error: ProviderError) -> ProviderError {
    match error {
        ProviderError::SignInExpired(_) => ProviderError::KeyRejected(format!(
            "{} rejected the API key.",
            ProviderKind::Ollama.display_name()
        )),
        other => other,
    }
}

#[cfg(test)]
mod tests;
