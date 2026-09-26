//! API keys in the OS keychain (principle 7).
//!
//! Only the Rust core reads secret values. The one thing allowed to leave this
//! module toward the frontend is [`SecretStatus`], which carries at most the
//! last four characters ("key ending 4Kf2").

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

use crate::error::{Error, Result};

/// Keychain service name. Entry names (the "account") are things like
/// `provider:anthropic` or `source:openalex`.
pub const KEYCHAIN_SERVICE: &str = "org.sift.desktop";

pub trait SecretStore: Send + Sync {
    fn set(&self, name: &str, value: &str) -> Result<()>;
    fn get(&self, name: &str) -> Result<Option<String>>;
    fn delete(&self, name: &str) -> Result<()>;
}

/// macOS Keychain, Windows Credential Manager, or Secret Service on Linux.
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, name).map_err(|e| Error::Secret(e.to_string()))
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new(KEYCHAIN_SERVICE)
    }
}

impl SecretStore for KeychainStore {
    fn set(&self, name: &str, value: &str) -> Result<()> {
        self.entry(name)?
            .set_password(value)
            .map_err(|e| Error::Secret(e.to_string()))
    }

    fn get(&self, name: &str) -> Result<Option<String>> {
        match self.entry(name)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Secret(e.to_string())),
        }
    }

    fn delete(&self, name: &str) -> Result<()> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Secret(e.to_string())),
        }
    }
}

/// In-memory store for tests.
#[derive(Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, String>>> {
        self.values
            .lock()
            .map_err(|_| Error::Secret("secret store lock poisoned".into()))
    }
}

impl SecretStore for MemoryStore {
    fn set(&self, name: &str, value: &str) -> Result<()> {
        self.lock()?.insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.lock()?.get(name).cloned())
    }

    fn delete(&self, name: &str) -> Result<()> {
        self.lock()?.remove(name);
        Ok(())
    }
}

/// What the frontend may know about a stored secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    pub present: bool,
    /// "key ending 4Kf2", or `None` when absent or too short to reveal anything.
    pub hint: Option<String>,
}

/// Masked description of a secret. Keys shorter than 12 characters show no
/// characters at all, so the hint never reveals a meaningful share of the key.
pub fn hint(secret: &str) -> Option<String> {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 12 {
        return None;
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    Some(format!("key ending {tail}"))
}

pub fn status(store: &dyn SecretStore, name: &str) -> Result<SecretStatus> {
    Ok(match store.get(name)? {
        Some(v) => SecretStatus {
            present: true,
            hint: hint(&v),
        },
        None => SecretStatus {
            present: false,
            hint: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_shows_only_last_four() {
        assert_eq!(
            hint("sk-ant-api03-abcdefgh4Kf2").as_deref(),
            Some("key ending 4Kf2")
        );
        assert_eq!(hint("short"), None);
        assert_eq!(hint("ééééééééééé€€€€").as_deref(), Some("key ending €€€€"));
    }

    #[test]
    fn memory_store_round_trip_and_status() {
        let store = MemoryStore::default();
        assert_eq!(
            status(&store, "provider:x").unwrap(),
            SecretStatus {
                present: false,
                hint: None
            }
        );
        store.set("provider:x", "sk-test-000000000000abcd").unwrap();
        let s = status(&store, "provider:x").unwrap();
        assert!(s.present);
        assert_eq!(s.hint.as_deref(), Some("key ending abcd"));
        assert!(!serde_json::to_string(&s).unwrap().contains("sk-test"));
        store.delete("provider:x").unwrap();
        assert!(store.get("provider:x").unwrap().is_none());
    }

    /// Touches the real OS keychain, so it is opt-in:
    /// `cargo test -- --ignored keychain_round_trip`
    #[test]
    #[ignore]
    fn keychain_round_trip() {
        let store = KeychainStore::new("org.sift.desktop.test");
        store.set("test:roundtrip", "value-000000001234").unwrap();
        assert_eq!(
            store.get("test:roundtrip").unwrap().as_deref(),
            Some("value-000000001234")
        );
        store.delete("test:roundtrip").unwrap();
        assert!(store.get("test:roundtrip").unwrap().is_none());
    }
}
