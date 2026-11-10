//! Command implementations.

use std::collections::BTreeSet;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use bmodels_auth::VaultLookup;
use bmodels_engine::{Engine, Progress, Selection, SourceOutcome, SyncReport, notices};
use bmodels_model::display::{format_age, format_ago, format_date, format_datetime, format_tokens};
use bmodels_model::{
    AckTarget, Event, EventKind, ModelStatus, SecretValue, SourceHealth, Timestamp,
};
use bmodels_notify::{Digest, NotifyItem};
use bmodels_sources::SwitchyHttp;
use bmodels_store::{EventQuery, Store};
use serde::Serialize;

use crate::args::{
    AckArgs, Cli, Commands, ConfigCommand, HistoryArgs, JsonArgs, KeyArgs, ListArgs, NewArgs,
    NoteArgs, ShowArgs, SyncArgs, WatchArgs,
};
use crate::context::{Context, block_on, now};
use crate::output::{self, Paint, Table, cell, kind_paint, paint, styled};
use crate::resolve::{Resolved, resolve};
use crate::tui_backend::{self, source_rows};

/// Runs `cli`.
pub fn dispatch(cli: Cli) -> anyhow::Result<ExitCode> {
    if matches!(&cli.command, Some(Commands::Config(ConfigCommand::Path))) {
        return config_path(&cli);
    }
    let ctx = Context::load(&cli)?;
    match cli.command.unwrap_or(Commands::Tui) {
        Commands::Tui => tui(&ctx),
        Commands::Sync(args) => sync(&ctx, &args),
        Commands::Watch(args) => watch(&ctx, &args),
        Commands::New(args) => new(&ctx, &args),
        Commands::Ack(args) => ack(&ctx, &args),
        Commands::Star(args) => set_status(&ctx, &args, ModelStatus::Starred, "starred"),
        Commands::Unstar(args) | Commands::Unignore(args) => {
            set_status(&ctx, &args, ModelStatus::Seen, "cleared")
        }
        Commands::Ignore(args) => set_status(&ctx, &args, ModelStatus::Ignored, "ignored"),
        Commands::Unsee(args) => set_status(&ctx, &args, ModelStatus::Unseen, "marked unseen"),
        Commands::Note(args) => note(&ctx, &args),
        Commands::List(args) => list(&ctx, &args),
        Commands::Show(args) => show(&ctx, &args),
        Commands::History(args) => history(&ctx, &args),
        Commands::Sources(args) => sources(&ctx, &args),
        Commands::Auth(command) => crate::auth::auth(&ctx, &command),
        Commands::Secret(command) => crate::auth::secret(&ctx, &command),
        Commands::Config(command) => config(&ctx, &command),
    }
}

fn parse_since(since: Option<&String>) -> anyhow::Result<Option<Timestamp>> {
    since
        .map(|s| {
            let d = bmodels_model::parse_duration(s)?;
            Ok(now() - i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        })
        .transpose()
}

// ------------------------------------------------------------------ tui

fn tui(ctx: &Context) -> anyhow::Result<ExitCode> {
    // Log lines would draw over the UI; errors surface in the status bar instead.
    log::set_max_level(log::LevelFilter::Off);
    let backend = tui_backend::StoreBackend::new(ctx.clone());
    bmodels_tui::run(backend)?;
    Ok(ExitCode::SUCCESS)
}

// ------------------------------------------------------------------ sync / watch

fn sync(ctx: &Context, args: &SyncArgs) -> anyhow::Result<ExitCode> {
    let selection = if args.due {
        Selection::Due
    } else if args.sources.is_empty() {
        Selection::Enabled
    } else {
        Selection::Ids(args.sources.clone())
    };
    let live = !args.quiet && !args.json;
    let report = block_on(async {
        let store = ctx.store().await?;
        let report = run_sync(ctx, &store, &selection, live).await?;
        if args.notify {
            notify(ctx, &store).await?;
        }
        anyhow::Ok(report)
    })??;
    if args.json {
        output::json(&report_json(&report))?;
    } else {
        output::print_summary(&report, args.quiet);
    }
    Ok(if report.failures().next().is_some() {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

async fn run_sync(
    ctx: &Context,
    store: &Store,
    selection: &Selection,
    live: bool,
) -> anyhow::Result<SyncReport> {
    let http = SwitchyHttp::new(ctx.timeout())?;
    let vault = ctx.vault();
    let secrets = VaultLookup::new(Some(&vault));
    let engine = Engine::new(ctx.config(), store, &http, &secrets);
    let progress = |p: &Progress| {
        if !live {
            return;
        }
        match p {
            Progress::Planned(ids) if ids.is_empty() => {
                println!("{}", paint(Paint::Dim, "nothing to sync"));
            }
            Progress::Finished(r) if matches!(r.outcome, SourceOutcome::Skipped(_)) => {}
            Progress::Finished(r) => {
                let (mark, style) = match &r.outcome {
                    SourceOutcome::Synced(c) if c.new > 0 => ("●", Paint::Green),
                    SourceOutcome::Synced(_) | SourceOutcome::NotModified => ("✓", Paint::Dim),
                    SourceOutcome::Skipped(_) => ("-", Paint::Yellow),
                    SourceOutcome::Failed(_) => ("✗", Paint::Red),
                };
                println!(
                    "{} {:<16} {}",
                    paint(style, mark),
                    r.name,
                    output::source_summary(r)
                );
                for w in &r.warnings {
                    println!("  {}", paint(Paint::Yellow, &format!("warning: {w}")));
                }
            }
            Progress::Planned(_) | Progress::Started(_) => {}
        }
    };
    Ok(engine.sync(selection, &progress).await?)
}

#[derive(Serialize)]
struct SourceJson<'a> {
    source: &'a str,
    name: &'a str,
    status: &'static str,
    detail: String,
    new: u64,
    new_listings: u64,
    changed: u64,
    removed: u64,
    present: u64,
    warnings: &'a [String],
}

fn report_json(report: &SyncReport) -> Vec<SourceJson<'_>> {
    report
        .sources
        .iter()
        .map(|r| {
            let (status, c) = match &r.outcome {
                SourceOutcome::Synced(c) => ("SYNCED", *c),
                SourceOutcome::NotModified => ("NOT_MODIFIED", bmodels_engine::Counts::default()),
                SourceOutcome::Skipped(_) => ("SKIPPED", bmodels_engine::Counts::default()),
                SourceOutcome::Failed(_) => ("FAILED", bmodels_engine::Counts::default()),
            };
            SourceJson {
                source: &r.source,
                name: &r.name,
                status,
                detail: output::source_summary(r),
                new: c.new,
                new_listings: c.new_listings,
                changed: c.changed,
                removed: c.removed,
                present: c.present,
                warnings: &r.warnings,
            }
        })
        .collect()
}

/// Delivers pending notifications and marks them; returns how many events were announced.
pub async fn notify(ctx: &Context, store: &Store) -> anyhow::Result<usize> {
    let config = ctx.config();
    let pending = notices::pending(config, store, now()).await?;
    let items: Vec<NotifyItem> = pending
        .notices
        .iter()
        .map(|n| NotifyItem {
            kind: n.event.kind,
            key: n.event.canonical_key.clone(),
            name: n
                .model
                .as_ref()
                .map_or_else(|| n.event.canonical_key.clone(), |m| m.display_name.clone()),
            vendor: n
                .model
                .as_ref()
                .map_or_else(String::new, |m| m.vendor.clone()),
            source: n.event.listing.source.clone(),
            source_name: n.source_name.clone(),
            model_id: n.event.listing.model_id.clone(),
            url: n.listing.as_ref().and_then(|l| l.url.clone()),
            changes: n.event.changes.clone(),
            at: n.event.at,
        })
        .collect();
    let count = items.len();
    let webhook = if items.is_empty() {
        None
    } else {
        config.notify.webhook.as_ref().and_then(|value| {
            let vault = ctx.vault();
            bmodels_sources::resolve_value(value, &VaultLookup::new(Some(&vault)))
                .map_err(|e| eprintln!("bmodels: notify.webhook: {e}"))
                .ok()
        })
    };
    for error in bmodels_notify::deliver(&config.notify, webhook.as_ref(), &Digest { items }).await
    {
        eprintln!("bmodels: {error}");
    }
    store.mark_notified(&pending.ids, now()).await?;
    Ok(count)
}

fn watch(ctx: &Context, args: &WatchArgs) -> anyhow::Result<ExitCode> {
    block_on(async {
        let store = ctx.store().await?;
        loop {
            let report = run_sync(ctx, &store, &Selection::Due, true).await?;
            if !report.sources.is_empty() {
                let totals = report.totals();
                println!(
                    "{} {}",
                    paint(Paint::Dim, &format_datetime(now())),
                    output::counts_summary(&totals)
                );
            }
            if !args.no_notify {
                let sent = notify(ctx, &store).await?;
                if sent > 0 {
                    println!(
                        "{}",
                        paint(Paint::Dim, &format!("notified about {sent} event(s)"))
                    );
                }
            }
            if args.once {
                return anyhow::Ok(ExitCode::SUCCESS);
            }
            let states = store.source_states().await?;
            let next = bmodels_engine::schedule(ctx.config(), &states, now())
                .first()
                .map(|d| d.next_at);
            let wait = next.map_or(3_600, |t| (t - now()).clamp(5, 3_600));
            if let Some(t) = next {
                log::info!("next sync at {}", format_datetime(t));
            }
            tokio::time::sleep(Duration::from_secs(u64::try_from(wait).unwrap_or(60))).await;
        }
    })?
}

// ------------------------------------------------------------------ inbox / ack / status

fn event_detail(e: &Event, source_name: &str) -> String {
    match e.kind {
        EventKind::New => format!("via {source_name}"),
        EventKind::NewListing => format!("on {source_name} as {}", e.listing.model_id),
        EventKind::Changed => e
            .changes
            .iter()
            .map(|c| {
                format!(
                    "{} {}→{}",
                    c.field.replace('_', " "),
                    c.old.as_deref().unwrap_or("–"),
                    c.new.as_deref().unwrap_or("–")
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
        EventKind::Removed => format!("gone from {source_name}{}", as_listing(e)),
        EventKind::Reappeared => format!("back on {source_name}{}", as_listing(e)),
    }
}

/// `" as <model id>"` when the listing's id differs from the model's family (a snapshot).
fn as_listing(e: &Event) -> String {
    let family = e.canonical_key.rsplit('/').next().unwrap_or_default();
    let id = e.listing.model_id.rsplit('/').next().unwrap_or_default();
    if id.eq_ignore_ascii_case(family) {
        String::new()
    } else {
        format!(" as {}", e.listing.model_id)
    }
}

#[derive(Serialize)]
struct EventJson<'a> {
    #[serde(flatten)]
    event: &'a Event,
    name: Option<&'a str>,
    vendor: Option<&'a str>,
}

fn print_events(
    ctx: &Context,
    events: &[Event],
    models: &std::collections::BTreeMap<String, bmodels_model::CanonicalModel>,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        let rows: Vec<EventJson<'_>> = events
            .iter()
            .map(|e| {
                let m = models.get(&e.canonical_key);
                EventJson {
                    event: e,
                    name: m.map(|m| m.display_name.as_str()),
                    vendor: m.map(|m| m.vendor.as_str()),
                }
            })
            .collect();
        return output::json(&rows);
    }
    let t = now();
    let mut table = Table::default();
    for e in events {
        let name = models
            .get(&e.canonical_key)
            .map_or("", |m| m.display_name.as_str());
        let source = ctx
            .config()
            .sources
            .get(&e.listing.source)
            .map_or(e.listing.source.as_str(), |s| {
                s.display_name(&e.listing.source)
            });
        let dim = e.acked_at.is_some();
        table.row(vec![
            styled(format!("#{}", e.id), Paint::Dim),
            styled(
                e.kind.label().to_uppercase(),
                if dim { Paint::Dim } else { kind_paint(e.kind) },
            ),
            styled(format_age(t, e.at), Paint::Dim),
            styled(name, if dim { Paint::Dim } else { Paint::Bold }),
            styled(e.canonical_key.clone(), Paint::Dim),
            cell(event_detail(e, source)),
        ]);
    }
    table.print();
    Ok(())
}

fn new(ctx: &Context, args: &NewArgs) -> anyhow::Result<ExitCode> {
    block_on(async {
        let store = ctx.store().await?;
        let mut events = store
            .events(&EventQuery {
                unacked: true,
                source: args.source.clone(),
                ..EventQuery::default()
            })
            .await?;
        let keys: BTreeSet<String> = events.iter().map(|e| e.canonical_key.clone()).collect();
        let models = store.models_by_key(&keys).await?;
        if let Some(vendor) = &args.vendor {
            events.retain(|e| {
                models
                    .get(&e.canonical_key)
                    .is_some_and(|m| &m.vendor == vendor)
            });
        }
        if events.is_empty() && !args.json {
            let synced = store
                .source_states()
                .await?
                .values()
                .any(|s| s.last_success_at.is_some());
            println!(
                "{}",
                if synced {
                    "all caught up ✓"
                } else {
                    "nothing synced yet — run `bmodels sync`"
                }
            );
            return anyhow::Ok(ExitCode::SUCCESS);
        }
        print_events(ctx, &events, &models, args.json)?;
        if !args.json {
            println!(
                "{}",
                paint(
                    Paint::Dim,
                    "acknowledge with `bmodels ack <model|#id>` or `bmodels ack --all`"
                )
            );
        }
        anyhow::Ok(ExitCode::SUCCESS)
    })?
}

fn ack(ctx: &Context, args: &AckArgs) -> anyhow::Result<ExitCode> {
    block_on(async {
        let store = ctx.store().await?;
        let targets: Vec<(String, AckTarget)> = if args.all {
            vec![(
                "inbox".to_owned(),
                AckTarget::All {
                    source: args.source.clone(),
                    vendor: args.vendor.clone(),
                },
            )]
        } else {
            let mut out = Vec::new();
            for t in &args.targets {
                let resolved = resolve(&store, t).await?;
                let label = match &resolved {
                    Resolved::Model(k) => k.clone(),
                    Resolved::Listing(l, _) => l.to_string(),
                    Resolved::Event(id) => format!("#{id}"),
                };
                out.push((label, resolved.ack_target()));
            }
            out
        };
        for (label, target) in targets {
            let o = store.ack(&target, now()).await?;
            if o.is_empty() {
                println!("{label}: already acknowledged");
            } else {
                println!(
                    "{} {label} ({} event(s), {} listing(s), {} model(s))",
                    paint(Paint::Green, "✓"),
                    o.events,
                    o.listings,
                    o.models
                );
            }
        }
        anyhow::Ok(ExitCode::SUCCESS)
    })?
}

async fn model_key(store: &Store, input: &str) -> anyhow::Result<String> {
    match resolve(store, input).await? {
        Resolved::Event(id) => {
            let event = store
                .events(&EventQuery::default())
                .await?
                .into_iter()
                .find(|e| e.id == id)
                .with_context(|| format!("no event #{id}"))?;
            Ok(event.canonical_key)
        }
        other => Ok(other.model_key().unwrap_or_default().to_owned()),
    }
}

fn set_status(
    ctx: &Context,
    args: &KeyArgs,
    status: ModelStatus,
    label: &str,
) -> anyhow::Result<ExitCode> {
    block_on(async {
        let store = ctx.store().await?;
        let key = model_key(&store, &args.model).await?;
        store.set_status(&key, status, now()).await?;
        println!("{} {key} {label}", paint(Paint::Green, "✓"));
        anyhow::Ok(ExitCode::SUCCESS)
    })?
}

fn note(ctx: &Context, args: &NoteArgs) -> anyhow::Result<ExitCode> {
    block_on(async {
        let store = ctx.store().await?;
        let key = model_key(&store, &args.model).await?;
        let text = args.text.join(" ");
        store
            .set_note(&key, (!text.is_empty()).then_some(text.as_str()))
            .await?;
        println!(
            "{} {key} note {}",
            paint(Paint::Green, "✓"),
            if text.is_empty() { "cleared" } else { "saved" }
        );
        anyhow::Ok(ExitCode::SUCCESS)
    })?
}

// ------------------------------------------------------------------ list / show / history

#[derive(Serialize)]
struct ModelJson {
    #[serde(flatten)]
    model: bmodels_model::CanonicalModel,
    info: bmodels_model::ModelInfo,
    listings: Vec<bmodels_model::ListingKey>,
}

fn list(ctx: &Context, args: &ListArgs) -> anyhow::Result<ExitCode> {
    let status = args
        .status
        .as_deref()
        .map(|s| {
            ModelStatus::parse(&s.to_uppercase())
                .with_context(|| format!("unknown status `{s}` (unseen, seen, starred, ignored)"))
        })
        .transpose()?;
    let since = parse_since(args.since.as_ref())?;
    block_on(async {
        let store = ctx.store().await?;
        let data = tui_backend::load_data(ctx, &store).await?;
        let filter = args.filter.as_deref().map(str::to_lowercase);
        let mut models: Vec<&bmodels_model::CanonicalModel> = data
            .models
            .iter()
            .filter(|m| {
                args.all || status == Some(ModelStatus::Ignored) || m.status != ModelStatus::Ignored
            })
            .filter(|m| status.is_none_or(|s| m.status == s))
            .filter(|m| args.vendor.as_ref().is_none_or(|v| &m.vendor == v))
            .filter(|m| since.is_none_or(|t| m.first_seen_at >= t))
            .filter(|m| {
                args.source.as_ref().is_none_or(|s| {
                    data.listings_of(&m.key)
                        .iter()
                        .any(|l| &l.key.source == s && l.is_present())
                })
            })
            .filter(|m| {
                filter.as_ref().is_none_or(|f| {
                    m.key.contains(f.as_str()) || m.display_name.to_lowercase().contains(f.as_str())
                })
            })
            .collect();
        models.sort_by_key(|m| {
            (
                std::cmp::Reverse(m.first_seen_at),
                std::cmp::Reverse(m.created_at),
                m.key.clone(),
            )
        });
        let total = models.len();
        if args.limit > 0 {
            models.truncate(args.limit);
        }
        if args.json {
            let rows: Vec<ModelJson> = models
                .iter()
                .map(|m| ModelJson {
                    model: (*m).clone(),
                    info: data.merged_info(&m.key),
                    listings: data
                        .listings_of(&m.key)
                        .iter()
                        .map(|l| l.key.clone())
                        .collect(),
                })
                .collect();
            output::json(&rows)?;
            return anyhow::Ok(ExitCode::SUCCESS);
        }
        print_model_table(&data, &models);
        if total > models.len() {
            println!(
                "{}",
                paint(
                    Paint::Dim,
                    &format!("… {} more (use --limit 0 for all)", total - models.len())
                )
            );
        }
        anyhow::Ok(ExitCode::SUCCESS)
    })?
}

fn show(ctx: &Context, args: &ShowArgs) -> anyhow::Result<ExitCode> {
    block_on(async {
        let store = ctx.store().await?;
        let key = model_key(&store, &args.model).await?;
        let data = tui_backend::load_data(ctx, &store).await?;
        let model = data
            .model(&key)
            .with_context(|| format!("no model `{key}`"))?
            .clone();
        let listings: Vec<bmodels_model::Listing> =
            data.listings_of(&key).into_iter().cloned().collect();
        let events = store
            .events(&EventQuery {
                canonical_key: Some(key.clone()),
                ..EventQuery::default()
            })
            .await?;
        let info = data.merged_info(&key);
        if args.json {
            #[derive(Serialize)]
            struct Show<'a> {
                model: &'a bmodels_model::CanonicalModel,
                info: &'a bmodels_model::ModelInfo,
                listings: &'a [bmodels_model::Listing],
                events: &'a [Event],
            }
            output::json(&Show {
                model: &model,
                info: &info,
                listings: &listings,
                events: &events,
            })?;
            return anyhow::Ok(ExitCode::SUCCESS);
        }
        let t = now();
        println!(
            "{}  {}",
            paint(Paint::Bold, &model.display_name),
            paint(Paint::Dim, &model.key)
        );
        for (label, value) in model_rows(&model, &info, t) {
            println!("  {}  {value}", paint(Paint::Dim, &format!("{label:<12}")));
        }
        if let Some(d) = &info.description {
            println!("\n  {d}");
        }
        println!(
            "\n{}",
            paint(Paint::Cyan, &format!("Listings ({})", listings.len()))
        );
        print_listings(&data, &listings);
        if !events.is_empty() {
            println!("\n{}", paint(Paint::Cyan, "Events"));
            let models = std::collections::BTreeMap::from([(model.key.clone(), model.clone())]);
            print_events(ctx, &events, &models, false)?;
        }
        anyhow::Ok(ExitCode::SUCCESS)
    })?
}

fn history(ctx: &Context, args: &HistoryArgs) -> anyhow::Result<ExitCode> {
    let since = parse_since(args.since.as_ref())?;
    block_on(async {
        let store = ctx.store().await?;
        let events = store
            .events(&EventQuery {
                since,
                source: args.source.clone(),
                limit: Some(args.limit),
                ..EventQuery::default()
            })
            .await?;
        let keys: BTreeSet<String> = events.iter().map(|e| e.canonical_key.clone()).collect();
        let models = store.models_by_key(&keys).await?;
        if events.is_empty() && !args.json {
            println!("no events yet");
        } else {
            print_events(ctx, &events, &models, args.json)?;
        }
        anyhow::Ok(ExitCode::SUCCESS)
    })?
}

fn print_model_table(data: &bmodels_tui::Data, models: &[&bmodels_model::CanonicalModel]) {
    let mut table = Table::default();
    for m in models {
        let info = data.merged_info(&m.key);
        let (mark, style) = match m.status {
            ModelStatus::Unseen => ("●", Paint::Green),
            ModelStatus::Starred => ("★", Paint::Yellow),
            ModelStatus::Ignored => ("⊘", Paint::Dim),
            ModelStatus::Seen => (" ", Paint::Dim),
        };
        let present = data
            .listings_of(&m.key)
            .iter()
            .filter(|l| l.is_present())
            .count();
        table.row(vec![
            styled(mark, style),
            styled(format_date(m.first_seen_at), Paint::Dim),
            styled(
                m.display_name.clone(),
                if m.status == ModelStatus::Unseen {
                    Paint::Bold
                } else {
                    Paint::Dim
                },
            ),
            cell(m.key.clone()),
            cell(info.context_window.map(format_tokens).unwrap_or_default()),
            cell(info.price_summary().unwrap_or_default()),
            styled(format!("{present} src"), Paint::Dim),
        ]);
    }
    table.print();
}

fn model_rows(
    model: &bmodels_model::CanonicalModel,
    info: &bmodels_model::ModelInfo,
    t: Timestamp,
) -> Vec<(&'static str, String)> {
    let mut rows: Vec<(&'static str, String)> = vec![
        ("status", model.status.as_str().to_lowercase()),
        (
            "first seen",
            format!(
                "{} ({})",
                format_datetime(model.first_seen_at),
                format_ago(t, model.first_seen_at)
            ),
        ),
    ];
    if let Some(c) = model.created_at {
        rows.push(("released", format_date(c)));
    }
    if let Some(c) = info.context_window {
        rows.push(("context", format_tokens(c)));
    }
    if let Some(c) = info.max_output {
        rows.push(("max output", format_tokens(c)));
    }
    if let Some(p) = info.price_summary() {
        rows.push(("price / 1M", format!("{p} (in / out)")));
    }
    if !info.input_modalities.is_empty() {
        rows.push((
            "modalities",
            format!(
                "{} → {}",
                info.input_modalities.join(", "),
                info.output_modalities.join(", ")
            ),
        ));
    }
    if !info.capabilities.is_empty() {
        rows.push((
            "capabilities",
            info.capabilities
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if !info.variants.is_empty() {
        rows.push(("variants", info.variants.join(", ")));
    }
    if let Some(d) = &info.deprecation_date {
        rows.push(("deprecation", d.clone()));
    }
    if let Some(k) = &info.knowledge_cutoff {
        rows.push(("knowledge", k.clone()));
    }
    if let Some(n) = &model.note {
        rows.push(("note", n.clone()));
    }
    rows
}

fn print_listings(data: &bmodels_tui::Data, listings: &[bmodels_model::Listing]) {
    let mut table = Table::default();
    for l in listings {
        table.row(vec![
            cell("  "),
            cell(data.source_name(&l.key.source).to_owned()),
            cell(l.key.model_id.clone()),
            styled(
                l.removed_at.map_or_else(
                    || format!("since {}", format_date(l.first_seen_at)),
                    |r| format!("removed {}", format_date(r)),
                ),
                if l.removed_at.is_some() {
                    Paint::Red
                } else {
                    Paint::Dim
                },
            ),
            cell(l.info.price_summary().unwrap_or_default()),
            styled(l.url.clone().unwrap_or_default(), Paint::Dim),
        ]);
    }
    table.print();
}

// ------------------------------------------------------------------ sources / config

fn sources(ctx: &Context, args: &JsonArgs) -> anyhow::Result<ExitCode> {
    block_on(async {
        let store = ctx.store().await?;
        let states = store.source_states().await?;
        let rows = source_rows(ctx.config(), &states, now());
        if args.json {
            #[derive(Serialize)]
            struct Row<'a> {
                id: &'a str,
                name: &'a str,
                kind: &'a str,
                enabled: bool,
                interval: &'a str,
                health: SourceHealth,
                next_due: Option<Timestamp>,
                state: &'a bmodels_model::SourceState,
            }
            let out: Vec<Row<'_>> = rows
                .iter()
                .map(|r| Row {
                    id: &r.id,
                    name: &r.name,
                    kind: r.kind.as_str(),
                    enabled: r.enabled,
                    interval: &r.interval,
                    health: r.state.health(),
                    next_due: r.next_due,
                    state: &r.state,
                })
                .collect();
            output::json(&out)?;
            return anyhow::Ok(ExitCode::SUCCESS);
        }
        let t = now();
        let mut table = Table::default();
        table.row(vec![
            cell(""),
            styled("SOURCE", Paint::Dim),
            styled("KIND", Paint::Dim),
            styled("MODELS", Paint::Dim),
            styled("SYNCED", Paint::Dim),
            styled("EVERY", Paint::Dim),
            styled("STATUS", Paint::Dim),
        ]);
        for r in &rows {
            let (mark, style, detail) = if r.enabled {
                match r.state.health() {
                    SourceHealth::Ok => ("●", Paint::Green, "ok".to_owned()),
                    SourceHealth::Never => ("○", Paint::Dim, "never synced".to_owned()),
                    SourceHealth::Skipped => (
                        "◌",
                        Paint::Yellow,
                        r.state.skipped.clone().unwrap_or_default(),
                    ),
                    SourceHealth::Error => (
                        "✗",
                        Paint::Red,
                        r.state.last_error.clone().unwrap_or_default(),
                    ),
                }
            } else {
                ("○", Paint::Dim, "disabled".to_owned())
            };
            table.row(vec![
                styled(mark, style),
                cell(format!("{} ({})", r.name, r.id)),
                styled(r.kind.as_str(), Paint::Dim),
                cell(r.state.listing_count.to_string()),
                styled(
                    r.state
                        .last_success_at
                        .map_or_else(|| "–".to_owned(), |s| format_age(t, s)),
                    Paint::Dim,
                ),
                styled(r.interval.clone(), Paint::Dim),
                styled(detail, style),
            ]);
        }
        table.print();
        anyhow::Ok(ExitCode::SUCCESS)
    })?
}

fn config_path(cli: &Cli) -> anyhow::Result<ExitCode> {
    let options = bmodels_config::LoadOptions::from_process()?.with_explicit(cli.config.clone());
    let global = options.global_path()?;
    println!(
        "config  {}{}",
        global.display(),
        if global.is_file() {
            ""
        } else {
            "  (not created; `bmodels config init`)"
        }
    );
    if let Some(explicit) = &cli.config {
        println!("extra   {}", explicit.display());
    }
    match bmodels_config::load(&options) {
        Ok(mut loaded) => {
            if let Some(dir) = &cli.data_dir {
                loaded.data_dir.clone_from(dir);
            }
            println!("data    {}", loaded.database_path().display());
        }
        Err(e) => println!("data    (configuration error: {e})"),
    }
    Ok(ExitCode::SUCCESS)
}

fn config(ctx: &Context, command: &ConfigCommand) -> anyhow::Result<ExitCode> {
    match command {
        ConfigCommand::Path => unreachable!("handled before loading"),
        ConfigCommand::Show => {
            // Plaintext secrets are shown as `<redacted>`; references are shown as written.
            let mut shown = ctx.config().clone();
            for spec in shown.sources.values_mut() {
                for value in spec.headers.values_mut() {
                    *value = value.redacted();
                }
            }
            shown.notify.webhook = shown.notify.webhook.as_ref().map(SecretValue::redacted);
            print!(
                "{}",
                toml::to_string_pretty(&shown).context("encoding configuration")?
            );
        }
        ConfigCommand::Init { force } => {
            let path = &ctx.loaded.global_path;
            if path.exists() && !force {
                anyhow::bail!(
                    "{} already exists (use --force to overwrite)",
                    path.display()
                );
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, bmodels_config::STARTER_CONFIG)?;
            println!("wrote {}", path.display());
        }
        ConfigCommand::Check => {
            for source in &ctx.loaded.sources {
                println!("{} {source}", paint(Paint::Dim, "layer"));
            }
            let enabled = ctx.config().enabled_sources().count();
            println!(
                "{} configuration is valid ({enabled} enabled source(s))",
                paint(Paint::Green, "✓")
            );
            crate::auth::print_findings(&bmodels_auth::plaintext_secrets(ctx.config()));
        }
    }
    Ok(ExitCode::SUCCESS)
}
