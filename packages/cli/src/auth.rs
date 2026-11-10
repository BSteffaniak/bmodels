//! `auth` and `secret` commands.
//!
//! Every command works without a terminal. Interactive runs prompt for anything missing
//! (source, key, confirmation); non-interactive runs fail with the flag to pass instead.
//! Values are only ever read through hidden prompts, stdin, or an environment variable, never
//! from arguments, and are never printed.

use std::collections::{BTreeMap, BTreeSet};
use std::process::ExitCode;

use anyhow::Context as _;
use bmodels_auth::{
    AuthVault, Discovered, SecurityAction, SecurityReport, VaultLookup, discover, keyed_sources,
};
use bmodels_model::{
    API_KEY_VAR, Config, Secret, SecretName, SecretRef, SecretValue, SourceHealth, SourceSpec,
};
use bmodels_sources::{
    FetchContext, FetchOutcome, Http, SecretLookup, SourceError, SwitchyHttp, Validators,
};
use serde::Serialize;

use crate::args::{
    AuthCommand, ImportArgs, JsonArgs, LoginArgs, LogoutArgs, SecretCommand, SecretSetArgs,
    SecretUnsetArgs, StatusArgs, ValueInput, VerifyArgs,
};
use crate::context::{Context, block_on};
use crate::output::{self, Paint, Table, cell, paint, styled};
use crate::prompt;

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Runs an `auth` subcommand.
pub fn auth(ctx: &Context, command: &AuthCommand) -> anyhow::Result<ExitCode> {
    match command {
        AuthCommand::Providers(args) => providers(ctx, args),
        AuthCommand::Status(args) => status(ctx, args),
        AuthCommand::Login(args) => login(ctx, args),
        AuthCommand::Verify(args) => verify(ctx, args),
        AuthCommand::Logout(args) => logout(ctx, args),
        AuthCommand::Discover(args) => discover_cmd(ctx, args),
        AuthCommand::Import(args) => import(ctx, args),
    }
}

/// Runs a `secret` subcommand.
pub fn secret(ctx: &Context, command: &SecretCommand) -> anyhow::Result<ExitCode> {
    match command {
        SecretCommand::List(args) => secret_list(ctx, args),
        SecretCommand::Set(args) => secret_set(ctx, args),
        SecretCommand::Unset(args) => secret_unset(ctx, args),
        SecretCommand::Check(args) => secret_check(ctx, args),
    }
}

// ------------------------------------------------------------------ key origins

/// Source ids with an API key in the vault, or the reason the vault cannot be read.
fn stored_keys(vault: &AuthVault) -> Result<BTreeSet<String>, String> {
    if !vault.exists() {
        return Ok(BTreeSet::new());
    }
    vault
        .names()
        .map(|names| {
            names
                .into_iter()
                .filter(|n| n.var == API_KEY_VAR)
                .map(|n| n.profile)
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// Every place a source's key could come from, in lookup order (first wins). Commands are not
/// run: a configured `api_key_cmd` counts as present.
fn key_origins(id: &str, spec: &SourceSpec, stored: &BTreeSet<String>) -> Vec<String> {
    let mut origins = Vec::new();
    if spec.api_key_cmd.is_some() {
        origins.push("api_key_cmd".to_owned());
    }
    if stored.contains(id) {
        origins.push(format!("vault ({id}/{API_KEY_VAR})"));
    }
    if let Some(var) = &spec.api_key_env
        && env_var(var).is_some()
    {
        origins.push(format!("env ({var})"));
    }
    origins
}

fn source<'a>(config: &'a Config, id: &str) -> anyhow::Result<&'a SourceSpec> {
    config
        .sources
        .get(id)
        .with_context(|| format!("unknown source `{id}` (see `bmodels auth providers`)"))
}

fn print_report(report: &SecurityReport) {
    for action in &report.actions {
        if matches!(
            action,
            SecurityAction::BoundDeviceSeal | SecurityAction::ReboundDeviceSeal
        ) {
            eprintln!("{}", paint(Paint::Dim, "bound to this device"));
        }
    }
    if let Some(reason) = &report.skipped {
        eprintln!(
            "{}",
            paint(
                Paint::Yellow,
                &format!("note: device seal not applied ({reason})")
            )
        );
    }
}

// ------------------------------------------------------------------ providers / status

#[derive(Serialize)]
struct ProviderJson<'a> {
    id: &'a str,
    name: &'a str,
    env: Option<&'a str>,
    key_url: Option<&'a str>,
    optional: bool,
    configured: Option<String>,
}

fn providers(ctx: &Context, args: &JsonArgs) -> anyhow::Result<ExitCode> {
    let vault = ctx.vault();
    let stored = stored_keys(&vault).unwrap_or_default();
    let rows: Vec<ProviderJson<'_>> = keyed_sources(ctx.config(), &stored)
        .into_iter()
        .map(|(id, spec)| ProviderJson {
            id,
            name: spec.display_name(id),
            env: spec.api_key_env.as_deref(),
            key_url: spec.key_url.as_deref(),
            optional: spec.api_key_optional,
            configured: key_origins(id, spec, &stored).into_iter().next(),
        })
        .collect();
    if args.json {
        output::json(&rows)?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut table = Table::default();
    table.row(vec![
        cell(""),
        styled("SOURCE", Paint::Dim),
        styled("KEY", Paint::Dim),
        styled("ENV", Paint::Dim),
        styled("GET A KEY", Paint::Dim),
    ]);
    for r in &rows {
        let (mark, style, key) = match &r.configured {
            Some(origin) => ("●", Paint::Green, origin.clone()),
            None if r.optional => ("○", Paint::Dim, "optional".to_owned()),
            None => ("○", Paint::Yellow, "missing".to_owned()),
        };
        table.row(vec![
            styled(mark, style),
            cell(format!("{} ({})", r.name, r.id)),
            styled(key, style),
            styled(r.env.unwrap_or("–"), Paint::Dim),
            styled(r.key_url.unwrap_or(""), Paint::Dim),
        ]);
    }
    table.print();
    Ok(ExitCode::SUCCESS)
}

#[derive(Serialize)]
struct StatusJson<'a> {
    id: &'a str,
    name: &'a str,
    key: Option<String>,
    overridden: Vec<String>,
    device_sealed: Option<bool>,
    health: SourceHealth,
    detail: Option<String>,
}

#[derive(Serialize)]
struct VaultJson {
    path: String,
    exists: bool,
    device_seal: &'static str,
    error: Option<String>,
}

fn status(ctx: &Context, args: &StatusArgs) -> anyhow::Result<ExitCode> {
    let config = ctx.config();
    let vault = ctx.vault();
    let stored = stored_keys(&vault);
    let stored_set = stored.clone().unwrap_or_default();
    if let Some(id) = &args.source {
        source(config, id)?;
    }
    let states = block_on(async {
        let store = ctx.store().await?;
        anyhow::Ok(store.source_states().await?)
    })??;
    let mut rows = Vec::new();
    for (id, spec) in keyed_sources(config, &stored_set) {
        if args.source.as_ref().is_some_and(|s| s != id) {
            continue;
        }
        let mut origins = key_origins(id, spec, &stored_set).into_iter();
        let key = origins.next();
        let device_sealed = key
            .as_deref()
            .filter(|k| k.starts_with("vault"))
            .map(|_| vault.inspect(id).profile_device_sealed);
        let state = states.get(id.as_str());
        rows.push(StatusJson {
            id,
            name: spec.display_name(id),
            key,
            overridden: origins.collect(),
            device_sealed,
            health: state.map_or(SourceHealth::Never, bmodels_model::SourceState::health),
            detail: state.and_then(|s| s.last_error.clone().or_else(|| s.skipped.clone())),
        });
    }
    let vault_info = VaultJson {
        path: vault.path().display().to_string(),
        exists: vault.exists(),
        device_seal: vault.device_seal().as_str(),
        error: stored.err(),
    };
    let missing = rows.iter().any(|r| r.key.is_none());
    if args.json {
        #[derive(Serialize)]
        struct Out<'a> {
            vault: VaultJson,
            sources: Vec<StatusJson<'a>>,
        }
        output::json(&Out {
            vault: vault_info,
            sources: rows,
        })?;
    } else {
        print_status(&vault_info, &rows);
    }
    Ok(if args.source.is_some() && missing {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn print_status(vault_info: &VaultJson, rows: &[StatusJson<'_>]) {
    println!(
        "{} {}{}  {}",
        paint(Paint::Dim, "vault"),
        vault_info.path,
        if vault_info.exists {
            ""
        } else {
            " (created on first login)"
        },
        paint(
            Paint::Dim,
            &format!("device seal: {}", vault_info.device_seal)
        )
    );
    if let Some(e) = &vault_info.error {
        println!("{}", paint(Paint::Red, &format!("vault unavailable: {e}")));
    }
    let mut table = Table::default();
    table.row(vec![
        cell(""),
        styled("SOURCE", Paint::Dim),
        styled("KEY FROM", Paint::Dim),
        styled("ALSO SET", Paint::Dim),
        styled("LAST SYNC", Paint::Dim),
    ]);
    for r in rows {
        let (mark, style) = match (&r.key, r.health) {
            (None, _) => ("○", Paint::Yellow),
            (Some(_), SourceHealth::Error) => ("✗", Paint::Red),
            (Some(_), _) => ("●", Paint::Green),
        };
        let key = r.key.clone().map_or_else(
            || format!("missing: `bmodels auth login {}`", r.id),
            |k| match r.device_sealed {
                Some(true) => format!("{k}, device-sealed"),
                _ => k,
            },
        );
        let last = match r.health {
            SourceHealth::Ok => "ok".to_owned(),
            SourceHealth::Never => "never synced".to_owned(),
            SourceHealth::Skipped | SourceHealth::Error => r.detail.clone().unwrap_or_default(),
        };
        table.row(vec![
            styled(mark, style),
            cell(format!("{} ({})", r.name, r.id)),
            styled(key, style),
            styled(r.overridden.join(", "), Paint::Dim),
            styled(
                last,
                if r.health == SourceHealth::Error {
                    Paint::Red
                } else {
                    Paint::Dim
                },
            ),
        ]);
    }
    table.print();
}

// ------------------------------------------------------------------ verification

/// Result of checking one key.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "SCREAMING_SNAKE_CASE")]
enum Check {
    /// The provider listed its models.
    Ok { models: usize },
    /// The provider rejected the key.
    Rejected { detail: String },
    /// The check could not complete (network, configuration).
    Failed { detail: String },
    /// No key is configured.
    Missing { detail: String },
}

impl Check {
    const fn passed(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    fn describe(&self) -> (String, Paint) {
        match self {
            Self::Ok { models } => (format!("ok ({models} models)"), Paint::Green),
            Self::Rejected { detail } => (format!("rejected: {detail}"), Paint::Red),
            Self::Failed { detail } => (format!("could not check: {detail}"), Paint::Yellow),
            Self::Missing { detail } => (detail.clone(), Paint::Yellow),
        }
    }
}

/// Lists `id`'s models with `key` (and its configured headers).
async fn check_key(
    http: &dyn Http,
    lookup: &dyn SecretLookup,
    id: &str,
    spec: &SourceSpec,
    key: Secret,
) -> Check {
    let headers = match spec
        .headers
        .iter()
        .map(|(name, value)| {
            bmodels_sources::resolve_value(value, lookup).map(|s| (name.clone(), s))
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(h) => h,
        Err(e) => {
            return Check::Failed {
                detail: e.to_string(),
            };
        }
    };
    let ctx = FetchContext {
        http,
        api_key: Some(key),
        headers,
        validators: Validators::default(),
    };
    let secrets: Vec<&Secret> = ctx
        .api_key
        .iter()
        .chain(ctx.headers.iter().map(|(_, s)| s))
        .collect();
    match bmodels_sources::fetch(id, spec, &ctx).await {
        Ok(FetchOutcome::Fetched(f)) => Check::Ok {
            models: f.records.len(),
        },
        Ok(FetchOutcome::NotModified) => Check::Ok { models: 0 },
        Err(SourceError::Status { status, .. }) if status == 401 || status == 403 => {
            Check::Rejected {
                detail: format!("HTTP {status}"),
            }
        }
        Err(e) => Check::Failed {
            detail: e.redacted(&secrets).to_string(),
        },
    }
}

fn verify(ctx: &Context, args: &VerifyArgs) -> anyhow::Result<ExitCode> {
    let config = ctx.config();
    let vault = ctx.vault();
    let lookup = VaultLookup::new(Some(&vault));
    let stored = stored_keys(&vault).unwrap_or_default();
    let ids: Vec<String> = if args.sources.is_empty() {
        keyed_sources(config, &stored)
            .into_iter()
            .filter(|(id, spec)| spec.enabled && !key_origins(id, spec, &stored).is_empty())
            .map(|(id, _)| id.clone())
            .collect()
    } else {
        for id in &args.sources {
            source(config, id)?;
        }
        args.sources.clone()
    };
    let http = SwitchyHttp::new(ctx.timeout())?;
    let results = block_on(async {
        let mut results = Vec::new();
        for id in &ids {
            let spec = &config.sources[id];
            let check = match bmodels_sources::resolve_key(id, spec, &lookup) {
                Ok(Some(key)) => check_key(&http, &lookup, id, spec, key.secret).await,
                Ok(None) => Check::Missing {
                    detail: format!("no key: `bmodels auth login {id}`"),
                },
                Err(e) => Check::Missing {
                    detail: e.to_string(),
                },
            };
            results.push((id.clone(), check));
        }
        results
    })?;
    let ok = results.iter().all(|(_, c)| c.passed());
    if args.json {
        #[derive(Serialize)]
        struct Row<'a> {
            source: &'a str,
            #[serde(flatten)]
            check: &'a Check,
        }
        let rows: Vec<Row<'_>> = results
            .iter()
            .map(|(source, check)| Row { source, check })
            .collect();
        output::json(&rows)?;
    } else if results.is_empty() {
        println!("no source has a key configured (see `bmodels auth providers`)");
    } else {
        let mut table = Table::default();
        for (id, check) in &results {
            let (text, style) = check.describe();
            table.row(vec![
                styled(if check.passed() { "✓" } else { "✗" }, style),
                cell(config.sources[id].display_name(id).to_owned()),
                styled(text, style),
            ]);
        }
        table.print();
    }
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

// ------------------------------------------------------------------ login / import / logout

/// Verifies (unless `skip_verify`) and stores `key` as `id`'s API key.
fn store_key(
    ctx: &Context,
    id: &str,
    key: Secret,
    replace: bool,
    skip_verify: bool,
) -> anyhow::Result<()> {
    let spec = source(ctx.config(), id)?;
    let vault = ctx.vault();
    if !skip_verify {
        let lookup = VaultLookup::new(Some(&vault));
        let http = SwitchyHttp::new(ctx.timeout())?;
        eprint!("{}", paint(Paint::Dim, "checking the key... "));
        let check = block_on(check_key(&http, &lookup, id, spec, key.clone()))?;
        let (text, style) = check.describe();
        eprintln!("{}", paint(style, &text));
        match check {
            Check::Rejected { .. } => {
                anyhow::bail!(
                    "{} rejected the key; nothing was saved",
                    spec.display_name(id)
                )
            }
            Check::Failed { .. } => eprintln!(
                "{}",
                paint(
                    Paint::Yellow,
                    "saving anyway; run `bmodels auth verify` once the provider is reachable"
                )
            ),
            Check::Ok { .. } | Check::Missing { .. } => {}
        }
    }
    let name = SecretName::api_key(id)?;
    let report = if replace {
        vault.set(&name, key)
    } else {
        vault.insert_if_absent(&name, key)
    }
    .with_context(|| format!("storing {name}"))?;
    println!(
        "{} saved {} key to {}",
        paint(Paint::Green, "✓"),
        spec.display_name(id),
        vault.path().display()
    );
    print_report(&report);
    if let Some(var) = &spec.api_key_env
        && env_var(var).is_some()
    {
        eprintln!(
            "{}",
            paint(
                Paint::Dim,
                &format!("{var} is also set; the vault key takes precedence")
            )
        );
    }
    if spec.api_key_cmd.is_some() {
        eprintln!(
            "{}",
            paint(
                Paint::Yellow,
                "note: this source sets `api_key_cmd`, which takes precedence over the vault"
            )
        );
    }
    Ok(())
}

/// Whether the vault already has `id`'s key, asking to replace it if so. Returns `false` when
/// the user declines.
fn confirm_replace(vault: &AuthVault, id: &str, replace: bool) -> anyhow::Result<bool> {
    let exists = vault.exists() && vault.get(&SecretName::api_key(id)?)?.is_some();
    if !exists || replace {
        return Ok(true);
    }
    prompt::require(
        &format!("the vault already has a key for {id}"),
        "pass --replace to overwrite it",
    )?;
    prompt::confirm(&format!("Replace the stored key for {id}?"), false)
}

fn pick_source(ctx: &Context, stored: &BTreeSet<String>) -> anyhow::Result<String> {
    prompt::require(
        "no source given",
        "pass the source id (see `bmodels auth providers`)",
    )?;
    let sources = keyed_sources(ctx.config(), stored);
    let labels: Vec<String> = sources
        .iter()
        .map(|(id, spec)| {
            let state = key_origins(id, spec, stored)
                .into_iter()
                .next()
                .map_or_else(|| "no key".to_owned(), |o| format!("key from {o}"));
            format!("{} ({id}) - {state}", spec.display_name(id))
        })
        .collect();
    let i = prompt::choose("Which source?", &labels)?;
    Ok(sources[i].0.clone())
}

fn login(ctx: &Context, args: &LoginArgs) -> anyhow::Result<ExitCode> {
    let vault = ctx.vault();
    let stored = stored_keys(&vault).unwrap_or_default();
    let id = match &args.source {
        Some(id) => id.clone(),
        None => pick_source(ctx, &stored)?,
    };
    let spec = source(ctx.config(), &id)?;
    if !confirm_replace(&vault, &id, args.replace)? {
        println!("kept the stored key");
        return Ok(ExitCode::SUCCESS);
    }
    let key = if let Some(key) = supplied(&args.input)? {
        key
    } else {
        prompt::require(
            "no key given",
            "pass --stdin (pipe the key in) or --from-env VAR",
        )?;
        if let Some(url) = &spec.key_url {
            eprintln!("Create a key at {}", paint(Paint::Cyan, url));
            if !args.no_open && open::that_detached(url).is_ok() {
                eprintln!("{}", paint(Paint::Dim, "(opened in your browser)"));
            }
        }
        prompt::hidden(&format!("{} API key: ", spec.display_name(&id)))?
    };
    store_key(ctx, &id, key, true, args.no_verify)?;
    Ok(ExitCode::SUCCESS)
}

/// The value from `--stdin` or `--from-env`, if either was given.
fn supplied(input: &ValueInput) -> anyhow::Result<Option<Secret>> {
    if input.stdin {
        return prompt::stdin_secret().map(Some);
    }
    input
        .from_env
        .as_ref()
        .map(|var| {
            env_var(var)
                .map(Secret::new)
                .with_context(|| format!("{var} is not set"))
        })
        .transpose()
}

fn logout(ctx: &Context, args: &LogoutArgs) -> anyhow::Result<ExitCode> {
    let vault = ctx.vault();
    let stored = stored_keys(&vault).map_err(anyhow::Error::msg)?;
    let ids: Vec<String> = if args.all {
        stored.iter().cloned().collect()
    } else if let Some(id) = &args.source {
        vec![id.clone()]
    } else {
        prompt::require("no source given", "pass the source id or --all")?;
        let labels: Vec<String> = stored.iter().cloned().collect();
        anyhow::ensure!(!labels.is_empty(), "the vault has no API keys");
        vec![labels[prompt::choose("Remove which key?", &labels)?].clone()]
    };
    if ids.is_empty() {
        println!("the vault has no API keys");
        return Ok(ExitCode::SUCCESS);
    }
    if !args.yes {
        prompt::require("removal needs confirmation", "pass --yes")?;
        if !prompt::confirm(
            &format!("Remove stored key(s) for {}?", ids.join(", ")),
            false,
        )? {
            return Ok(ExitCode::SUCCESS);
        }
    }
    let mut missing = false;
    for id in &ids {
        if vault.unset(&SecretName::api_key(id)?)? {
            println!("{} removed {id}", paint(Paint::Green, "✓"));
        } else {
            println!("{} no stored key for {id}", paint(Paint::Yellow, "-"));
            missing = true;
        }
    }
    Ok(if missing && !args.all {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn discovered(ctx: &Context) -> Vec<Discovered> {
    discover(ctx.config(), std::env::home_dir().as_deref(), &env_var)
}

fn discover_cmd(ctx: &Context, args: &JsonArgs) -> anyhow::Result<ExitCode> {
    let found = discovered(ctx);
    if args.json {
        #[derive(Serialize)]
        struct Row {
            index: usize,
            source: String,
            origin: String,
            importable: bool,
        }
        let rows: Vec<Row> = found
            .iter()
            .enumerate()
            .map(|(i, d)| Row {
                index: i + 1,
                source: d.source.clone(),
                origin: d.origin.to_string(),
                importable: d.origin.importable(),
            })
            .collect();
        output::json(&rows)?;
        return Ok(ExitCode::SUCCESS);
    }
    if found.is_empty() {
        println!("no API keys found (environment, Codex, Claude Code settings)");
        return Ok(ExitCode::SUCCESS);
    }
    let mut table = Table::default();
    for (i, d) in found.iter().enumerate() {
        let note = match &d.origin {
            bmodels_auth::DiscoveryOrigin::ClaudeKeyHelper { command, .. } => format!(
                "set `api_key_cmd = {command:?}` under [sources.{}]",
                d.source
            ),
            _ => format!("bmodels auth import {} --from {}", d.source, i + 1),
        };
        table.row(vec![
            styled(format!("{:>2}", i + 1), Paint::Dim),
            cell(d.source.clone()),
            cell(d.origin.to_string()),
            styled(note, Paint::Dim),
        ]);
    }
    table.print();
    Ok(ExitCode::SUCCESS)
}

fn import(ctx: &Context, args: &ImportArgs) -> anyhow::Result<ExitCode> {
    source(ctx.config(), &args.source)?;
    let found = discovered(ctx);
    let d = args
        .from
        .checked_sub(1)
        .and_then(|i| found.get(i))
        .with_context(|| {
            format!(
                "no discovered key #{} (see `bmodels auth discover`)",
                args.from
            )
        })?;
    anyhow::ensure!(
        d.source == args.source,
        "discovered key #{} is for {}, not {}",
        args.from,
        d.source,
        args.source
    );
    let vault = ctx.vault();
    if !confirm_replace(&vault, &args.source, args.replace)? {
        return Ok(ExitCode::SUCCESS);
    }
    if !args.yes {
        prompt::require("import needs confirmation", "pass --yes")?;
        eprintln!("Copy the key from {}", d.origin);
        eprintln!(
            "into {} as {}/{API_KEY_VAR}",
            vault.path().display(),
            args.source
        );
        if !prompt::confirm("Import it?", true)? {
            return Ok(ExitCode::SUCCESS);
        }
    }
    let key = d.read(&env_var)?;
    store_key(ctx, &args.source, key, true, false)?;
    Ok(ExitCode::SUCCESS)
}

// ------------------------------------------------------------------ secret

/// Where configuration references each vault entry.
fn references(config: &Config) -> BTreeMap<String, Vec<String>> {
    let mut refs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut note = |value: &SecretValue, key: String| {
        if let SecretValue::Ref(SecretRef::Vault(name)) = value {
            refs.entry(name.clone()).or_default().push(key);
        }
    };
    for (id, spec) in &config.sources {
        for (header, value) in &spec.headers {
            note(value, format!("sources.{id}.headers.{header}"));
        }
    }
    if let Some(webhook) = &config.notify.webhook {
        note(webhook, "notify.webhook".to_owned());
    }
    for id in config.sources.keys() {
        refs.entry(format!("{id}/{API_KEY_VAR}"))
            .or_default()
            .push(format!("sources.{id} API key"));
    }
    refs
}

fn secret_list(ctx: &Context, args: &JsonArgs) -> anyhow::Result<ExitCode> {
    let vault = ctx.vault();
    let names = if vault.exists() {
        vault.names()?
    } else {
        Vec::new()
    };
    let refs = references(ctx.config());
    if args.json {
        #[derive(Serialize)]
        struct Row {
            name: String,
            used_by: Vec<String>,
        }
        let rows: Vec<Row> = names
            .iter()
            .map(|n| Row {
                name: n.to_string(),
                used_by: refs.get(&n.to_string()).cloned().unwrap_or_default(),
            })
            .collect();
        output::json(&rows)?;
        return Ok(ExitCode::SUCCESS);
    }
    println!("{} {}", paint(Paint::Dim, "vault"), vault.path().display());
    if names.is_empty() {
        println!("no secrets stored");
        return Ok(ExitCode::SUCCESS);
    }
    let mut table = Table::default();
    for n in &names {
        let used = refs.get(&n.to_string()).map(|u| u.join(", "));
        table.row(vec![
            cell(n.to_string()),
            used.map_or_else(
                || styled("unused", Paint::Yellow),
                |u| styled(u, Paint::Dim),
            ),
        ]);
    }
    table.print();
    Ok(ExitCode::SUCCESS)
}

fn secret_set(ctx: &Context, args: &SecretSetArgs) -> anyhow::Result<ExitCode> {
    let name = SecretName::parse(&args.name)?;
    let vault = ctx.vault();
    let exists = vault.exists() && vault.get(&name)?.is_some();
    if exists && !args.replace {
        prompt::require(
            &format!("{name} already exists"),
            "pass --replace to overwrite it",
        )?;
        if !prompt::confirm(&format!("Replace {name}?"), false)? {
            return Ok(ExitCode::SUCCESS);
        }
    }
    let value = if let Some(value) = supplied(&args.input)? {
        value
    } else {
        prompt::require("no value given", "pass --stdin or --from-env VAR")?;
        prompt::hidden(&format!("{name}: "))?
    };
    let report = vault.set(&name, value)?;
    println!("{} saved {name}", paint(Paint::Green, "✓"));
    print_report(&report);
    if !references(ctx.config()).contains_key(&name.to_string()) {
        eprintln!(
            "{}",
            paint(
                Paint::Dim,
                &format!("reference it in config as {{ vault = \"{name}\" }}")
            )
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn secret_unset(ctx: &Context, args: &SecretUnsetArgs) -> anyhow::Result<ExitCode> {
    let name = SecretName::parse(&args.name)?;
    if !args.yes {
        prompt::require("removal needs confirmation", "pass --yes")?;
        if !prompt::confirm(&format!("Remove {name}?"), false)? {
            return Ok(ExitCode::SUCCESS);
        }
    }
    if ctx.vault().unset(&name)? {
        println!("{} removed {name}", paint(Paint::Green, "✓"));
        Ok(ExitCode::SUCCESS)
    } else {
        println!("{} {name} was not stored", paint(Paint::Yellow, "-"));
        Ok(ExitCode::FAILURE)
    }
}

fn secret_check(ctx: &Context, args: &JsonArgs) -> anyhow::Result<ExitCode> {
    let findings = bmodels_auth::plaintext_secrets(ctx.config());
    if args.json {
        #[derive(Serialize)]
        struct Row {
            key: String,
            suggestion: String,
            replacement: String,
        }
        let rows: Vec<Row> = findings
            .iter()
            .map(|f| Row {
                key: f.key.clone(),
                suggestion: f.suggestion.to_string(),
                replacement: f.replacement(),
            })
            .collect();
        output::json(&rows)?;
    } else if findings.is_empty() {
        println!(
            "{} no plaintext secrets in configuration",
            paint(Paint::Green, "✓")
        );
    } else {
        print_findings(&findings);
    }
    Ok(if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Prints how to move each plaintext secret into the vault.
pub fn print_findings(findings: &[bmodels_auth::PlaintextFinding]) {
    for f in findings {
        println!(
            "{} {} is plaintext; move it to the vault:",
            paint(Paint::Yellow, "!"),
            f.key
        );
        println!(
            "    bmodels secret set {}   then   {} = {}",
            f.suggestion,
            f.key.rsplit('.').next().unwrap_or(&f.key),
            f.replacement()
        );
    }
}
