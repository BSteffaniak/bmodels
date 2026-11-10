//! Drawing: pure functions of [`App`] state onto a bmux [`PaintCx`].

use std::cell::{Cell, RefCell};

use bmodels_model::display::{format_age, format_ago, format_date, format_datetime, format_tokens};
use bmodels_model::{
    CanonicalModel, Event, EventKind, Listing, ListingStatus, ModelInfo, ModelStatus, SourceHealth,
};
use bmux_tui::composition::TextBlock;
use bmux_tui::geometry::{Insets, Rect, Size};
use bmux_tui::paint::{LocalRect, PaintCx};
use bmux_tui::style::{Color, Modifier, Style};
use bmux_tui::text::{Line, Span, Text, TextWrap};
use bmux_tui::text_width::{display_width, truncate_to_display_width};
use bmux_tui_components::key_hint_bar::{KeyHint, KeyHintBarComponent};
use bmux_tui_components::labeled_details::{
    DetailItem, LabelPlacement, LabeledDetailsComponent, LabeledDetailsStyles,
};
use bmux_tui_components::modal_frame::{ModalFrame, ModalFrameComponent, ModalSizing, ModalTheme};
use bmux_tui_components::pane::{Pane, PaneComponent, PaneState};
use bmux_tui_components::status_bar::{StatusBarComponent, StatusSegment, StatusSeverity};
use bmux_tui_components::tab_bar::{TabBarComponent, TabBarPolicy, TabBarState, TabItem};
use bmux_tui_components::text_input_box::{TextInputBoxComponent, TextInputBoxPolicy};
use bmux_tui_components::uniform_list::UniformList;

use crate::app::{App, MessageLevel, Modal};
use crate::backend::{Backend, Data, SourceRow};
use crate::view::{Row, Tab};

const ACCENT: Color = Color::Cyan;
const MUTED: Style = Style::new().fg(Color::BrightBlack);
const SELECTED: Style = Style::new().bg(Color::Blue).fg(Color::BrightWhite);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);
/// Minimum width at which details dock to the right instead of opening as a modal.
pub const DOCK_WIDTH: u16 = 110;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn write(cx: &mut PaintCx<'_, '_>, rect: Rect, line: &Line) {
    cx.write_line(LocalRect::terminal(rect), line);
}

fn styled(text: impl Into<String>, style: Style) -> Line {
    Line::from_spans(vec![Span::styled(text.into(), style)])
}

const fn row(area: Rect, y: u16) -> Rect {
    Rect::new(area.x, area.y.saturating_add(y), area.width, 1)
}

fn width_of(s: &str) -> u16 {
    u16::try_from(display_width(s)).unwrap_or(u16::MAX)
}

/// Pads or truncates to exactly `width` display columns.
fn fit(s: &str, width: usize) -> String {
    let t = truncate_to_display_width(s, width);
    let pad = width.saturating_sub(display_width(&t));
    format!("{t}{}", " ".repeat(pad))
}

fn fit_right(s: &str, width: usize) -> String {
    let t = truncate_to_display_width(s, width);
    let pad = width.saturating_sub(display_width(&t));
    format!("{}{t}", " ".repeat(pad))
}

const fn spinner(tick: u64) -> &'static str {
    #[allow(clippy::cast_possible_truncation)] // modulo keeps it tiny
    SPINNER[(tick % SPINNER.len() as u64) as usize]
}

const fn kind_color(kind: EventKind) -> Color {
    match kind {
        EventKind::New => Color::Green,
        EventKind::NewListing => Color::Cyan,
        EventKind::Changed => Color::Yellow,
        EventKind::Removed => Color::Red,
        EventKind::Reappeared => Color::Magenta,
    }
}

const fn kind_label(kind: EventKind) -> &'static str {
    match kind {
        EventKind::New => "NEW",
        EventKind::NewListing => "LISTING",
        EventKind::Changed => "CHANGED",
        EventKind::Removed => "REMOVED",
        EventKind::Reappeared => "BACK",
    }
}

const fn status_mark(status: ModelStatus) -> (&'static str, Style) {
    match status {
        ModelStatus::Unseen => ("●", Style::new().fg(Color::Green)),
        ModelStatus::Starred => ("★", Style::new().fg(Color::Yellow)),
        ModelStatus::Ignored => ("⊘", MUTED),
        ModelStatus::Seen => (" ", Style::new()),
    }
}

/// Compact age that stays within 5 columns (`now`, `5m`, `3h`, `2d`, `5w`, `4mo`, `2y`).
fn short_age(now: i64, ts: i64) -> String {
    let delta = now.saturating_sub(ts);
    if delta < 7_776_000 {
        format_age(now, ts)
    } else if delta < 31_536_000 {
        format!("{}mo", delta / 2_592_000)
    } else {
        format!("{}y", delta / 31_536_000)
    }
}

/// Renders the whole UI.
pub fn render<B: Backend>(app: &mut App<B>, cx: &mut PaintCx<'_, '_>) {
    let full = cx.clip();
    cx.fill(LocalRect::terminal(full), " ", Style::new());
    if full.width < 40 || full.height < 8 {
        write(cx, row(full, 0), &Line::raw("terminal too small"));
        return;
    }
    app.narrow = full.width < DOCK_WIDTH;
    render_header(app, cx, row(full, 0));
    let body = Rect::new(full.x, full.y + 1, full.width, full.height - 3);
    render_body(app, cx, body);
    render_status(app, cx, row(full, full.height - 2));
    render_hints(app, cx, row(full, full.height - 1));
    if let Some(modal) = app.modal.clone() {
        render_modal(app, &modal, cx, full);
    }
}

fn render_header<B: Backend>(app: &App<B>, cx: &mut PaintCx<'_, '_>, area: Rect) {
    let title = " bmodels ";
    write(
        cx,
        area,
        &styled(
            title,
            Style::new()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
    );
    let unacked = app.data.unacked();
    let labels = [
        if unacked > 0 {
            format!("Inbox ({unacked})")
        } else {
            "Inbox".to_owned()
        },
        format!("Models ({})", app.data.models.len()),
        "Sources".to_owned(),
        "History".to_owned(),
    ];
    let items: Vec<TabItem<'_>> = Tab::ALL
        .iter()
        .zip(labels.iter())
        .map(|(t, l)| TabItem::new(t.id(), l))
        .collect();
    let state = RefCell::new(TabBarState::new(Some(app.tab.index())));
    let info = if app.filter_editing || !app.view.filter.is_empty() {
        format!(
            "filter: {}{}",
            app.view.filter,
            if app.filter_editing { "▏" } else { "" }
        )
    } else if app.tab == Tab::Models && app.view.listing_view {
        "listings".to_owned()
    } else {
        String::new()
    };
    let info_width = width_of(&info).min(area.width / 3);
    let left = width_of(title) + 1;
    let tabs = Rect::new(
        area.x + left,
        area.y,
        area.width.saturating_sub(left + info_width + 1),
        1,
    );
    cx.paint_component(
        &TabBarComponent::new("tabs", &items, &state).policy(TabBarPolicy::bare()),
        tabs,
    );
    if info_width > 0 {
        let style = if app.filter_editing {
            Style::new().fg(Color::Yellow)
        } else {
            MUTED
        };
        write(
            cx,
            Rect::new(area.right() - info_width, area.y, info_width, 1),
            &styled(
                truncate_to_display_width(&info, usize::from(info_width)),
                style,
            ),
        );
    }
}

fn render_body<B: Backend>(app: &mut App<B>, cx: &mut PaintCx<'_, '_>, body: Rect) {
    let docked = app.details && !app.narrow && app.selected().is_some();
    let (list, pane) = if docked {
        let width = (body.width * 2 / 5).clamp(40, 70);
        (
            Rect::new(body.x, body.y, body.width - width, body.height),
            Some(Rect::new(body.right() - width, body.y, width, body.height)),
        )
    } else {
        (body, None)
    };
    let header = row(list, 0);
    let rows_area = Rect::new(
        list.x,
        list.y + 1,
        list.width,
        list.height.saturating_sub(1),
    );
    app.viewport_rows = usize::from(rows_area.height.max(1));
    app.list_area = rows_area;
    let rows = app.rows().to_vec();
    let width = usize::from(rows_area.width.saturating_sub(1));
    write(
        cx,
        header,
        &styled(
            fit(&column_header(app, width), usize::from(header.width)),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
    );

    if rows.is_empty() {
        write(
            cx,
            row(rows_area, 1),
            &styled(format!("  {}", empty_text(app)), MUTED),
        );
    } else {
        let data = app.data.clone();
        let tick = app.tick;
        let sync = app.sync.clone();
        UniformList::new("rows", rows.len()).paint(
            rows_area,
            app.state_mut(),
            cx,
            |i, area, selected, cx| {
                let mut line =
                    row_line(&data, rows[i], usize::from(area.width), tick, sync.as_ref());
                if selected {
                    line = line.patch_style(SELECTED);
                    cx.fill(LocalRect::terminal(area), " ", SELECTED);
                }
                write(cx, area, &line);
            },
        );
    }

    if let Some(area) = pane {
        let lines = app
            .selected()
            .map(|r| detail_lines(&app.data, r, area.width.saturating_sub(4)))
            .unwrap_or_default();
        let state = Cell::new(PaneState::new(area));
        let pane = PaneComponent::new(
            "details",
            Pane::new().title("Details (i)"),
            &state,
            TextBlock::new(Text::from_lines(lines)).wrap(TextWrap::None),
        );
        cx.paint_component(&pane, area);
    }
}

fn empty_text<B: Backend>(app: &App<B>) -> &'static str {
    if !app.loaded {
        "loading…"
    } else if !app.view.filter.is_empty() {
        "nothing matches the filter"
    } else {
        match app.tab {
            Tab::Inbox if app.data.models.is_empty() => "nothing synced yet — press r to sync",
            Tab::Inbox => "all caught up ✓",
            Tab::Models => "no models yet — press r to sync",
            Tab::Sources => "no sources configured",
            Tab::History => "no events yet",
        }
    }
}

fn column_header<B: Backend>(app: &App<B>, width: usize) -> String {
    match app.tab {
        Tab::Inbox | Tab::History => {
            let rest = width.saturating_sub(17);
            format!(
                "  {:<8} {:>5}  {}{}",
                "Event",
                "Age",
                fit("Model", rest * 2 / 5),
                "Detail"
            )
        }
        Tab::Models if app.view.listing_view => {
            let cols = ModelCols::for_width(width);
            format!(
                "  {:>5}  {}{}{}{}",
                "Seen",
                fit("Source", 14),
                fit("Model id", cols.name),
                if cols.ctx {
                    fit_right("Context", 8)
                } else {
                    String::new()
                },
                if cols.price {
                    fit_right("Price in/out", 18)
                } else {
                    String::new()
                },
            )
        }
        Tab::Models => {
            let cols = ModelCols::for_width(width);
            format!(
                "  {:>5}  {}{}{}{}{}",
                "Seen",
                fit("Model", cols.name),
                fit("Key", cols.key),
                if cols.ctx {
                    fit_right("Context", 8)
                } else {
                    String::new()
                },
                if cols.price {
                    fit_right("Price in/out", 18)
                } else {
                    String::new()
                },
                if cols.sources { "  Sources" } else { "" },
            )
        }
        Tab::Sources => format!(
            "  {}{}{:>7}  {:>6}  {:>6}  Status",
            fit("Source", 20),
            fit("Kind", 18),
            "Models",
            "Synced",
            "Next"
        ),
    }
}

/// Column layout of the Models tab.
#[derive(Debug, Clone, Copy)]
struct ModelCols {
    name: usize,
    key: usize,
    ctx: bool,
    price: bool,
    sources: bool,
}

impl ModelCols {
    fn for_width(width: usize) -> Self {
        let fixed = 9; // marker + age + gaps
        let mut cols = Self {
            name: 0,
            key: 0,
            ctx: width >= 70,
            price: width >= 90,
            sources: width >= 120,
        };
        let extra = usize::from(cols.ctx) * 8
            + usize::from(cols.price) * 18
            + usize::from(cols.sources) * 10;
        let flex = width.saturating_sub(fixed + extra);
        cols.name = (flex * 9 / 20).max(10);
        cols.key = flex.saturating_sub(cols.name).max(8);
        cols
    }
}

fn row_line(
    data: &Data,
    row: Row,
    width: usize,
    tick: u64,
    sync: Option<&crate::app::SyncState>,
) -> Line {
    match row {
        Row::Event(i) => event_line(data, &data.events[i], width),
        Row::Model(i) => model_line(data, &data.models[i], width),
        Row::Listing(i) => listing_line(data, &data.listings[i], width),
        Row::Source(i) => source_line(&data.sources[i], width, data.now, tick, sync),
    }
}

fn event_detail(data: &Data, e: &Event) -> String {
    let source = data.source_name(&e.listing.source);
    match e.kind {
        EventKind::New => {
            let sources: Vec<&str> = data
                .listings_of(&e.canonical_key)
                .iter()
                .map(|l| data.source_name(&l.key.source))
                .collect();
            format!("via {}", sources.join(", "))
        }
        EventKind::NewListing => format!("on {source} as {}", e.listing.model_id),
        EventKind::Changed => {
            e.changes
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
                .join("; ")
                + &format!(" ({source})")
        }
        EventKind::Removed => format!("gone from {source}{}", as_listing(e)),
        EventKind::Reappeared => format!("back on {source}{}", as_listing(e)),
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

fn event_line(data: &Data, e: &Event, width: usize) -> Line {
    let model = data.model(&e.canonical_key);
    let name = model.map_or(e.canonical_key.as_str(), |m| m.display_name.as_str());
    let acked = e.acked_at.is_some();
    let rest = width.saturating_sub(17);
    let name_width = rest * 2 / 5;
    let color = kind_color(e.kind);
    let dim = |s: Style| if acked { MUTED } else { s };
    Line::from_spans(vec![
        Span::styled(if acked { "  " } else { "● " }, dim(Style::new().fg(color))),
        Span::styled(
            fit(kind_label(e.kind), 8),
            dim(Style::new().fg(color).add_modifier(Modifier::BOLD)),
        ),
        Span::styled(format!(" {:>5}  ", short_age(data.now, e.at)), MUTED),
        Span::styled(fit(name, name_width), dim(BOLD)),
        Span::styled(
            fit(&event_detail(data, e), rest.saturating_sub(name_width)),
            dim(Style::new()),
        ),
    ])
}

fn sources_of(data: &Data, key: &str) -> String {
    let listings = data.listings_of(key);
    let present = listings.iter().filter(|l| l.is_present()).count();
    format!("{present}")
}

fn model_line(data: &Data, m: &CanonicalModel, width: usize) -> Line {
    let cols = ModelCols::for_width(width);
    let (mark, mark_style) = status_mark(m.status);
    let info = data.merged_info(&m.key);
    let name_style = match m.status {
        ModelStatus::Unseen => BOLD,
        ModelStatus::Ignored => MUTED,
        _ => Style::new(),
    };
    let mut spans = vec![
        Span::styled(format!("{mark} "), mark_style),
        Span::styled(
            format!("{:>5}  ", short_age(data.now, m.first_seen_at)),
            MUTED,
        ),
        Span::styled(fit(&m.display_name, cols.name), name_style),
        Span::styled(fit(&m.key, cols.key), MUTED),
    ];
    if cols.ctx {
        spans.push(Span::raw(fit_right(
            &info.context_window.map(format_tokens).unwrap_or_default(),
            8,
        )));
    }
    if cols.price {
        spans.push(Span::raw(fit_right(
            &info.price_summary().unwrap_or_default(),
            18,
        )));
    }
    if cols.sources {
        spans.push(Span::styled(
            format!("  {:>7}", sources_of(data, &m.key)),
            MUTED,
        ));
    }
    Line::from_spans(spans)
}

fn listing_line(data: &Data, l: &Listing, width: usize) -> Line {
    let cols = ModelCols::for_width(width);
    let (mark, style) = if l.removed_at.is_some() {
        ("✗", Style::new().fg(Color::Red))
    } else if l.status == ListingStatus::Unseen {
        ("●", Style::new().fg(Color::Green))
    } else {
        (" ", Style::new())
    };
    let mut spans = vec![
        Span::styled(format!("{mark} "), style),
        Span::styled(
            format!("{:>5}  ", short_age(data.now, l.first_seen_at)),
            MUTED,
        ),
        Span::styled(
            fit(data.source_name(&l.key.source), 14),
            Style::new().fg(ACCENT),
        ),
        Span::raw(fit(
            &l.key.model_id,
            cols.name + cols.key.saturating_sub(14),
        )),
    ];
    if cols.ctx {
        spans.push(Span::raw(fit_right(
            &l.info.context_window.map(format_tokens).unwrap_or_default(),
            8,
        )));
    }
    if cols.price {
        spans.push(Span::raw(fit_right(
            &l.info.price_summary().unwrap_or_default(),
            18,
        )));
    }
    Line::from_spans(spans)
}

fn source_line(
    s: &SourceRow,
    width: usize,
    now: i64,
    tick: u64,
    sync: Option<&crate::app::SyncState>,
) -> Line {
    let running = sync.is_some_and(|x| x.running.contains(&s.id));
    let (mark, mark_style, status) = if running {
        (
            spinner(tick),
            Style::new().fg(ACCENT),
            "syncing…".to_owned(),
        )
    } else if !s.enabled {
        ("○", MUTED, "disabled".to_owned())
    } else {
        match s.state.health() {
            SourceHealth::Ok => ("●", Style::new().fg(Color::Green), "ok".to_owned()),
            SourceHealth::Never => ("○", MUTED, "never synced".to_owned()),
            SourceHealth::Skipped => (
                "◌",
                Style::new().fg(Color::Yellow),
                s.state.skipped.clone().unwrap_or_default(),
            ),
            SourceHealth::Error => (
                "✗",
                Style::new().fg(Color::Red),
                s.state.last_error.clone().unwrap_or_default(),
            ),
        }
    };
    let synced = s
        .state
        .last_success_at
        .map_or_else(|| "–".to_owned(), |t| short_age(now, t));
    let next = match s.next_due {
        Some(t) if s.enabled && t > now => short_age(t, now).replace("now", "<1m"),
        Some(_) if s.enabled => "due".to_owned(),
        _ => "–".to_owned(),
    };
    let text_style = if s.enabled { Style::new() } else { MUTED };
    let status_width = width.saturating_sub(2 + 20 + 18 + 7 + 2 + 6 + 2 + 6 + 2);
    Line::from_spans(vec![
        Span::styled(format!("{mark} "), mark_style),
        Span::styled(fit(&s.name, 20), text_style.add_modifier(Modifier::BOLD)),
        Span::styled(fit(s.kind.as_str(), 18), MUTED),
        Span::styled(format!("{:>7}  ", s.state.listing_count), text_style),
        Span::styled(format!("{synced:>6}  "), MUTED),
        Span::styled(format!("{next:>6}  "), MUTED),
        Span::styled(fit(&status, status_width), mark_style),
    ])
}

// ------------------------------------------------------------------ details

fn kv(label: &str, value: impl Into<String>) -> DetailItem {
    DetailItem::new(label, value)
}

fn details(items: &[DetailItem], width: u16) -> Vec<Line> {
    LabeledDetailsComponent::new("details", items)
        .item_spacing(false)
        .label_placement(LabelPlacement::Beside)
        .styles(LabeledDetailsStyles {
            label: MUTED.add_modifier(Modifier::BOLD),
            value: Style::new(),
            continuation: Style::new(),
        })
        .lines(width.max(20))
}

fn section(title: &str) -> Line {
    styled(title, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
}

fn wrapped(text: &str, width: u16, style: Style) -> Vec<Line> {
    styled(text, style).wrap_word(usize::from(width.max(10)))
}

/// Detail lines for a row.
#[must_use]
pub fn detail_lines(data: &Data, row: Row, width: u16) -> Vec<Line> {
    match row {
        Row::Source(i) => source_details(data, &data.sources[i], width),
        Row::Event(i) => {
            let e = &data.events[i];
            let mut lines = vec![Line::from_spans(vec![
                Span::styled(
                    format!("{} ", kind_label(e.kind)),
                    Style::new()
                        .fg(kind_color(e.kind))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format_datetime(e.at), MUTED),
            ])];
            lines.extend(wrapped(&event_detail(data, e), width, Style::new()));
            lines.push(Line::default());
            lines.extend(model_details(data, &e.canonical_key, width));
            lines
        }
        Row::Model(i) => model_details(data, &data.models[i].key, width),
        Row::Listing(i) => model_details(data, &data.listings[i].canonical_key, width),
    }
}

fn model_details(data: &Data, key: &str, width: u16) -> Vec<Line> {
    let Some(m) = data.model(key) else {
        return vec![styled(key, BOLD)];
    };
    let info = data.merged_info(key);
    let mut lines = vec![styled(m.display_name.clone(), BOLD)];
    lines.extend(details(&model_fields(data, m, &info), width));

    let listings = data.listings_of(key);
    lines.push(Line::default());
    lines.push(section(&format!("Listings ({})", listings.len())));
    for l in listings {
        let state = l.removed_at.map_or_else(
            || format!("since {}", format_date(l.first_seen_at)),
            |t| format!("removed {}", format_date(t)),
        );
        lines.push(Line::from_spans(vec![
            Span::styled(
                format!("  {} ", fit(data.source_name(&l.key.source), 14)),
                Style::new().fg(ACCENT),
            ),
            Span::raw(truncate_to_display_width(
                &l.key.model_id,
                usize::from(width.saturating_sub(17)),
            )),
        ]));
        let mut extra = vec![state];
        if let Some(p) = l.info.price_summary() {
            extra.push(p);
        }
        lines.push(styled(format!("  {:14} {}", "", extra.join(" · ")), MUTED));
    }
    if let Some(d) = &info.description {
        lines.push(Line::default());
        lines.push(section("Description"));
        lines.extend(wrapped(d, width, Style::new()));
    }
    let events: Vec<&Event> = data
        .events
        .iter()
        .filter(|e| e.canonical_key == key)
        .take(10)
        .collect();
    if !events.is_empty() {
        lines.push(Line::default());
        lines.push(section("Recent events"));
        for e in events {
            lines.push(Line::from_spans(vec![
                Span::styled(format!("  {} ", format_date(e.at)), MUTED),
                Span::styled(
                    format!("{:<8}", kind_label(e.kind)),
                    Style::new().fg(kind_color(e.kind)),
                ),
                Span::raw(truncate_to_display_width(
                    &event_detail(data, e),
                    usize::from(width.saturating_sub(22)),
                )),
            ]));
        }
    }
    lines
}

/// Label/value rows describing a model.
fn model_fields(data: &Data, m: &CanonicalModel, info: &ModelInfo) -> Vec<DetailItem> {
    let mut items = vec![
        kv("Key", m.key.clone()),
        kv("Status", m.status.as_str().to_lowercase()),
        kv(
            "First seen",
            format!(
                "{} ({})",
                format_date(m.first_seen_at),
                format_ago(data.now, m.first_seen_at)
            ),
        ),
    ];
    if let Some(created) = m.created_at {
        items.push(kv("Released", format_date(created)));
    }
    if let Some(c) = info.context_window {
        items.push(kv("Context", format_tokens(c)));
    }
    if let Some(c) = info.max_output {
        items.push(kv("Max output", format_tokens(c)));
    }
    if let Some(p) = info.price_summary() {
        items.push(kv("Price / 1M", format!("{p} (in / out)")));
    }
    if !info.input_modalities.is_empty() || !info.output_modalities.is_empty() {
        items.push(kv(
            "Modalities",
            format!(
                "{} → {}",
                info.input_modalities.join(", "),
                info.output_modalities.join(", ")
            ),
        ));
    }
    if !info.capabilities.is_empty() {
        items.push(kv(
            "Capabilities",
            info.capabilities
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if !info.variants.is_empty() {
        items.push(kv("Variants", info.variants.join(", ")));
    }
    if let Some(d) = &info.deprecation_date {
        items.push(kv("Deprecation", d.clone()));
    }
    if let Some(k) = &info.knowledge_cutoff {
        items.push(kv("Knowledge", k.clone()));
    }
    if let Some(o) = info.open_weights {
        items.push(kv("Open weights", if o { "yes" } else { "no" }));
    }
    if let Some(n) = &m.note {
        items.push(kv("Note", n.clone()));
    }
    items
}

fn source_details(data: &Data, s: &SourceRow, width: u16) -> Vec<Line> {
    let st = &s.state;
    let when = |t: Option<i64>| {
        t.map_or_else(
            || "never".to_owned(),
            |t| format!("{} ({})", format_datetime(t), format_ago(data.now, t)),
        )
    };
    let mut items = vec![
        kv("Id", s.id.clone()),
        kv("Kind", s.kind.as_str()),
        kv("Enabled", if s.enabled { "yes" } else { "no" }),
        kv("Interval", s.interval.clone()),
        kv("Models", st.listing_count.to_string()),
        kv("Last attempt", when(st.last_attempt_at)),
        kv("Last success", when(st.last_success_at)),
    ];
    if let Some(v) = &s.vendor {
        items.insert(2, kv("Vendor", v.clone()));
    }
    if let Some(t) = s.next_due.filter(|_| s.enabled) {
        items.push(kv("Next sync", format_datetime(t)));
    }
    if let Some(b) = st.baseline_at {
        items.push(kv("Watching since", format_date(b)));
    }
    if st.failures > 0 {
        items.push(kv("Failures", st.failures.to_string()));
    }
    let mut lines = vec![styled(s.name.clone(), BOLD)];
    lines.extend(details(&items, width));
    if let Some(e) = &st.last_error {
        lines.push(Line::default());
        lines.push(styled(
            "Last error",
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        lines.extend(wrapped(e, width, Style::new()));
    }
    if let Some(sk) = &st.skipped {
        lines.push(Line::default());
        lines.push(styled(
            "Skipped",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
        lines.extend(wrapped(sk, width, Style::new()));
    }
    lines.push(Line::default());
    lines.push(styled(
        if s.takes_key {
            "R syncs just this source · l adds its API key"
        } else {
            "R syncs just this source"
        },
        MUTED,
    ));
    lines
}

// ------------------------------------------------------------------ chrome

fn render_status<B: Backend>(app: &App<B>, cx: &mut PaintCx<'_, '_>, area: Rect) {
    let data = &app.data;
    let ok = data
        .sources
        .iter()
        .filter(|s| s.enabled && s.state.health() == SourceHealth::Ok)
        .count();
    let enabled = data.sources.iter().filter(|s| s.enabled).count();
    let left_text = format!(
        "{} unseen · {} models · {ok}/{enabled} sources ok",
        data.models
            .iter()
            .filter(|m| m.status == ModelStatus::Unseen)
            .count(),
        data.models.len(),
    );
    let center_text = app.message.as_ref().map(|(text, level)| {
        (
            text.clone(),
            match level {
                MessageLevel::Info => StatusSeverity::Info,
                MessageLevel::Success => StatusSeverity::Success,
                MessageLevel::Error => StatusSeverity::Error,
            },
        )
    });
    let right_text = app.sync.as_ref().map_or_else(
        || {
            data.sources
                .iter()
                .filter_map(|s| s.state.last_success_at)
                .max()
                .map(|t| format!("synced {}", format_ago(data.now, t)))
                .unwrap_or_default()
        },
        |s| {
            if s.total == 0 {
                format!("{} syncing…", spinner(app.tick))
            } else {
                format!("{} syncing {}/{}", spinner(app.tick), s.done, s.total)
            }
        },
    );
    let left = [StatusSegment::new(&left_text)];
    let center: Vec<StatusSegment<'_>> = center_text
        .as_ref()
        .map(|(t, sev)| StatusSegment::new(t).severity(*sev))
        .into_iter()
        .collect();
    let right = [
        StatusSegment::new(&right_text).severity(if app.sync.is_some() {
            StatusSeverity::Info
        } else {
            StatusSeverity::Muted
        }),
    ];
    cx.fill(
        LocalRect::terminal(area),
        " ",
        Style::new().bg(Color::Indexed(236)),
    );
    cx.paint_component(
        &StatusBarComponent::new("status")
            .left(&left)
            .center(&center)
            .right(&right),
        area,
    );
}

fn render_hints<B: Backend>(app: &App<B>, cx: &mut PaintCx<'_, '_>, area: Rect) {
    const INBOX: &[KeyHint<'static>] = &[
        KeyHint::new("↑↓", "move"),
        KeyHint::new("a", "ack"),
        KeyHint::new("A", "ack all"),
        KeyHint::new("s", "star"),
        KeyHint::new("x", "ignore"),
        KeyHint::new("o", "open"),
        KeyHint::new("i", "details"),
        KeyHint::new("r", "sync"),
        KeyHint::new("/", "filter"),
        KeyHint::new("tab", "next"),
        KeyHint::new("?", "help"),
        KeyHint::new("q", "quit"),
    ];
    const MODELS: &[KeyHint<'static>] = &[
        KeyHint::new("↑↓", "move"),
        KeyHint::new("a", "ack"),
        KeyHint::new("s", "star"),
        KeyHint::new("x", "ignore"),
        KeyHint::new("u", "unseen"),
        KeyHint::new("v", "listings"),
        KeyHint::new("I", "ignored"),
        KeyHint::new("o", "open"),
        KeyHint::new("r", "sync"),
        KeyHint::new("/", "filter"),
        KeyHint::new("?", "help"),
        KeyHint::new("q", "quit"),
    ];
    const SOURCES: &[KeyHint<'static>] = &[
        KeyHint::new("↑↓", "move"),
        KeyHint::new("R", "sync source"),
        KeyHint::new("r", "sync all"),
        KeyHint::new("l", "add key"),
        KeyHint::new("i", "details"),
        KeyHint::new("/", "filter"),
        KeyHint::new("?", "help"),
        KeyHint::new("q", "quit"),
    ];
    const FILTER: &[KeyHint<'static>] = &[
        KeyHint::new("type", "filter"),
        KeyHint::new("enter", "keep"),
        KeyHint::new("esc", "clear"),
        KeyHint::new("↑↓", "move"),
    ];
    let hints = if app.filter_editing {
        FILTER
    } else {
        match app.tab {
            Tab::Inbox | Tab::History => INBOX,
            Tab::Models => MODELS,
            Tab::Sources => SOURCES,
        }
    };
    cx.paint_component(&KeyHintBarComponent::new("hints", hints), area);
}

fn modal_frame(title: &str, max: Size) -> ModalFrame {
    ModalFrame::new(
        ModalSizing::new(Size::new(30, 6), max, Insets::all(1)),
        ModalTheme::dark(ACCENT),
    )
    .title(format!(" {title} "))
}

fn paint_text_modal(
    cx: &mut PaintCx<'_, '_>,
    area: Rect,
    frame: ModalFrame,
    lines: Vec<Line>,
    scroll: usize,
) {
    let block = TextBlock::new(Text::from_lines(lines))
        .wrap(TextWrap::None)
        .vertical_scroll(scroll);
    cx.paint_component(&ModalFrameComponent::new("modal", frame, block), area);
}

fn render_modal<B: Backend>(app: &App<B>, modal: &Modal, cx: &mut PaintCx<'_, '_>, full: Rect) {
    match modal {
        Modal::Help { scroll } => {
            let lines = help_lines();
            let scroll = (*scroll).min(lines.len().saturating_sub(1));
            paint_text_modal(
                cx,
                full,
                modal_frame(
                    "Keys — esc to close",
                    Size::new(72, full.height.saturating_sub(2)),
                ),
                lines,
                scroll,
            );
        }
        Modal::Details { scroll } => {
            let width = full.width.saturating_sub(4).min(90);
            let lines = app
                .selected()
                .map(|r| detail_lines(&app.data, r, width.saturating_sub(6)))
                .unwrap_or_default();
            let scroll = (*scroll).min(lines.len().saturating_sub(1));
            paint_text_modal(
                cx,
                full,
                modal_frame(
                    "Details — esc to close",
                    Size::new(width, full.height.saturating_sub(2)),
                ),
                lines,
                scroll,
            );
        }
        Modal::EnterKey { name, key_url, .. } => {
            let mut parts = vec![(
                format!(
                    "Paste the {name} API key. It is saved to the bmodels vault (encrypted, \
                     bound to this device) and the source syncs right away."
                ),
                Style::new(),
            )];
            if let Some(url) = key_url {
                parts.push((format!("Create one at {url}  (^o opens it)"), MUTED));
            }
            parts.push(("enter saves · esc cancels".to_owned(), MUTED));
            let frame = modal_frame(
                &format!("{name} API key"),
                Size::new(full.width.saturating_sub(4).min(76), 12),
            );
            let content = frame.content_area(full);
            let lines: Vec<Line> = parts
                .iter()
                .flat_map(|(text, style)| wrapped(text, content.width, *style))
                .collect();
            paint_text_modal(cx, full, frame, lines, 0);
            let input_rows = 4.min(content.height);
            let input = Rect::new(
                content.x,
                content.y + content.height.saturating_sub(input_rows),
                content.width,
                input_rows,
            );
            let field = TextInputBoxComponent::new("api-key", app.key_policy, &app.key_input)
                .label("API key")
                .policy(TextInputBoxPolicy::labeled_field().focused(true));
            cx.paint_component(&field, input);
        }
        Modal::ConfirmAckAll { ids } => {
            let lines = vec![
                Line::raw(format!(
                    "Acknowledge all {} visible inbox event(s)?",
                    ids.len()
                )),
                Line::default(),
                styled("y / enter to confirm, any other key to cancel", MUTED),
            ];
            paint_text_modal(
                cx,
                full,
                modal_frame("Acknowledge all", Size::new(60, 7)),
                lines,
                0,
            );
        }
    }
}

fn help_lines() -> Vec<Line> {
    let groups: &[(&str, &[(&str, &str)])] = &[
        (
            "Navigation",
            &[
                ("↑↓ / j k", "move"),
                ("PgUp PgDn / ^u ^d", "page"),
                ("g G / Home End", "first / last"),
                ("tab / 1-4", "switch tab"),
                ("enter / i", "toggle details"),
                ("/", "filter (esc clears)"),
            ],
        ),
        (
            "Seen state",
            &[
                (
                    "a",
                    "acknowledge the selection: an inbox event, a model (all its listings), or a listing (v)",
                ),
                ("A", "acknowledge everything visible in the inbox"),
                ("s", "star / unstar the model"),
                ("x", "ignore / unignore the model (never notifies again)"),
                ("u", "mark the model unseen again"),
            ],
        ),
        (
            "Views",
            &[
                ("v", "models: toggle canonical models / per-source listings"),
                ("I", "show / hide ignored models"),
                ("o", "open the model's page"),
            ],
        ),
        (
            "Sync",
            &[
                ("r", "sync every enabled source now"),
                ("R", "sources tab: sync the selected source"),
                ("l", "sources tab: add or replace the source's API key"),
            ],
        ),
        ("Other", &[("?", "this help"), ("q / ^c", "quit")]),
    ];
    let mut lines = Vec::new();
    for (title, keys) in groups {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(section(title));
        for (key, what) in *keys {
            lines.push(Line::from_spans(vec![
                Span::styled(format!("  {key:<20}"), Style::new().fg(Color::Yellow)),
                Span::raw((*what).to_owned()),
            ]));
        }
    }
    lines.push(Line::default());
    lines.push(section("Legend"));
    lines.push(Line::from_spans(vec![
        Span::styled("  ● ", Style::new().fg(Color::Green)),
        Span::raw("unseen  "),
        Span::styled("★ ", Style::new().fg(Color::Yellow)),
        Span::raw("starred  "),
        Span::styled("⊘ ", MUTED),
        Span::raw("ignored  "),
        Span::styled("✗ ", Style::new().fg(Color::Red)),
        Span::raw("removed / failed"),
    ]));
    lines
}
