//! Bearer tokens live only in native secure storage or process memory.

use std::{collections::HashMap, fmt};
use uuid::Uuid;

const SERVICE: &str = "app-depot-client";

#[derive(Debug, PartialEq, Eq)]
pub enum SecretError {
    Unavailable,
    Missing,
    Invalid,
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => write!(
                f,
                "native secure storage unavailable; use anonymous access or APP_DEPOT_TOKEN for this process"
            ),
            Self::Missing => write!(f, "secure credential missing"),
            Self::Invalid => write!(f, "invalid Bearer token"),
        }
    }
}
impl std::error::Error for SecretError {}

pub trait SecretStore {
    fn put(&mut self, token: &str) -> Result<String, SecretError>;
    fn replace(&mut self, reference: &str, token: &str) -> Result<(), SecretError>;
    fn get(&self, reference: &str) -> Result<String, SecretError>;
    fn delete(&mut self, reference: &str) -> Result<(), SecretError>;
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| (33..=126).contains(&byte))
}
fn valid_ref(reference: &str) -> bool {
    Uuid::parse_str(reference).is_ok()
}

pub struct NativeSecrets;
impl SecretStore for NativeSecrets {
    fn put(&mut self, token: &str) -> Result<String, SecretError> {
        if !valid_token(token) {
            return Err(SecretError::Invalid);
        }
        let reference = Uuid::new_v4().to_string();
        keyring::Entry::new(SERVICE, &reference)
            .map_err(|_| SecretError::Unavailable)?
            .set_password(token)
            .map_err(|_| SecretError::Unavailable)?;
        Ok(reference)
    }
    fn get(&self, reference: &str) -> Result<String, SecretError> {
        if !valid_ref(reference) {
            return Err(SecretError::Invalid);
        }
        keyring::Entry::new(SERVICE, reference)
            .map_err(|_| SecretError::Unavailable)?
            .get_password()
            .map_err(|error| match error {
                keyring::Error::NoEntry => SecretError::Missing,
                _ => SecretError::Unavailable,
            })
    }
    fn replace(&mut self, reference: &str, token: &str) -> Result<(), SecretError> {
        if !valid_ref(reference) || !valid_token(token) {
            return Err(SecretError::Invalid);
        }
        let entry =
            keyring::Entry::new(SERVICE, reference).map_err(|_| SecretError::Unavailable)?;
        entry.get_password().map_err(|error| match error {
            keyring::Error::NoEntry => SecretError::Missing,
            _ => SecretError::Unavailable,
        })?;
        entry
            .set_password(token)
            .map_err(|_| SecretError::Unavailable)
    }
    fn delete(&mut self, reference: &str) -> Result<(), SecretError> {
        if !valid_ref(reference) {
            return Err(SecretError::Invalid);
        }
        keyring::Entry::new(SERVICE, reference)
            .map_err(|_| SecretError::Unavailable)?
            .delete_credential()
            .map_err(|error| match error {
                keyring::Error::NoEntry => SecretError::Missing,
                _ => SecretError::Unavailable,
            })
    }
}

#[derive(Default)]
pub struct MemorySecrets {
    values: HashMap<String, String>,
}
impl SecretStore for MemorySecrets {
    fn put(&mut self, token: &str) -> Result<String, SecretError> {
        if !valid_token(token) {
            return Err(SecretError::Invalid);
        }
        let reference = Uuid::new_v4().to_string();
        self.values.insert(reference.clone(), token.into());
        Ok(reference)
    }
    fn get(&self, reference: &str) -> Result<String, SecretError> {
        self.values
            .get(reference)
            .cloned()
            .ok_or(SecretError::Missing)
    }
    fn replace(&mut self, reference: &str, token: &str) -> Result<(), SecretError> {
        if !valid_token(token) {
            return Err(SecretError::Invalid);
        }
        let stored = self.values.get_mut(reference).ok_or(SecretError::Missing)?;
        *stored = token.into();
        Ok(())
    }
    fn delete(&mut self, reference: &str) -> Result<(), SecretError> {
        self.values
            .remove(reference)
            .map(|_| ())
            .ok_or(SecretError::Missing)
    }
}

pub struct UnavailableSecrets;
impl SecretStore for UnavailableSecrets {
    fn put(&mut self, _: &str) -> Result<String, SecretError> {
        Err(SecretError::Unavailable)
    }
    fn get(&self, _: &str) -> Result<String, SecretError> {
        Err(SecretError::Unavailable)
    }
    fn replace(&mut self, _: &str, _: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable)
    }
    fn delete(&mut self, _: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refs_are_distinct_and_debug_never_contains_token() {
        let mut store = MemorySecrets::default();
        let first = store.put("one-secret").unwrap();
        let second = store.put("two-secret").unwrap();
        assert_ne!(first, second);
        assert_eq!(store.get(&first).unwrap(), "one-secret");
        store.replace(&first, "updated-secret").unwrap();
        assert_eq!(store.get(&first).unwrap(), "updated-secret");
        store.delete(&first).unwrap();
        assert_eq!(store.get(&first), Err(SecretError::Missing));
        assert_eq!(store.get(&second).unwrap(), "two-secret");
        assert!(!format!("{:?}", SecretError::Unavailable).contains("secret"));
    }
    #[test]
    fn unavailable_store_has_no_plaintext_fallback() {
        assert_eq!(
            UnavailableSecrets.put("token"),
            Err(SecretError::Unavailable)
        );
    }
}
