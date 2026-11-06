//! Google Generative Language `GET /v1beta/models` (pagination via `nextPageToken`).

use bmodels_model::{ModelInfo, ModelRecord, SourceSpec};
use serde_json::Value;

use super::{MAX_PAGES, encode, endpoint, join, send, with_spec_headers};
use crate::http::Request;
use crate::parse::{bool_at, json, str_at, string_at, strings_at, u64_at};
use crate::{FetchContext, FetchOutcome, Fetched, SourceError, Validators};

pub async fn fetch(spec: &SourceSpec, ctx: &FetchContext<'_>) -> Result<FetchOutcome, SourceError> {
    let base = join(&endpoint(spec)?, "models");
    let mut records = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let url = token.as_ref().map_or_else(
            || format!("{base}?pageSize=1000"),
            |t| format!("{base}?pageSize=1000&pageToken={}", encode(t)),
        );
        let request = with_spec_headers(Request::get(url), spec)
            .header_opt("x-goog-api-key", ctx.api_key.as_deref());
        let page = json(&send(ctx, &request).await?.body)?;
        let (mut items, next) = parse_page(&page)?;
        records.append(&mut items);
        match next {
            Some(t) => token = Some(t),
            None => break,
        }
    }
    Ok(FetchOutcome::Fetched(Fetched {
        records,
        complete: true,
        validators: Validators::default(),
        warnings: Vec::new(),
    }))
}

/// Records of one page and the next page token, if any.
pub fn parse_page(page: &Value) -> Result<(Vec<ModelRecord>, Option<String>), SourceError> {
    let items = page
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::Parse("expected `models` array".to_owned()))?;
    let records = items
        .iter()
        .filter_map(|item| {
            let name = str_at(item, "name")?;
            let id = name.strip_prefix("models/").unwrap_or(name);
            let mut record = ModelRecord::new(id);
            record.display_name = string_at(item, "displayName");
            let mut info = ModelInfo {
                description: string_at(item, "description"),
                context_window: u64_at(item, "inputTokenLimit"),
                max_output: u64_at(item, "outputTokenLimit"),
                ..ModelInfo::default()
            };
            info.capabilities
                .extend(strings_at(item, "supportedGenerationMethods"));
            if bool_at(item, "thinking") == Some(true) {
                info.capabilities.insert("reasoning".to_owned());
            }
            record.info = info;
            Some(record)
        })
        .collect();
    Ok((records, string_at(page, "nextPageToken")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_models_prefix_and_reads_limits() {
        let page = serde_json::json!({
            "models": [{
                "name": "models/gemini-2.5-pro",
                "displayName": "Gemini 2.5 Pro",
                "inputTokenLimit": 1_048_576,
                "outputTokenLimit": 65_536,
                "supportedGenerationMethods": ["generateContent", "countTokens"],
                "thinking": true
            }],
            "nextPageToken": "abc"
        });
        let (records, next) = parse_page(&page).unwrap();
        assert_eq!(next.as_deref(), Some("abc"));
        let r = &records[0];
        assert_eq!(r.model_id, "gemini-2.5-pro");
        assert_eq!(r.info.context_window, Some(1_048_576));
        assert!(r.info.capabilities.contains("reasoning"));
        assert!(r.info.capabilities.contains("generateContent"));
    }
}
