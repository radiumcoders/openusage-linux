//! Copilot personal quotas and organization billing. Ports `Providers/Copilot/` and both Copilot sign-ins.

mod client;
mod login;
mod mapper;

use crate::account::Credential;
use crate::error::ProviderError;
use crate::kind::ProviderKind;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Provider};

pub fn login(method: &crate::provider::LoginMethod) -> Box<dyn crate::provider::Login> {
    login::dispatch(method)
}

pub fn provider() -> Box<dyn Provider> {
    Box::new(CopilotProvider)
}

struct CopilotProvider;

impl Provider for CopilotProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        if let Some(organization) = ctx.account.organization.clone() {
            let Credential::ApiKey(token) = ctx.credential()? else {
                return Err(ProviderError::InvalidResponse);
            };
            let lines = client::organization_usage(ctx.http, &organization, &token)?;
            return Ok(ProviderUsage {
                breakdowns: Default::default(),
                plan: None,
                lines,
                notice: None,
            });
        }
        let Credential::OAuth(tokens) = ctx.credential()? else {
            return Err(ProviderError::InvalidResponse);
        };
        let response = client::fetch_usage(ctx.http, &tokens.access_token)?;
        ProviderError::require_success(&response, ProviderKind::Copilot)?;
        mapper::map_response(&response.body)
    }
}

#[cfg(test)]
mod tests;
