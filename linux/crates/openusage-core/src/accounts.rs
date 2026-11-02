//! Saving, renaming, and removing accounts, and refusing a sign-in that is already stored.
//! The CLI and the panel both go through here so a duplicate is caught the same way.

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::account::{Account, Credential, OAuthTokens};
use crate::kind::ProviderKind;
use crate::log;
use crate::store::Settings;
use crate::vault::Vault;

/// Stable identity for a sign-in: the provider's account id, or a hash of an API key.
pub fn credential_identity(credential: &Credential) -> Option<String> {
    match credential {
        Credential::OAuth(tokens) => tokens.extras.get(OAuthTokens::IDENTITY_KEY).cloned(),
        Credential::ApiKey(key) => {
            let digest = Sha256::digest(key.as_bytes());
            let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            Some(format!("key:{hex}"))
        }
    }
}

/// `Err` is the sentence to show when this sign-in is already saved, or the keyring could not be checked.
/// `except` is the account whose sign-in is being replaced, so it is not a duplicate of itself.
pub fn duplicate_message(
    vault: &dyn Vault,
    accounts: &[Account],
    credential: &Credential,
    kind: ProviderKind,
    organization: Option<&str>,
    except: Option<Uuid>,
) -> Result<(), String> {
    let identity = credential_identity(credential);
    for account in accounts {
        if Some(account.id) == except || account.kind != kind || account.hub.is_some() {
            continue;
        }
        if let Some(organization) = organization {
            if account
                .organization
                .as_deref()
                .is_none_or(|name| !name.eq_ignore_ascii_case(organization))
            {
                continue;
            }
        } else {
            if account.organization.is_some() {
                continue;
            }
            let Some(identity) = identity.as_deref() else {
                return Ok(());
            };
            match vault.load(account.id) {
                Ok(Some(existing))
                    if credential_identity(&existing).as_deref() == Some(identity) => {}
                Ok(_) => continue,
                Err(error) => {
                    log::error(
                        "auth",
                        &format!(
                            "{} could not check its keyring item for a duplicate sign-in: {error}",
                            account.card_id()
                        ),
                    );
                    return Err(format!(
                        "OpenUsage couldn't check the saved sign-in for {}. Check Keychain access and try again.",
                        account.label
                    ));
                }
            }
        }
        return Err(format!(
            "This account is already added as {}.",
            account.label
        ));
    }
    Ok(())
}

/// Saves the credential, then the account. A failed account write deletes the credential just stored.
pub fn save_new(
    settings: &Settings,
    vault: &dyn Vault,
    kind: ProviderKind,
    label: String,
    organization: Option<String>,
    credential: &Credential,
) -> Result<Account, String> {
    duplicate_message(
        vault,
        &settings.accounts(),
        credential,
        kind,
        organization.as_deref(),
        None,
    )?;
    let account = Account::new(kind, label, organization);
    vault
        .save(account.id, credential)
        .map_err(|error| error.to_string())?;
    let saved = account.clone();
    if let Err(error) = settings.update_accounts(|accounts| accounts.push(saved)) {
        let _ = vault.delete(account.id);
        return Err(error.to_string());
    }
    Ok(account)
}

/// Replaces the sign-in of an existing account. The id, name, and place stay.
pub fn replace_sign_in(
    settings: &Settings,
    vault: &dyn Vault,
    id: Uuid,
    credential: &Credential,
) -> Result<(), String> {
    let accounts = settings.accounts();
    let Some(account) = accounts.iter().find(|account| account.id == id) else {
        return Err("That account is no longer saved.".into());
    };
    let kind = account.kind;
    let organization = account.organization.clone();
    duplicate_message(
        vault,
        &accounts,
        credential,
        kind,
        organization.as_deref(),
        Some(id),
    )?;
    vault
        .save(id, credential)
        .map_err(|error| error.to_string())
}

/// Deletes the keyring item, then the account. A keyring failure leaves the account in place.
pub fn remove_account(settings: &Settings, vault: &dyn Vault, id: Uuid) -> Result<(), String> {
    vault.delete(id).map_err(|error| error.to_string())?;
    settings
        .update_accounts(|accounts| accounts.retain(|account| account.id != id))
        .map_err(|error| error.to_string())
}

pub fn rename_account(settings: &Settings, id: Uuid, label: &str) -> Result<(), String> {
    let label = label.trim().to_string();
    if label.is_empty() {
        return Err("An account name can't be empty.".into());
    }
    let mut found = false;
    settings
        .update_accounts(|accounts| {
            if let Some(account) = accounts.iter_mut().find(|account| account.id == id) {
                account.label = label.clone();
                found = true;
            }
        })
        .map_err(|error| error.to_string())?;
    if found {
        Ok(())
    } else {
        Err("That account is no longer saved.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::OAuthTokens;
    use crate::vault::MemoryVault;
    use std::fs;

    fn temp_settings() -> (std::path::PathBuf, Settings) {
        let dir = std::env::temp_dir().join(format!("openusage-accounts-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let settings = Settings::at(dir.join("settings.plist"));
        (dir, settings)
    }

    #[test]
    fn a_second_copy_of_an_api_key_is_the_same_account() {
        let existing = Account::new(ProviderKind::OpenRouter, "Personal", None);
        let key = "secret-key";
        let vault = MemoryVault::with(existing.id, Credential::ApiKey(key.into()));
        let again = Credential::ApiKey(key.into());
        let error = duplicate_message(
            &vault,
            &[existing.clone()],
            &again,
            ProviderKind::OpenRouter,
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(error, "This account is already added as Personal.");
        let other = Credential::ApiKey("different".into());
        assert!(duplicate_message(
            &vault,
            &[existing.clone()],
            &other,
            ProviderKind::OpenRouter,
            None,
            None
        )
        .is_ok());
        let id = existing.id;
        assert!(duplicate_message(
            &vault,
            &[existing],
            &again,
            ProviderKind::OpenRouter,
            None,
            Some(id)
        )
        .is_ok());
    }

    #[test]
    fn an_oauth_sign_in_without_an_identity_is_not_checked() {
        let existing = Account::new(ProviderKind::Claude, "Work", None);
        let vault = MemoryVault::default();
        let fresh = Credential::OAuth(OAuthTokens::new("token"));
        assert!(duplicate_message(
            &vault,
            &[existing],
            &fresh,
            ProviderKind::Claude,
            None,
            None
        )
        .is_ok());
    }

    #[test]
    fn save_replace_rename_and_remove_round_trip() {
        let (dir, settings) = temp_settings();
        let vault = MemoryVault::default();
        let first = save_new(
            &settings,
            &vault,
            ProviderKind::OpenRouter,
            "Personal".into(),
            None,
            &Credential::ApiKey("one".into()),
        )
        .unwrap();
        let duplicate = save_new(
            &settings,
            &vault,
            ProviderKind::OpenRouter,
            "Other".into(),
            None,
            &Credential::ApiKey("one".into()),
        );
        assert!(duplicate.unwrap_err().contains("Personal"));
        replace_sign_in(
            &settings,
            &vault,
            first.id,
            &Credential::ApiKey("two".into()),
        )
        .unwrap();
        assert!(
            matches!(vault.load(first.id).unwrap(), Some(Credential::ApiKey(key)) if key == "two")
        );
        assert_eq!(settings.accounts()[0].id, first.id);
        rename_account(&settings, first.id, "Work").unwrap();
        assert_eq!(settings.accounts()[0].label, "Work");
        remove_account(&settings, &vault, first.id).unwrap();
        assert!(settings.accounts().is_empty());
        assert!(vault.load(first.id).unwrap().is_none());
        let _ = fs::remove_dir_all(dir);
    }
}
