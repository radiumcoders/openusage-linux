use std::fmt;

use crate::http::HttpResponse;
use crate::kind::ProviderKind;

/// A provider read's failure, shown to the user next to the account. Messages match the Mac app,
/// pointing at the `openusage` commands where the Mac points at its window.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderError {
    SignInExpired(ProviderKind),
    ConnectionFailed,
    InvalidResponse,
    RequestFailed(u16),
    RateLimited { retry_after_seconds: Option<f64> },
    /// The message is shown as is.
    Unavailable(String),
    /// The provider refused an API key or token; the message names it ("OpenRouter rejected the API key.").
    KeyRejected(String),
    /// The credential is missing from the keyring.
    MissingCredential,
    /// The keyring itself failed.
    Vault(String),
    /// A server between OpenUsage and the provider, such as a CLIProxyAPI hub, can't be reached.
    /// The message is shown as is. Backoff treats it like a connection failure.
    Unreachable(String),
}

impl ProviderError {
    /// 401, or 403 that isn't a rate limit, means the sign-in or key was refused.
    pub fn is_auth_failure(response: &HttpResponse) -> bool {
        response.status == 401 || (response.status == 403 && !Self::is_rate_limit(response))
    }

    /// GitHub sends rate limits as 403 with `x-ratelimit-remaining: 0` or `retry-after`.
    pub fn is_rate_limit(response: &HttpResponse) -> bool {
        response.status == 429
            || (response.status == 403
                && (response.header("x-ratelimit-remaining") == Some("0") || response.header("retry-after").is_some()))
    }

    /// Fails the read unless the response is a 2xx: an auth failure is an expired sign-in.
    pub fn require_success(response: &HttpResponse, kind: ProviderKind) -> Result<(), ProviderError> {
        if Self::is_auth_failure(response) {
            return Err(Self::SignInExpired(kind));
        }
        if !(200..300).contains(&response.status) {
            return Err(Self::failed(response));
        }
        Ok(())
    }

    pub fn failed(response: &HttpResponse) -> ProviderError {
        if Self::is_rate_limit(response) {
            ProviderError::RateLimited { retry_after_seconds: response.retry_after_seconds() }
        } else {
            ProviderError::RequestFailed(response.status)
        }
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Self::RateLimited { .. })
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SignInExpired(kind) => write!(
                f,
                "{} sign-in expired. Sign in again with `openusage add {}` to keep reading usage.",
                kind.display_name(),
                kind.raw()
            ),
            Self::ConnectionFailed => f.write_str("Connection failed. Check your internet connection and try again."),
            Self::InvalidResponse => f.write_str("The provider returned an invalid response."),
            Self::RequestFailed(status) => write!(f, "The provider request failed (HTTP {status})."),
            Self::RateLimited { .. } => f.write_str("The provider asked OpenUsage to wait before asking again."),
            Self::Unavailable(message) => f.write_str(message),
            Self::KeyRejected(message) => write!(
                f,
                "{message} Remove the account and add it again with a new one (`openusage remove`, `openusage add`)."
            ),
            Self::MissingCredential => f.write_str(
                "This account's sign-in is missing from the keyring. Remove the account and add it again with `openusage add`.",
            ),
            Self::Vault(reason) => write!(f, "OpenUsage couldn't use the keyring for this account ({reason})."),
            Self::Unreachable(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ProviderError {}

/// A sign-in's failure.
#[derive(Debug, Clone, PartialEq)]
pub enum LoginError {
    Cancelled,
    InvalidResponse,
    RequestFailed(u16),
    InvalidState,
    InvalidCode,
    InvalidCallback,
    AuthorizationFailed(String),
    ListenerFailed(String),
    Expired,
    Failed(String),
    Provider(ProviderError),
}

impl fmt::Display for LoginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Sign-in was cancelled."),
            Self::InvalidResponse => f.write_str("The sign-in response was invalid."),
            Self::RequestFailed(status) => write!(f, "Sign-in failed with HTTP {status}."),
            Self::InvalidState => f.write_str("Sign-in state did not match. Try again."),
            Self::InvalidCode => f.write_str("That code isn't valid. Paste the whole code and try again."),
            Self::InvalidCallback => f.write_str("The browser sign-in callback was invalid."),
            Self::AuthorizationFailed(reason) => write!(f, "Sign-in was not authorized ({reason})."),
            Self::ListenerFailed(reason) => write!(f, "OpenUsage couldn't listen for the sign-in callback ({reason})."),
            Self::Expired => f.write_str("The sign-in code expired. Try again."),
            Self::Failed(message) => f.write_str(message),
            Self::Provider(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for LoginError {}

impl From<ProviderError> for LoginError {
    fn from(error: ProviderError) -> Self {
        LoginError::Provider(error)
    }
}
