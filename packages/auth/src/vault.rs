//! The bmodels vault: an application-owned sshenv vault (see [`sshenv_vault::app`]).
//!
//! It is unlocked by a generated key stored beside it, never the user's SSH keys, so reads
//! are prompt-free and work from `bmodels watch` under launchd or cron. Entries are encrypted
//! per profile and, by default, bound to this device when the host supports it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use bmodels_model::{AuthSpec, Secret, SecretName};
use bmodels_sources::SecretLookup;
use sshenv_vault::app::{
    AppDeviceSealOptions, AppDeviceSealPolicy, AppVault, AppVaultConfig, AppVaultError,
    ProfileSecurityStatus, SecurityReport,
};

/// Application name used for the vault's managed key (`bmodels_sshenv_ed25519`).
pub const APP_NAME: &str = "bmodels";

/// Errors from vault operations. Messages never contain secret values.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The underlying vault failed.
    #[error(transparent)]
    Vault(#[from] AppVaultError),
}

/// The bmodels vault.
#[derive(Debug, Clone)]
pub struct AuthVault {
    inner: AppVault,
}

impl AuthVault {
    /// The vault at `path`, with device-seal options from `[auth]`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, spec: &AuthSpec) -> Self {
        let strict = spec
            .device_seal_strict
            .map(|b| if b { "true" } else { "false" });
        let seal = AppDeviceSealOptions::from_settings(
            AppDeviceSealPolicy::Preferred,
            spec.device_seal.as_deref(),
            spec.device_seal_backend.as_deref(),
            spec.device_seal_mode.as_deref(),
            strict,
        );
        Self {
            inner: AppVault::new(AppVaultConfig::new(APP_NAME, path).with_device_seal(seal)),
        }
    }

    /// Vault file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.inner.path()
    }

    /// Whether the vault file exists.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.inner.exists()
    }

    /// The configured device-seal policy.
    #[must_use]
    pub const fn device_seal(&self) -> AppDeviceSealPolicy {
        self.inner.config().device_seal.policy
    }

    /// One entry. A missing vault reads as `None`.
    ///
    /// # Errors
    ///
    /// * [`VaultError::Vault`] if the vault or profile cannot be unlocked
    pub fn get(&self, name: &SecretName) -> Result<Option<Secret>, VaultError> {
        Ok(self.inner.get(&name.profile, &name.var)?.map(Secret::from))
    }

    /// Stores an entry (creating the vault on first use).
    ///
    /// # Errors
    ///
    /// * [`VaultError::Vault`] if the vault cannot be created, unlocked, or saved, or a
    ///   required device seal cannot be applied
    pub fn set(&self, name: &SecretName, value: Secret) -> Result<SecurityReport, VaultError> {
        Ok(self
            .inner
            .set(&name.profile, &name.var, value.into_zeroizing())?)
    }

    /// Stores an entry only if absent.
    ///
    /// # Errors
    ///
    /// * [`VaultError::Vault`] (`AlreadyExists`) if the entry exists; otherwise as [`Self::set`]
    pub fn insert_if_absent(
        &self,
        name: &SecretName,
        value: Secret,
    ) -> Result<SecurityReport, VaultError> {
        Ok(self
            .inner
            .insert_if_absent(&name.profile, &name.var, value.into_zeroizing())?)
    }

    /// Removes an entry; returns whether it existed.
    ///
    /// # Errors
    ///
    /// * [`VaultError::Vault`] if the vault cannot be unlocked or saved
    pub fn unset(&self, name: &SecretName) -> Result<bool, VaultError> {
        Ok(self.inner.unset(&name.profile, &name.var)?)
    }

    /// Profile names, without unlocking any profile.
    ///
    /// # Errors
    ///
    /// * [`VaultError::Vault`] if the vault metadata cannot be unlocked
    pub fn profiles(&self) -> Result<BTreeSet<String>, VaultError> {
        Ok(self.inner.profile_names()?)
    }

    /// Every entry name. Unlocks each profile; values are wiped immediately.
    ///
    /// # Errors
    ///
    /// * [`VaultError::Vault`] if the vault or a profile cannot be unlocked
    pub fn names(&self) -> Result<Vec<SecretName>, VaultError> {
        let mut names = Vec::new();
        for profile in self.profiles()? {
            for var in self.inner.var_names(&profile)? {
                names.push(SecretName {
                    profile: profile.clone(),
                    var,
                });
            }
        }
        Ok(names)
    }

    /// Security state of one profile (read-only).
    #[must_use]
    pub fn inspect(&self, profile: &str) -> ProfileSecurityStatus {
        self.inner.inspect(profile)
    }
}

/// Reads an environment variable.
type EnvFn<'a> = Box<dyn Fn(&str) -> Option<String> + Send + Sync + 'a>;

/// A [`SecretLookup`] over the process environment and the vault, caching per instance so a
/// sync unlocks each profile at most once. Create one per command or sync.
pub struct VaultLookup<'a> {
    vault: Option<&'a AuthVault>,
    env: EnvFn<'a>,
    profiles: OnceLock<Result<BTreeSet<String>, String>>,
    values: Mutex<BTreeMap<SecretName, Option<Secret>>>,
}

impl std::fmt::Debug for VaultLookup<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultLookup")
            .field("vault", &self.vault.map(AuthVault::path))
            .finish_non_exhaustive()
    }
}

impl<'a> VaultLookup<'a> {
    /// Looks up `vault` (if any) and the process environment.
    #[must_use]
    pub fn new(vault: Option<&'a AuthVault>) -> Self {
        Self::with_env(vault, |name| {
            std::env::var(name).ok().filter(|v| !v.trim().is_empty())
        })
    }

    /// Looks up `vault` (if any) and a custom environment (tests).
    #[must_use]
    pub fn with_env(
        vault: Option<&'a AuthVault>,
        env: impl Fn(&str) -> Option<String> + Send + Sync + 'a,
    ) -> Self {
        Self {
            vault,
            env: Box::new(env),
            profiles: OnceLock::new(),
            values: Mutex::new(BTreeMap::new()),
        }
    }
}

impl SecretLookup for VaultLookup<'_> {
    fn var(&self, name: &str) -> Option<String> {
        (self.env)(name)
    }

    fn vault(&self, name: &SecretName) -> Result<Option<Secret>, String> {
        let Some(vault) = self.vault.filter(|v| v.exists()) else {
            return Ok(None);
        };
        let profiles = self
            .profiles
            .get_or_init(|| vault.profiles().map_err(|e| e.to_string()));
        match profiles {
            Err(e) => return Err(e.clone()),
            Ok(p) if !p.contains(&name.profile) => return Ok(None),
            Ok(_) => {}
        }
        let values = || {
            self.values
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        let cached = values().get(name).cloned();
        if let Some(cached) = cached {
            return Ok(cached);
        }
        let value = vault.get(name).map_err(|e| e.to_string())?;
        values().insert(name.clone(), value.clone());
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(dir: &Path) -> AuthVault {
        AuthVault::new(
            dir.join("auth").join("vault"),
            &AuthSpec {
                device_seal: Some("off".into()),
                ..AuthSpec::default()
            },
        )
    }

    fn name(text: &str) -> SecretName {
        SecretName::parse(text).unwrap()
    }

    #[test]
    fn set_get_names_unset() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        assert!(v.get(&name("openai/API_KEY")).unwrap().is_none());
        assert!(!v.exists());
        v.set(&name("openai/API_KEY"), Secret::new("sk-1")).unwrap();
        v.set(&name("notify/WEBHOOK"), Secret::new("https://hook"))
            .unwrap();
        assert_eq!(
            v.get(&name("openai/API_KEY")).unwrap().unwrap().expose(),
            "sk-1"
        );
        assert_eq!(
            v.names().unwrap(),
            vec![name("notify/WEBHOOK"), name("openai/API_KEY")]
        );
        assert!(v.unset(&name("openai/API_KEY")).unwrap());
        assert!(v.get(&name("openai/API_KEY")).unwrap().is_none());
        assert!(
            v.insert_if_absent(&name("notify/WEBHOOK"), Secret::new("x"))
                .is_err()
        );
    }

    #[test]
    fn lookup_reads_vault_then_caches() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        v.set(&name("openai/API_KEY"), Secret::new("sk-1")).unwrap();
        let lookup = VaultLookup::with_env(Some(&v), |n| (n == "E").then(|| "e".to_owned()));
        assert_eq!(lookup.var("E").as_deref(), Some("e"));
        assert_eq!(
            lookup
                .vault(&name("openai/API_KEY"))
                .unwrap()
                .unwrap()
                .expose(),
            "sk-1"
        );
        // Removed behind the lookup's back: the cached value is still served.
        v.unset(&name("openai/API_KEY")).unwrap();
        assert!(lookup.vault(&name("openai/API_KEY")).unwrap().is_some());
        assert!(lookup.vault(&name("other/API_KEY")).unwrap().is_none());
    }

    #[test]
    fn missing_vault_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        let lookup = VaultLookup::with_env(Some(&v), |_| None);
        assert!(lookup.vault(&name("openai/API_KEY")).unwrap().is_none());
        assert!(!v.exists());
    }
}
