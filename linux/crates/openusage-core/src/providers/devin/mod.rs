//! Devin quotas. Ports `Providers/Devin/`.

mod client;
mod login;
mod mapper;

use crate::account::Credential;
use crate::error::ProviderError;
use crate::kind::ProviderKind;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Login, LoginMethod, Provider};

pub fn provider() -> Box<dyn Provider> {
    Box::new(DevinProvider)
}

pub fn login(_method: &LoginMethod) -> Box<dyn Login> {
    Box::new(login::DevinLogin)
}

struct DevinProvider;

impl Provider for DevinProvider {
    fn fetch(&self, ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        let Credential::OAuth(tokens) = ctx.credential()? else {
            return Err(ProviderError::InvalidResponse);
        };
        let response = client::fetch_user_status(ctx.http, &tokens)?;
        ProviderError::require_success(&response, ProviderKind::Devin)?;
        mapper::map(&response.body)
    }
}

#[cfg(test)]
mod tests;
