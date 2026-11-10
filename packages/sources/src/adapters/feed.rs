//! RSS 2.0 and Atom feeds. Items become [`RecordKind::Announcement`] records keyed by
//! `guid` / `id` (else link, else title). Only the newest `limit` items are kept, and the
//! result is never complete (old items falling off a feed are not "removed").

use bmodels_model::{ModelInfo, ModelRecord, RecordKind, SourceSpec};
use roxmltree::{Document, Node};

use super::{endpoint, send_conditional, with_headers};
use crate::http::Request;
use crate::parse::{plain_text, timestamp};
use crate::{FetchContext, FetchOutcome, Fetched, SourceError};

/// Items kept when `limit` is unset.
const DEFAULT_LIMIT: usize = 50;

pub async fn fetch(
    _id: &str,
    spec: &SourceSpec,
    ctx: &FetchContext<'_>,
) -> Result<FetchOutcome, SourceError> {
    let request = with_headers(Request::get(endpoint(spec)?), ctx);
    let Some((response, validators)) = send_conditional(ctx, request).await? else {
        return Ok(FetchOutcome::NotModified);
    };
    let mut records = parse(&response.text())?;
    records.truncate(spec.limit.unwrap_or(DEFAULT_LIMIT));
    for record in &mut records {
        record.vendor_hint.clone_from(&spec.vendor);
    }
    Ok(FetchOutcome::Fetched(Fetched {
        records,
        complete: false,
        validators,
        warnings: Vec::new(),
    }))
}

/// Parses RSS or Atom, newest first when the feed has dates.
///
/// # Errors
///
/// * [`SourceError::Parse`] if the body is not XML or is neither RSS nor Atom
pub fn parse(xml: &str) -> Result<Vec<ModelRecord>, SourceError> {
    let doc = Document::parse(xml).map_err(|e| SourceError::Parse(format!("invalid XML: {e}")))?;
    let root = doc.root_element();
    let mut records: Vec<ModelRecord> = match root.tag_name().name() {
        "rss" | "RDF" => root
            .descendants()
            .filter(|n| n.has_tag_name("item"))
            .filter_map(rss_item)
            .collect(),
        "feed" => root
            .children()
            .filter(|n| n.has_tag_name("entry"))
            .filter_map(atom_entry)
            .collect(),
        other => {
            return Err(SourceError::Parse(format!(
                "expected an RSS or Atom feed, found <{other}>"
            )));
        }
    };
    records.sort_by_key(|r| std::cmp::Reverse(r.created_at));
    Ok(records)
}

fn child_text<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|c| c.has_tag_name(name))
        .and_then(|c| c.text())
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

fn announcement(
    id: &str,
    title: Option<&str>,
    link: Option<&str>,
    date: Option<&str>,
    summary: Option<&str>,
) -> ModelRecord {
    let mut record = ModelRecord::new(id);
    record.kind = RecordKind::Announcement;
    record.display_name = title.map(|t| plain_text(t, 200));
    record.url = link.map(str::to_owned);
    record.created_at = date.and_then(timestamp);
    record.info = ModelInfo {
        description: summary
            .map(|s| plain_text(s, 300))
            .filter(|s| !s.is_empty()),
        ..ModelInfo::default()
    };
    record
}

fn rss_item(item: Node<'_, '_>) -> Option<ModelRecord> {
    let title = child_text(item, "title");
    let link = child_text(item, "link");
    let id = child_text(item, "guid").or(link).or(title)?;
    let date = child_text(item, "pubDate").or_else(|| child_text(item, "date"));
    Some(announcement(
        id,
        title,
        link,
        date,
        child_text(item, "description"),
    ))
}

fn atom_entry(entry: Node<'_, '_>) -> Option<ModelRecord> {
    let title = child_text(entry, "title");
    let link = entry
        .children()
        .filter(|c| c.has_tag_name("link"))
        .find(|c| c.attribute("rel").is_none_or(|r| r == "alternate"))
        .and_then(|c| c.attribute("href"));
    let id = child_text(entry, "id").or(link).or(title)?;
    let date = child_text(entry, "published").or_else(|| child_text(entry, "updated"));
    let summary = child_text(entry, "summary").or_else(|| child_text(entry, "content"));
    Some(announcement(id, title, link, date, summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_rss_fixture() {
        let records = parse(include_str!("../../fixtures/openai_news.xml")).unwrap();
        assert_eq!(records.len(), 3);
        let r = &records[0];
        assert_eq!(r.kind, RecordKind::Announcement);
        assert!(r.model_id.starts_with("https://openai.com/"));
        assert!(r.display_name.is_some());
        assert!(r.created_at.is_some());
        assert!(
            records
                .windows(2)
                .all(|w| w[0].created_at >= w[1].created_at)
        );
    }

    #[test]
    fn parses_atom() {
        let xml = r#"<?xml version="1.0"?>
            <feed xmlns="http://www.w3.org/2005/Atom">
              <entry><id>tag:a,1</id><title>Old</title><updated>2024-01-01T00:00:00Z</updated>
                <link rel="alternate" href="https://a/1"/></entry>
              <entry><id>tag:a,2</id><title>New &amp; shiny</title><published>2025-01-01T00:00:00Z</published>
                <link href="https://a/2"/><summary>&lt;b&gt;hi&lt;/b&gt;</summary></entry>
            </feed>"#;
        let records = parse(xml).unwrap();
        assert_eq!(records[0].model_id, "tag:a,2");
        assert_eq!(records[0].display_name.as_deref(), Some("New & shiny"));
        assert_eq!(records[0].url.as_deref(), Some("https://a/2"));
        assert_eq!(records[0].info.description.as_deref(), Some("hi"));
        assert_eq!(records[1].url.as_deref(), Some("https://a/1"));
    }

    #[test]
    fn rejects_non_feeds() {
        assert!(parse("<html/>").is_err());
        assert!(parse("not xml").is_err());
    }
}
