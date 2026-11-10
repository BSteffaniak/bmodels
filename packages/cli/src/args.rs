//! Command-line arguments.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Watch model registries for new and changed AI models.
///
/// With no command, opens the interactive UI.
#[derive(Debug, Parser)]
#[command(name = "bmodels", version, about, long_about = None)]
pub struct Cli {
    /// Extra configuration file applied on top of the global one.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Data directory holding the database (overrides config and `$BMODELS_DATA_DIR`).
    #[arg(long, global = true, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Log more (repeat for debug output).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Command.
    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Open the interactive UI (the default).
    Tui,
    /// Sync sources now.
    Sync(SyncArgs),
    /// Sync on a schedule in the foreground, notifying about new models.
    Watch(WatchArgs),
    /// Show the inbox: events you have not acknowledged.
    New(NewArgs),
    /// Acknowledge models, listings, or events.
    Ack(AckArgs),
    /// Star a model.
    Star(KeyArgs),
    /// Unstar a model (it stays seen).
    Unstar(KeyArgs),
    /// Ignore a model: it never surfaces or notifies again.
    Ignore(KeyArgs),
    /// Stop ignoring a model.
    Unignore(KeyArgs),
    /// Mark a model unseen again.
    Unsee(KeyArgs),
    /// Attach a note to a model (no text clears it).
    Note(NoteArgs),
    /// List known models.
    List(ListArgs),
    /// Show everything known about one model.
    Show(ShowArgs),
    /// Show recent events, including acknowledged ones.
    History(HistoryArgs),
    /// Show configured sources and their health.
    Sources(JsonArgs),
    /// Manage source API keys (stored in the bmodels sshenv vault).
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Manage other secrets in the vault (webhook URLs, header values).
    #[command(subcommand)]
    Secret(SecretCommand),
    /// Configuration helpers.
    #[command(subcommand)]
    Config(ConfigCommand),
}

/// `--json`.
#[derive(Debug, Args)]
pub struct JsonArgs {
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `sync`.
#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)] // independent CLI flags
pub struct SyncArgs {
    /// Sources to sync (default: every enabled source).
    pub sources: Vec<String>,
    /// Only sources whose interval has elapsed.
    #[arg(long, conflicts_with = "sources")]
    pub due: bool,
    /// Send notifications for new events.
    #[arg(long)]
    pub notify: bool,
    /// Print only the summary line (errors still print).
    #[arg(short, long)]
    pub quiet: bool,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `watch`.
#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Do not send notifications.
    #[arg(long)]
    pub no_notify: bool,
    /// Run one scheduling pass and exit (for cron / launchd).
    #[arg(long)]
    pub once: bool,
}

/// `new`.
#[derive(Debug, Args)]
pub struct NewArgs {
    /// Only events from this source.
    #[arg(long)]
    pub source: Option<String>,
    /// Only models of this vendor.
    #[arg(long)]
    pub vendor: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `ack`.
#[derive(Debug, Args)]
pub struct AckArgs {
    /// Model keys (`openai/gpt-6`), listings (`openrouter:openai/gpt-6`), event ids (`#12`),
    /// or unambiguous names.
    #[arg(required_unless_present = "all")]
    pub targets: Vec<String>,
    /// Acknowledge every inbox event (optionally limited by `--source` / `--vendor`).
    #[arg(long, conflicts_with = "targets")]
    pub all: bool,
    /// With `--all`: only this source.
    #[arg(long, requires = "all")]
    pub source: Option<String>,
    /// With `--all`: only this vendor.
    #[arg(long, requires = "all")]
    pub vendor: Option<String>,
}

/// A single model argument.
#[derive(Debug, Args)]
pub struct KeyArgs {
    /// Model key, listing, or unambiguous name.
    pub model: String,
}

/// `note`.
#[derive(Debug, Args)]
pub struct NoteArgs {
    /// Model key, listing, or unambiguous name.
    pub model: String,
    /// Note text (omit to clear).
    pub text: Vec<String>,
}

/// `list`.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// Only this vendor.
    #[arg(long)]
    pub vendor: Option<String>,
    /// Only models listed by this source.
    #[arg(long)]
    pub source: Option<String>,
    /// Only models first seen within this long (`7d`, `12h`).
    #[arg(long, value_name = "DURATION")]
    pub since: Option<String>,
    /// Only models with this status (unseen, seen, starred, ignored).
    #[arg(long)]
    pub status: Option<String>,
    /// Substring filter on key and name.
    pub filter: Option<String>,
    /// Include ignored models.
    #[arg(long)]
    pub all: bool,
    /// Maximum rows (0 = unlimited).
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `show`.
#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Model key, listing, or unambiguous name.
    pub model: String,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `history`.
#[derive(Debug, Args)]
pub struct HistoryArgs {
    /// Only events within this long (`7d`).
    #[arg(long, value_name = "DURATION")]
    pub since: Option<String>,
    /// Only events from this source.
    #[arg(long)]
    pub source: Option<String>,
    /// Maximum rows.
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `config` subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the configuration and data paths.
    Path,
    /// Print the composed configuration as TOML.
    Show,
    /// Write a commented starter configuration to the global path.
    Init {
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Validate the configuration.
    Check,
}

/// `auth` subcommands.
///
/// Interactive when stdin and stderr are terminals; otherwise nothing prompts and missing
/// choices fail with the flag to pass.
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// List sources that take an API key and whether one is configured.
    Providers(JsonArgs),
    /// Show where each source's key comes from, what it overrides, and its last sync result.
    Status(StatusArgs),
    /// Store a source's API key in the vault (verifies it first).
    Login(LoginArgs),
    /// Check configured keys against each provider's model list.
    Verify(VerifyArgs),
    /// Delete stored API keys.
    Logout(LogoutArgs),
    /// List API keys other tools keep on this machine (never prints values).
    Discover(JsonArgs),
    /// Copy a discovered key into the vault.
    Import(ImportArgs),
}

/// `auth status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Only this source.
    pub source: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `auth login`.
#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Source id (`openai`). Omit to pick interactively.
    pub source: Option<String>,
    /// Where the key comes from (default: a hidden prompt).
    #[command(flatten)]
    pub input: ValueInput,
    /// Replace a key already in the vault.
    #[arg(long)]
    pub replace: bool,
    /// Store without checking the key against the provider.
    #[arg(long)]
    pub no_verify: bool,
    /// Do not open the provider's key page in a browser.
    #[arg(long)]
    pub no_open: bool,
}

/// `auth verify`.
#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Sources to check (default: every enabled source with a key configured).
    pub sources: Vec<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// `auth logout`.
#[derive(Debug, Args)]
pub struct LogoutArgs {
    /// Source id. Omit to pick interactively.
    #[arg(conflicts_with = "all")]
    pub source: Option<String>,
    /// Every stored API key.
    #[arg(long)]
    pub all: bool,
    /// Do not ask for confirmation (required when not interactive).
    #[arg(long)]
    pub yes: bool,
}

/// `auth import`.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// Source id the key is for.
    pub source: String,
    /// Index shown by `bmodels auth discover`.
    #[arg(long, value_name = "INDEX")]
    pub from: usize,
    /// Replace a key already in the vault.
    #[arg(long)]
    pub replace: bool,
    /// Do not ask for confirmation (required when not interactive).
    #[arg(long)]
    pub yes: bool,
}

/// `secret` subcommands. Names are `profile/VAR` (`notify/WEBHOOK`).
#[derive(Debug, Subcommand)]
pub enum SecretCommand {
    /// List vault entries and where configuration uses them (never prints values).
    List(JsonArgs),
    /// Store a value.
    Set(SecretSetArgs),
    /// Delete a value.
    Unset(SecretUnsetArgs),
    /// Find plaintext secrets in configuration (exit code 1 if any).
    Check(JsonArgs),
}

/// `secret set`.
#[derive(Debug, Args)]
pub struct SecretSetArgs {
    /// Entry name, `profile/VAR`.
    pub name: String,
    /// Where the value comes from (default: a hidden prompt).
    #[command(flatten)]
    pub input: ValueInput,
    /// Replace an existing value.
    #[arg(long)]
    pub replace: bool,
}

/// `secret unset`.
#[derive(Debug, Args)]
pub struct SecretUnsetArgs {
    /// Entry name, `profile/VAR`.
    pub name: String,
    /// Do not ask for confirmation (required when not interactive).
    #[arg(long)]
    pub yes: bool,
}

/// Non-interactive ways to supply a secret. Values are never accepted as arguments.
#[derive(Debug, Args)]
pub struct ValueInput {
    /// Read the value from stdin (`op read ... | bmodels auth login openai --stdin`).
    #[arg(long, visible_alias = "key-stdin", conflicts_with = "from_env")]
    pub stdin: bool,
    /// Copy the value from this environment variable.
    #[arg(long, value_name = "VAR")]
    pub from_env: Option<String>,
}
