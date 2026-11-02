use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kind::{AuthMethod, ProviderKind};

/// Swift's default `JSONEncoder` writes a `Date` as seconds since 2001-01-01. Accounts and keyring
/// items keep that encoding so the Swift edition's data reads unchanged.
pub mod swift_date {
    use chrono::{DateTime, TimeZone, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub const REFERENCE_UNIX: f64 = 978_307_200.0;

    pub fn to_seconds(date: &DateTime<Utc>) -> f64 {
        date.timestamp_millis() as f64 / 1000.0 - REFERENCE_UNIX
    }

    pub fn from_seconds(seconds: f64) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(((seconds + REFERENCE_UNIX) * 1000.0).round() as i64)
            .single()
            .unwrap_or_else(Utc::now)
    }

    pub fn serialize<S: Serializer>(date: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(to_seconds(date))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<DateTime<Utc>, D::Error> {
        Ok(from_seconds(f64::deserialize(deserializer)?))
    }

    pub mod option {
        use super::*;

        pub fn serialize<S: Serializer>(date: &Option<DateTime<Utc>>, serializer: S) -> Result<S::Ok, S::Error> {
            match date {
                Some(date) => serializer.serialize_some(&to_seconds(date)),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error> {
            Ok(Option::<f64>::deserialize(deserializer)?.map(from_seconds))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: Uuid,
    pub kind: ProviderKind,
    pub label: String,
    /// A Copilot organization account carries the organization whose billing its token reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    /// A CLIProxyAPI hub link, kept as written so an account mirrored by the Mac app round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub: Option<serde_json::Value>,
    #[serde(with = "swift_date")]
    pub added_at: DateTime<Utc>,
}

impl Account {
    pub fn new(kind: ProviderKind, label: impl Into<String>, organization: Option<String>) -> Self {
        Self { id: Uuid::new_v4(), kind, label: label.into(), organization, hub: None, added_at: Utc::now() }
    }

    /// `codex@1a2b3c4d`: the id the limits JSON, the CLI, and the layout use.
    pub fn card_id(&self) -> String {
        let id = self.id.to_string().to_lowercase();
        format!("{}@{}", self.kind.raw(), &id[..8])
    }

    pub fn auth_method(&self) -> AuthMethod {
        if self.organization.is_some() {
            AuthMethod::OrganizationToken
        } else {
            self.kind.auth_methods().remove(0)
        }
    }

    /// "ChatGPT · Work", or just the provider name when the label is the provider name.
    pub fn full_name(&self) -> String {
        let name = self.kind.display_name();
        if self.label.is_empty() || self.label == name {
            name.to_string()
        } else {
            format!("{name} · {}", self.label)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, with = "swift_date::option", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub extras: BTreeMap<String, String>,
}

impl OAuthTokens {
    pub const IDENTITY_KEY: &'static str = "account_identity";
    pub const REFRESH_MARGIN_SECONDS: i64 = 5 * 60;

    pub fn new(access_token: impl Into<String>) -> Self {
        Self { access_token: access_token.into(), refresh_token: None, expires_at: None, extras: BTreeMap::new() }
    }

    pub fn needs_refresh(&self, now: DateTime<Utc>) -> bool {
        match (&self.refresh_token, self.expires_at) {
            (Some(_), Some(expires_at)) => expires_at <= now + Duration::seconds(Self::REFRESH_MARGIN_SECONDS),
            _ => false,
        }
    }
}

/// One account's sign-in. Serialized the way Swift synthesizes `AccountCredential`:
/// `{"oauth":{"_0":{…}}}` or `{"apiKey":{"_0":"…"}}`.
#[derive(Debug, Clone, PartialEq)]
pub enum Credential {
    OAuth(OAuthTokens),
    ApiKey(String),
}

#[derive(Serialize, Deserialize)]
struct Wrapped<T> {
    #[serde(rename = "_0")]
    value: T,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CredentialRepr {
    Oauth(Wrapped<OAuthTokens>),
    ApiKey(Wrapped<String>),
}

impl Serialize for Credential {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Credential::OAuth(tokens) => CredentialRepr::Oauth(Wrapped { value: tokens.clone() }),
            Credential::ApiKey(key) => CredentialRepr::ApiKey(Wrapped { value: key.clone() }),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Credential {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match CredentialRepr::deserialize(deserializer)? {
            CredentialRepr::Oauth(wrapped) => Credential::OAuth(wrapped.value),
            CredentialRepr::ApiKey(wrapped) => Credential::ApiKey(wrapped.value),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_swift_account_and_credential_encoding() {
        let account: Account = serde_json::from_str(
            r#"{"id":"8AD3B217-0000-4000-8000-000000000000","kind":"claude","label":"Work","addedAt":0}"#,
        )
        .unwrap();
        assert_eq!(account.card_id(), "claude@8ad3b217");
        assert_eq!(account.added_at.timestamp(), 978_307_200);

        let key: Credential = serde_json::from_str(r#"{"apiKey":{"_0":"sk-1"}}"#).unwrap();
        assert_eq!(key, Credential::ApiKey("sk-1".into()));
        assert_eq!(serde_json::to_string(&key).unwrap(), r#"{"apiKey":{"_0":"sk-1"}}"#);

        let oauth: Credential = serde_json::from_str(
            r#"{"oauth":{"_0":{"accessToken":"a","refreshToken":"r","expiresAt":100,"extras":{}}}}"#,
        )
        .unwrap();
        let Credential::OAuth(tokens) = oauth else { panic!("expected oauth") };
        assert_eq!(tokens.expires_at.unwrap().timestamp(), 978_307_300);
    }
}
