//! The Anthropic API key, kept in the platform credential store.
//!
//! The key is never written to `settings.json` and never logged. When none
//! is stored, `ANTHROPIC_API_KEY` from the environment is used instead.
//! Every native call runs on a blocking thread with a deadline, so a locked
//! keychain cannot hold up the runtime or the interface.

use std::time::Duration;

use keyring_core::api::CredentialStoreApi;

use super::{KeyStatus, Secret};

/// The service every Spotifast secret is filed under.
const SERVICE: &str = "rocks.spotifast.Spotifast";
/// This secret's own entry, beside the Spotify grants.
const ENTRY: &str = "anthropic-api-key";
const ENVIRONMENT: &str = "ANTHROPIC_API_KEY";
const TIMEOUT: Duration = Duration::from_secs(20);

/// The key to use, stored first and then the environment's.
pub async fn resolve() -> Option<String> {
    let stored = native(read).await.ok().flatten();
    choose(stored, std::env::var(ENVIRONMENT).ok()).map(|(key, _)| key)
}

/// Where the key would come from right now.
pub async fn status() -> KeyStatus {
    match native(read).await {
        Ok(stored) => choose(stored, std::env::var(ENVIRONMENT).ok())
            .map_or(KeyStatus::Missing, |(_, status)| status),
        Err(()) => KeyStatus::StoreFailed,
    }
}

/// Saves `secret`, replacing any stored key.
pub async fn store(secret: Secret) -> KeyStatus {
    let key = secret.0.trim().to_string();
    if key.is_empty() {
        return status().await;
    }
    match native(move || write(&key)).await {
        Ok(()) => KeyStatus::Stored,
        Err(()) => KeyStatus::StoreFailed,
    }
}

/// Removes the stored key; the environment's, if any, takes over.
pub async fn forget() -> KeyStatus {
    match native(delete).await {
        Ok(()) => status().await,
        Err(()) => KeyStatus::StoreFailed,
    }
}

/// A stored key wins over the environment; blank values count as none.
fn choose(stored: Option<String>, environment: Option<String>) -> Option<(String, KeyStatus)> {
    let usable = |key: Option<String>| {
        key.map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
    };
    usable(stored)
        .map(|key| (key, KeyStatus::Stored))
        .or_else(|| usable(environment).map(|key| (key, KeyStatus::Environment)))
}

/// Runs a credential-store call off the runtime's worker threads.
async fn native<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, ()> + Send + 'static,
) -> Result<T, ()> {
    match tokio::time::timeout(TIMEOUT, tokio::task::spawn_blocking(call)).await {
        Ok(Ok(result)) => result,
        _ => {
            log::warn!("the credential store did not answer for the Anthropic API key");
            Err(())
        }
    }
}

fn entry() -> Result<keyring_core::Entry, ()> {
    #[cfg(target_os = "linux")]
    let store = zbus_secret_service_keyring_store::Store::new();
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new();
    #[cfg(windows)]
    let store = windows_native_keyring_store::Store::new();
    // Provider errors can carry platform data; only that it failed is kept.
    let store = store.map_err(|_| log::warn!("the credential store is unavailable"))?;
    store
        .build(SERVICE, ENTRY, None)
        .map_err(|_| log::warn!("unable to open the Anthropic API key entry"))
}

fn read() -> Result<Option<String>, ()> {
    match entry()?.get_secret() {
        Ok(secret) => Ok(String::from_utf8(secret).ok()),
        Err(keyring_core::Error::NoEntry) => Ok(None),
        Err(_) => {
            log::warn!("unable to read the Anthropic API key from the credential store");
            Err(())
        }
    }
}

fn write(key: &str) -> Result<(), ()> {
    entry()?
        .set_secret(key.as_bytes())
        .map_err(|_| log::warn!("unable to save the Anthropic API key"))
}

fn delete() -> Result<(), ()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(_) => {
            log::warn!("unable to remove the Anthropic API key");
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_key_wins_over_the_environment() {
        assert_eq!(
            choose(Some("stored".into()), Some("env".into())),
            Some(("stored".into(), KeyStatus::Stored))
        );
    }

    #[test]
    fn the_environment_fills_in_when_nothing_is_stored() {
        assert_eq!(
            choose(None, Some(" env \n".into())),
            Some(("env".into(), KeyStatus::Environment))
        );
        assert_eq!(
            choose(Some("   ".into()), Some("env".into())),
            Some(("env".into(), KeyStatus::Environment))
        );
    }

    #[test]
    fn blank_keys_are_no_keys() {
        assert_eq!(choose(None, None), None);
        assert_eq!(choose(Some(String::new()), Some(" ".into())), None);
    }
}
