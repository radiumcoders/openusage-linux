//! One module per provider, each a port of `Sources/OpenUsage/Providers/<Name>/` plus its sign-in.
//! `provider(kind)` and `login(kind, method)` are the only entry points the app uses.

use crate::error::{LoginError, ProviderError};
use crate::http::HttpClient;
use crate::kind::ProviderKind;
use crate::login::LoginPrompter;
use crate::metric::ProviderUsage;
use crate::provider::{FetchContext, Login, LoginMethod, Provider};
use crate::account::Credential;

/// A provider not ported yet: its reads and sign-ins fail with a clear message.
struct NotPorted(ProviderKind);

impl Provider for NotPorted {
    fn fetch(&self, _ctx: &mut FetchContext) -> Result<ProviderUsage, ProviderError> {
        Err(ProviderError::Unavailable(format!("{} isn't supported in this build yet.", self.0.display_name())))
    }
}

impl Login for NotPorted {
    fn login(&self, _prompter: &dyn LoginPrompter, _http: &dyn HttpClient) -> Result<(String, Credential), LoginError> {
        Err(LoginError::Failed(format!("{} sign-in isn't supported in this build yet.", self.0.display_name())))
    }
}

pub fn provider(kind: ProviderKind) -> Box<dyn Provider> {
    match kind {
        _ => Box::new(NotPorted(kind)),
    }
}

pub fn login(kind: ProviderKind, method: &LoginMethod) -> Box<dyn Login> {
    match (kind, method) {
        _ => Box::new(NotPorted(kind)),
    }
}
