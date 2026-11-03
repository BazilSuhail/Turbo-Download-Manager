//! OS keychain storage for secrets such as HTTP proxy passwords and site logins.
//!
//! Settings hold only a reference (`keychain:<service_key>`, see [`reference`]);
//! the secret itself lives in the platform keychain under service `TDM` and
//! account `<service_key>`. Examples of keys: `proxy/password`,
//! `site/<host>/<user>`.
//!
//! Backends: Windows Credential Manager, macOS Keychain, and the Linux Secret
//! Service over D-Bus (GNOME Keyring, KWallet, ...).
//!
//! Fallback: when no keychain answers ([`SecretError::Unavailable`]), callers
//! must fall back to their non-keychain behaviour and emit a warning. The
//! engine owner decides what that fallback is. Secrets are never logged and
//! never appear in error messages.

use std::fmt;

/// Keychain service name under which all TDM secrets are stored.
const SERVICE: &str = "TDM";
/// Prefix that marks a settings string as a keychain reference.
const REFERENCE_PREFIX: &str = "keychain:";

/// Failure from a keychain operation. No variant carries the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// No keychain could be reached or used (no D-Bus secret service, locked
    /// or denied access, platform failure). Callers should fall back and warn.
    Unavailable(String),
    /// No secret is stored under the requested key.
    ///
    /// [`delete`] returns this when there was nothing to delete. Callers that
    /// only want the secret gone should treat it as success.
    NotFound,
    /// Any other failure, such as a stored value that is not UTF-8 or a key the
    /// platform rejects.
    Other(String),
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretError::Unavailable(why) => write!(f, "OS keychain unavailable: {why}"),
            SecretError::NotFound => write!(f, "no secret stored in the OS keychain"),
            SecretError::Other(why) => write!(f, "OS keychain error: {why}"),
        }
    }
}

impl std::error::Error for SecretError {}

fn entry(service_key: &str) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(SERVICE, service_key).map_err(map_error)
}

/// Maps a keyring error to a [`SecretError`] without copying any payload,
/// since platform error text and credential debug output could in principle
/// echo stored data.
fn map_error(err: keyring::Error) -> SecretError {
    match err {
        keyring::Error::NoEntry => SecretError::NotFound,
        keyring::Error::NoStorageAccess(_) => {
            SecretError::Unavailable("keychain access denied or no keychain".into())
        }
        keyring::Error::PlatformFailure(_) => {
            SecretError::Unavailable("keychain platform failure".into())
        }
        keyring::Error::BadEncoding(_) => {
            SecretError::Other("stored secret is not valid UTF-8".into())
        }
        keyring::Error::TooLong(name, _) => {
            SecretError::Other(format!("attribute {name} exceeds platform limit"))
        }
        keyring::Error::Invalid(attr, _) => {
            SecretError::Other(format!("attribute {attr} is invalid"))
        }
        keyring::Error::Ambiguous(_) => SecretError::Other("keychain entry is ambiguous".into()),
        // keyring::Error is #[non_exhaustive]; unknown future variants stay generic.
        _ => SecretError::Other("keychain operation failed".into()),
    }
}

/// Stores `secret` in the keychain under `service_key`, replacing any old value.
pub fn store(service_key: &str, secret: &str) -> Result<(), SecretError> {
    entry(service_key)?.set_password(secret).map_err(map_error)
}

/// Loads the secret stored under `service_key`.
///
/// Returns [`SecretError::NotFound`] when nothing is stored there.
pub fn load(service_key: &str) -> Result<String, SecretError> {
    entry(service_key)?.get_password().map_err(map_error)
}

/// Removes the secret stored under `service_key`.
///
/// Returns [`SecretError::NotFound`] when nothing was stored. Callers that only
/// need the secret gone should treat `NotFound` as success.
pub fn delete(service_key: &str) -> Result<(), SecretError> {
    entry(service_key)?.delete_credential().map_err(map_error)
}

/// Builds the settings string that points at the secret for `service_key`.
pub fn reference(service_key: &str) -> String {
    format!("{REFERENCE_PREFIX}{service_key}")
}

/// Returns the service key if `s` is a keychain reference, otherwise `None`.
/// An empty key is not a valid reference.
pub fn parse_reference(s: &str) -> Option<&str> {
    s.strip_prefix(REFERENCE_PREFIX).filter(|key| !key.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_round_trips() {
        let r = reference("proxy/password");
        assert_eq!(r, "keychain:proxy/password");
        assert_eq!(parse_reference(&r), Some("proxy/password"));
    }

    #[test]
    fn reference_keeps_site_keys() {
        let r = reference("site/example.com/alice");
        assert_eq!(parse_reference(&r), Some("site/example.com/alice"));
    }

    #[test]
    fn parse_rejects_plain_values() {
        assert_eq!(parse_reference("hunter2"), None);
        assert_eq!(parse_reference("Keychain:proxy/password"), None);
        assert_eq!(parse_reference(""), None);
    }

    #[test]
    fn parse_rejects_empty_key() {
        assert_eq!(parse_reference("keychain:"), None);
    }
}
