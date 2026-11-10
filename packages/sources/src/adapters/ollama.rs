//! The ollama.com library page (HTML; there is no JSON catalog API).
//!
//! Each model is an `<a href="/library/NAME">` card holding a description `<p>`, capability
//! badges (`bg-indigo-50` spans), and size badges (`bg-[#ddf4ff]` spans). Scraping is fragile,
//! so results are never complete (layout drift cannot mass-"remove" models) and a page that
//! yields no cards is an error.

use bmodels_model::{ModelInfo, ModelRecord, SourceSpec};

use super::{endpoint, send_conditional, with_headers};
use crate::http::Request;
use crate::parse::plain_text;
use crate::{FetchContext, FetchOutcome, Fetched, SourceError};

const CARD: &str = "href=\"/library/";

pub async fn fetch(spec: &SourceSpec, ctx: &FetchContext<'_>) -> Result<FetchOutcome, SourceError> {
    let request = with_headers(Request::get(endpoint(spec)?), ctx);
    let Some((response, validators)) = send_conditional(ctx, request).await? else {
        return Ok(FetchOutcome::NotModified);
    };
    let records = parse(&response.text());
    if records.is_empty() {
        return Err(SourceError::Parse(
            "no models found on the library page (layout changed?)".to_owned(),
        ));
    }
    Ok(FetchOutcome::Fetched(Fetched {
        records,
        complete: false,
        validators,
        warnings: Vec::new(),
    }))
}

pub fn parse(html: &str) -> Vec<ModelRecord> {
    let starts: Vec<usize> = html.match_indices(CARD).map(|(i, _)| i).collect();
    let mut records: Vec<ModelRecord> = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let after = &html[start + CARD.len()..];
        let Some(end) = after.find('"') else {
            continue;
        };
        let name = &after[..end];
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            || records.iter().any(|r| r.model_id == name)
        {
            continue;
        }
        let card_end = starts.get(n + 1).copied().unwrap_or(html.len());
        let card = &html[start..card_end];
        let mut record = ModelRecord::new(name);
        record.display_name = Some(name.to_owned());
        record.url = Some(format!("https://ollama.com/library/{name}"));
        let mut info = ModelInfo {
            description: element_text(card, "<p class=\"max-w-lg"),
            open_weights: Some(true),
            ..ModelInfo::default()
        };
        info.capabilities.extend(span_texts(card, "bg-indigo-50"));
        info.variants = span_texts(card, "bg-[#ddf4ff]");
        record.info = info;
        records.push(record);
    }
    records
}

/// Text of the first element starting with `open` (up to its closing tag).
fn element_text(card: &str, open: &str) -> Option<String> {
    let start = card.find(open)?;
    let body_start = start + card[start..].find('>')? + 1;
    let tag = open.trim_start_matches('<').split_whitespace().next()?;
    let body_end = body_start + card[body_start..].find(&format!("</{tag}>"))?;
    let text = plain_text(&card[body_start..body_end], 300);
    (!text.is_empty()).then_some(text)
}

/// Texts of every `<span>` whose class contains `class`.
fn span_texts(card: &str, class: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = card;
    while let Some(i) = rest.find("<span") {
        rest = &rest[i..];
        let Some(open_end) = rest.find('>') else {
            break;
        };
        let open = &rest[..open_end];
        let Some(close) = rest.find("</span>") else {
            break;
        };
        if open.contains(class) && close > open_end {
            let text = plain_text(&rest[open_end + 1..close], 40);
            if !text.is_empty() && !out.contains(&text) {
                out.push(text);
            }
        }
        rest = &rest[open_end + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_fixture() {
        let records = parse(include_str!("../../fixtures/ollama.html"));
        assert!(records.len() >= 3, "{}", records.len());
        let first = &records[0];
        assert_eq!(first.model_id, "autotrust-jev");
        assert!(
            first
                .info
                .description
                .as_deref()
                .unwrap()
                .starts_with("AutoTrust")
        );
        assert!(first.info.capabilities.contains("vision"));
        assert_eq!(first.info.variants, ["9b", "35b"]);
    }

    #[test]
    fn ignores_non_model_links() {
        let html = r#"<a href="/library/qwen3:8b">x</a><a href="/library/llama3.3">y</a><a href="/library/llama3.3">dup</a>"#;
        let ids: Vec<_> = parse(html).into_iter().map(|r| r.model_id).collect();
        assert_eq!(ids, ["llama3.3"]);
    }
}
