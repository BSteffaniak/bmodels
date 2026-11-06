//! Per-platform application directories.
//!
//! | dir      | macOS                                     | Linux                                         | Windows                 |
//! | -------- | ----------------------------------------- | --------------------------------------------- | ----------------------- |
//! | `config` | `$XDG_CONFIG_HOME/bmodels` or `~/.config/bmodels` | same                                  | `%APPDATA%\bmodels`     |
//! | `data`   | `~/Library/Application Support/bmodels`   | `$XDG_DATA_HOME/bmodels` or `~/.local/share/bmodels` | `%LOCALAPPDATA%\bmodels` |
//!
//! The configuration directory deliberately uses `~/.config` on macOS (like most CLI tools).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Application directory name.
pub const APP_NAME: &str = "bmodels";
/// Database file name inside the data directory.
pub const DATABASE_FILE: &str = "bmodels.sqlite";

/// The home directory could not be determined.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot determine the home directory")]
pub struct NoHome;

/// Home directory plus an environment snapshot, so path resolution is testable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    home: Option<PathBuf>,
    vars: BTreeMap<String, String>,
}

impl Environment {
    /// Creates an environment from explicit values.
    #[must_use]
    pub const fn new(home: Option<PathBuf>, vars: BTreeMap<String, String>) -> Self {
        Self { home, vars }
    }

    /// Captures the current process's home directory and environment.
    #[must_use]
    pub fn from_process() -> Self {
        Self::new(dirs::home_dir(), std::env::vars().collect())
    }

    /// The home directory, if known.
    #[must_use]
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
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

    fn var_path(&self, name: &str) -> Option<PathBuf> {
        self.var(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    }

    /// Expands a leading `~` to the home directory.
    ///
    /// # Errors
    ///
    /// * [`NoHome`] if the path starts with `~` and the home directory is unknown
    pub fn expand_tilde(&self, path: &str) -> Result<PathBuf, NoHome> {
        if path == "~" {
            return self.home().map(Path::to_path_buf).ok_or(NoHome);
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return Ok(self.home().ok_or(NoHome)?.join(rest));
        }
        Ok(PathBuf::from(path))
    }
}

/// Target platform for directory conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// macOS.
    Macos,
    /// Linux and other Unix-likes.
    Linux,
    /// Windows.
    Windows,
}

impl Platform {
    /// The platform this binary was compiled for.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Resolved application directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs {
    /// Holds `config.toml`.
    pub config: PathBuf,
    /// Holds the database.
    pub data: PathBuf,
}

impl AppDirs {
    /// Resolves the layout for `platform`.
    ///
    /// # Errors
    ///
    /// * [`NoHome`] if a needed base directory cannot be determined
    pub fn resolve(env: &Environment, platform: Platform) -> Result<Self, NoHome> {
        let home = || env.home().map(Path::to_path_buf).ok_or(NoHome);
        Ok(match platform {
            Platform::Windows => {
                let roaming = env
                    .var_path("APPDATA")
                    .map_or_else(|| home().map(|h| h.join("AppData").join("Roaming")), Ok)?;
                let local = env
                    .var_path("LOCALAPPDATA")
                    .map_or_else(|| home().map(|h| h.join("AppData").join("Local")), Ok)?;
                Self {
                    config: roaming.join(APP_NAME),
                    data: local.join(APP_NAME),
                }
            }
            Platform::Macos | Platform::Linux => {
                let config = env
                    .var_path("XDG_CONFIG_HOME")
                    .map_or_else(|| home().map(|h| h.join(".config")), Ok)?
                    .join(APP_NAME);
                let data = if platform == Platform::Macos {
                    home()?
                        .join("Library")
                        .join("Application Support")
                        .join(APP_NAME)
                } else {
                    env.var_path("XDG_DATA_HOME")
                        .map_or_else(|| home().map(|h| h.join(".local").join("share")), Ok)?
                        .join(APP_NAME)
                };
                Self { config, data }
            }
        })
    }

    /// `<config>/config.toml`.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    /// `<data>/bmodels.sqlite`.
    #[must_use]
    pub fn database_file(&self) -> PathBuf {
        self.data.join(DATABASE_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> Environment {
        Environment::new(
            Some(PathBuf::from("/home/u")),
            vars.iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn linux_honors_xdg() {
        let dirs = AppDirs::resolve(
            &env(&[("XDG_CONFIG_HOME", "/cfg"), ("XDG_DATA_HOME", "/data")]),
            Platform::Linux,
        )
        .unwrap();
        assert_eq!(
            dirs.config_file(),
            PathBuf::from("/cfg/bmodels/config.toml")
        );
        assert_eq!(
            dirs.database_file(),
            PathBuf::from("/data/bmodels/bmodels.sqlite")
        );
    }

    #[test]
    fn macos_uses_dot_config_and_application_support() {
        let dirs = AppDirs::resolve(&env(&[]), Platform::Macos).unwrap();
        assert_eq!(dirs.config, PathBuf::from("/home/u/.config/bmodels"));
        assert_eq!(
            dirs.data,
            PathBuf::from("/home/u/Library/Application Support/bmodels")
        );
    }

    #[test]
    fn expands_tilde() {
        let e = env(&[]);
        assert_eq!(e.expand_tilde("~/x").unwrap(), PathBuf::from("/home/u/x"));
        assert_eq!(e.expand_tilde("/abs").unwrap(), PathBuf::from("/abs"));
    }
}
