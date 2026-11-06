//! Which rows each tab shows: pure functions of [`Data`] and view options.

use bmodels_model::{Event, ModelStatus};

use crate::backend::Data;

/// A tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tab {
    /// Unacknowledged events.
    Inbox,
    /// Canonical models (or listings).
    Models,
    /// Configured sources.
    Sources,
    /// All recent events.
    History,
}

impl Tab {
    /// Every tab, in display order.
    pub const ALL: [Self; 4] = [Self::Inbox, Self::Models, Self::Sources, Self::History];

    /// Stable id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Models => "models",
            Self::Sources => "sources",
            Self::History => "history",
        }
    }

    /// Position in [`Tab::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Inbox => 0,
            Self::Models => 1,
            Self::Sources => 2,
            Self::History => 3,
        }
    }

    /// The next (or previous) tab, wrapping.
    #[must_use]
    pub const fn cycle(self, back: bool) -> Self {
        let i = self.index();
        Self::ALL[if back { (i + 3) % 4 } else { (i + 1) % 4 }]
    }
}

/// One row of a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Index into [`Data::events`].
    Event(usize),
    /// Index into [`Data::models`].
    Model(usize),
    /// Index into [`Data::listings`].
    Listing(usize),
    /// Index into [`Data::sources`].
    Source(usize),
}

/// View options that change the row set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewOptions {
    /// Filter text (whitespace-separated terms, all must match, case-insensitive).
    pub filter: String,
    /// Models tab shows listings instead of canonical models.
    pub listing_view: bool,
    /// Models tab includes ignored models.
    pub show_ignored: bool,
}

struct Matcher(Vec<String>);

impl Matcher {
    fn new(filter: &str) -> Self {
        Self(filter.split_whitespace().map(str::to_lowercase).collect())
    }

    fn matches(&self, fields: &[&str]) -> bool {
        self.0.iter().all(|term| {
            fields
                .iter()
                .any(|f| f.to_lowercase().contains(term.as_str()))
        })
    }
}

fn event_matches(data: &Data, m: &Matcher, e: &Event) -> bool {
    let name = data
        .model(&e.canonical_key)
        .map_or("", |m| m.display_name.as_str());
    m.matches(&[
        &e.canonical_key,
        name,
        &e.listing.source,
        &e.listing.model_id,
        e.kind.as_str(),
    ])
}

/// Rows of `tab`, in display order.
#[must_use]
pub fn rows(data: &Data, tab: Tab, options: &ViewOptions) -> Vec<Row> {
    let m = Matcher::new(&options.filter);
    match tab {
        Tab::Inbox => data
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.acked_at.is_none() && event_matches(data, &m, e))
            .map(|(i, _)| Row::Event(i))
            .collect(),
        Tab::History => data
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| event_matches(data, &m, e))
            .map(|(i, _)| Row::Event(i))
            .collect(),
        Tab::Models if options.listing_view => {
            let mut ix: Vec<usize> = data
                .listings
                .iter()
                .enumerate()
                .filter(|(_, l)| {
                    let ignored = data
                        .model(&l.canonical_key)
                        .is_some_and(|m| m.status == ModelStatus::Ignored);
                    (options.show_ignored || !ignored)
                        && m.matches(&[
                            &l.key.source,
                            &l.key.model_id,
                            &l.canonical_key,
                            l.display_name.as_deref().unwrap_or(""),
                        ])
                })
                .map(|(i, _)| i)
                .collect();
            ix.sort_by_key(|i| {
                let l = &data.listings[*i];
                (std::cmp::Reverse(l.first_seen_at), l.key.clone())
            });
            ix.into_iter().map(Row::Listing).collect()
        }
        Tab::Models => {
            let mut ix: Vec<usize> = data
                .models
                .iter()
                .enumerate()
                .filter(|(_, model)| {
                    (options.show_ignored || model.status != ModelStatus::Ignored)
                        && m.matches(&[&model.key, &model.display_name, &model.vendor])
                })
                .map(|(i, _)| i)
                .collect();
            ix.sort_by_key(|i| {
                let model = &data.models[*i];
                (
                    std::cmp::Reverse(model.first_seen_at),
                    std::cmp::Reverse(model.created_at),
                    model.key.clone(),
                )
            });
            ix.into_iter().map(Row::Model).collect()
        }
        Tab::Sources => {
            let mut ix: Vec<usize> = data
                .sources
                .iter()
                .enumerate()
                .filter(|(_, s)| m.matches(&[&s.id, &s.name, s.kind.as_str()]))
                .map(|(i, _)| i)
                .collect();
            ix.sort_by_key(|i| (!data.sources[*i].enabled, data.sources[*i].id.clone()));
            ix.into_iter().map(Row::Source).collect()
        }
    }
}

/// The canonical model a row is about, if any.
#[must_use]
pub fn row_model_key(data: &Data, row: Row) -> Option<&str> {
    match row {
        Row::Event(i) => Some(data.events[i].canonical_key.as_str()),
        Row::Model(i) => Some(data.models[i].key.as_str()),
        Row::Listing(i) => Some(data.listings[i].canonical_key.as_str()),
        Row::Source(_) => None,
    }
}

/// A stable identity for keeping the selection across reloads.
#[must_use]
pub fn row_identity(data: &Data, row: Row) -> String {
    match row {
        Row::Event(i) => format!("e{}", data.events[i].id),
        Row::Model(i) => format!("m{}", data.models[i].key),
        Row::Listing(i) => format!("l{}", data.listings[i].key),
        Row::Source(i) => format!("s{}", data.sources[i].id),
    }
}
