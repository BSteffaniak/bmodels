//! Per-platform application directories, resolved by `bmux_config_composition`'s [`AppDirs`].
//!
//! | dir      | macOS                                     | Linux                                         | Windows                 |
//! | -------- | ----------------------------------------- | --------------------------------------------- | ----------------------- |
//! | `config` | `$XDG_CONFIG_HOME/bmodels` or `~/.config/bmodels` | same                                  | `%APPDATA%\bmodels`     |
//! | `data`   | `~/Library/Application Support/bmodels`   | `$XDG_DATA_HOME/bmodels` or `~/.local/share/bmodels` | `%LOCALAPPDATA%\bmodels\data` |

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use bmux_config_composition::{AppDirs, Platform};

/// Application directory name.
pub const APP_NAME: &str = "bmodels";
/// Database file name inside the data directory.
pub const DATABASE_FILE: &str = "bmodels.sqlite";

/// The home directory could not be determined.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot determine the home directory (HOME is not set)")]
pub struct NoHome;

/// Environment snapshot plus the platform conventions to apply, so path resolution is
/// testable. The home directory comes from `HOME` (or `USERPROFILE` on Windows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    vars: BTreeMap<String, String>,
    platform: Platform,
}

impl Environment {
    /// Creates an environment from explicit variables, for the current platform.
    #[must_use]
    pub const fn new(vars: BTreeMap<String, String>) -> Self {
        Self {
            vars,
            platform: Platform::current(),
        }
    }

    /// Resolves directories with `platform`'s conventions instead of the current one.
    #[must_use]
    pub const fn with_platform(mut self, platform: Platform) -> Self {
        self.platform = platform;
        self
    }

    /// Captures the current process's environment.
    #[must_use]
    pub fn from_process() -> Self {
        Self::new(std::env::vars().collect())
    }

    /// All captured variables.
    #[must_use]
    pub const fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    /// A non-empty variable.
    #[must_use]
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// Expands a leading `~` to the home directory.
    ///
    /// # Errors
    ///
    /// * [`NoHome`] if the path starts with `~` and the home directory is unknown
    pub fn expand_tilde(&self, path: &str) -> Result<PathBuf, NoHome> {
        bmux_config_composition::expand_home(path, &self.vars, self.platform).map_err(|_| NoHome)
    }

    /// The application directories.
    ///
    /// # Errors
    ///
    /// * [`NoHome`] if a needed base directory cannot be determined
    pub fn app_dirs(&self) -> Result<AppDirs, NoHome> {
        // `APP_NAME` is a valid single component, so the only failure is a missing home.
        AppDirs::resolve(APP_NAME, &self.vars, self.platform).map_err(|_| NoHome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)], platform: Platform) -> Environment {
        let mut vars: BTreeMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        vars.insert("HOME".to_owned(), "/home/u".to_owned());
        Environment::new(vars).with_platform(platform)
    }

    #[test]
    fn linux_honors_xdg() {
        let dirs = env(
            &[("XDG_CONFIG_HOME", "/cfg"), ("XDG_DATA_HOME", "/data")],
            Platform::Unix,
        )
        .app_dirs()
        .unwrap();
        assert_eq!(
            dirs.config_file(),
            PathBuf::from("/cfg/bmodels/config.toml")
        );
        assert_eq!(dirs.data, PathBuf::from("/data/bmodels"));
    }

    #[test]
    fn macos_uses_dot_config_and_application_support() {
        let dirs = env(&[], Platform::MacOs).app_dirs().unwrap();
        assert_eq!(dirs.config, PathBuf::from("/home/u/.config/bmodels"));
        assert_eq!(
            dirs.data,
            PathBuf::from("/home/u/Library/Application Support/bmodels")
        );
    }

    #[test]
    fn expands_tilde_and_reports_missing_home() {
        let e = env(&[], Platform::Unix);
        assert_eq!(e.expand_tilde("~/x").unwrap(), PathBuf::from("/home/u/x"));
        assert_eq!(e.expand_tilde("/abs").unwrap(), PathBuf::from("/abs"));
        let no_home = Environment::new(BTreeMap::new());
        assert_eq!(no_home.expand_tilde("~/x"), Err(NoHome));
        assert_eq!(no_home.app_dirs(), Err(NoHome));
    }
}
