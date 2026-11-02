//! Port of `OpenRouterUsageClient`, `OpenRouterActivitySource`, and `refreshOpenRouter`.
//! A standard key reports its own spend. A management key sums every listed key, then reads
//! `/api/v1/activity` for yesterday and the prior 30 days. Credits are the account balance; a
//! workspace-scoped management key is refused that endpoint with a 403 and simply has no balance.

mod history;
mod mapper;

use crate::error::ProviderError;
use crate::http::{HttpClient, HttpRequest, HttpResponse};
use crate::kind::ProviderKind;
use crate::metric::{MetricLine, ProviderUsage};
use crate::provider::{FetchContext, Login, LoginMethod, Provider};

const KEY_URL: &str = "https://openrouter.ai/api/v1/key";
const CREDITS_URL: &str = "https://openrouter.ai/api/v1/credits";
const KEYS_URL: &str = "https://openrouter.ai/api/v1/keys";
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 50;

pub fn provider() -> Box<dyn Provider> {
    Box::new(OpenRouterProvider)
}

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(crate::providers::ApiKeyLogin(ProviderKind::OpenRouter))
}

struct OpenRouterProvider;

impl Provider for OpenRouterProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        self.read(ctx).map_err(rejected)
    }
}

impl OpenRouterProvider {
    fn read(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        let api_key = ctx.api_key()?;
        let info = mapper::key(&get(ctx.http, KEY_URL, &api_key)?, ctx.now)?;
        let mut lines = Vec::new();
        let breakdowns = if info.management {
            lines.extend(credits(ctx.http, &api_key)?);
            let spend = listed_spend(ctx.http, &api_key)?;
            let (rows, breakdowns) = history::lines(ctx, &api_key, &spend, true);
            lines.extend(rows);
            breakdowns
        } else {
            let spend = info.spend.ok_or(ProviderError::InvalidResponse)?;
            let (rows, breakdowns) = history::lines(ctx, &api_key, &spend, false);
            lines.extend(rows);
            breakdowns
        };
        lines.extend(info.lines);
        Ok(ProviderUsage {
            breakdowns,
            plan: info.plan,
            lines,
            notice: None,
        })
    }
}

fn credits(http: &dyn HttpClient, api_key: &str) -> Result<Vec<MetricLine>, ProviderError> {
    let response = send(http, CREDITS_URL, api_key)?;
    if response.status == 403 {
        return Ok(Vec::new());
    }
    ProviderError::require_success(&response, ProviderKind::OpenRouter)?;
    mapper::credits_lines(&response.body)
}

fn listed_spend(http: &dyn HttpClient, api_key: &str) -> Result<mapper::Spend, ProviderError> {
    let mut total = mapper::Spend::zero();
    for page in 0..MAX_PAGES {
        let url = format!(
            "{KEYS_URL}?include_disabled=true&include_expired=true&offset={}",
            page * PAGE_SIZE
        );
        let keys = mapper::keys_page(&get(http, &url, api_key)?)?;
        let count = keys.len();
        for key in &keys {
            total.add(key);
        }
        if count < PAGE_SIZE {
            return Ok(total);
        }
    }
    Err(ProviderError::InvalidResponse)
}

fn get(http: &dyn HttpClient, url: &str, api_key: &str) -> Result<Vec<u8>, ProviderError> {
    let response = send(http, url, api_key)?;
    ProviderError::require_success(&response, ProviderKind::OpenRouter)?;
    Ok(response.body)
}

fn send(http: &dyn HttpClient, url: &str, api_key: &str) -> Result<HttpResponse, ProviderError> {
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
            ProviderKind::OpenRouter.display_name()
        )),
        other => other,
    }
}

#[cfg(test)]
mod tests;
