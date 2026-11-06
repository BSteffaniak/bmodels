//! Shared command context: configuration, database path, and the async runtime.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use bmodels_config::{LoadOptions, LoadedConfig};
use bmodels_model::{Config, Timestamp};
use bmodels_store::Store;

use crate::args::Cli;

/// Loaded configuration and paths.
#[derive(Debug, Clone)]
pub struct Context {
    /// Composed configuration with provenance.
    pub loaded: Arc<LoadedConfig>,
    /// Database file.
    pub database: PathBuf,
}

impl Context {
    /// Loads configuration for `cli`.
    pub fn load(cli: &Cli) -> anyhow::Result<Self> {
        let options = LoadOptions::from_process()?.with_explicit(cli.config.clone());
        let mut loaded = bmodels_config::load(&options)?;
        if let Some(dir) = &cli.data_dir {
            loaded.data_dir.clone_from(dir);
        }
        let database = loaded.database_path();
        Ok(Self {
            loaded: Arc::new(loaded),
            database,
        })
    }

    /// The composed configuration.
    pub fn config(&self) -> &Config {
        &self.loaded.config
    }

    /// Opens the store.
    pub async fn store(&self) -> anyhow::Result<Store> {
        Store::open(&self.database)
            .await
            .with_context(|| format!("opening {}", self.database.display()))
    }

    /// The HTTP timeout.
    pub fn timeout(&self) -> std::time::Duration {
        self.config()
            .settings
            .timeout()
            .unwrap_or(std::time::Duration::from_secs(30))
    }
}

/// Runs a future on a fresh current-thread runtime.
pub fn block_on<F: Future>(future: F) -> anyhow::Result<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    Ok(rt.block_on(future))
}

/// Current Unix time.
pub fn now() -> Timestamp {
    bmodels_model::now()
}
