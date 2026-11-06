//! Anthropic `GET /v1/models` (cursor pagination via `has_more` / `last_id`).

use bmodels_model::{ModelInfo, ModelRecord, SourceSpec};
use serde_json::Value;

use super::{MAX_PAGES, encode, endpoint, join, send, with_spec_headers};
use crate::http::Request;
use crate::parse::{bool_at, json, string_at, timestamp_at, u64_at};
use crate::{FetchContext, FetchOutcome, Fetched, SourceError, Validators};

/// `anthropic-version` header value.
const API_VERSION: &str = "2023-06-01";

pub async fn fetch(spec: &SourceSpec, ctx: &FetchContext<'_>) -> Result<FetchOutcome, SourceError> {
    let base = join(&endpoint(spec)?, "models");
    let mut records = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let url = after.as_ref().map_or_else(
            || format!("{base}?limit=1000"),
            |cursor| format!("{base}?limit=1000&after_id={}", encode(cursor)),
        );
        let request = with_spec_headers(Request::get(url), spec)
            .header("anthropic-version", API_VERSION)
            .header_opt("x-api-key", ctx.api_key.as_deref());
        let page = json(&send(ctx, &request).await?.body)?;
        let (mut items, next) = parse_page(&page)?;
        records.append(&mut items);
        match next {
            Some(cursor) => after = Some(cursor),
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

/// Records of one page and the cursor of the next, if any.
pub fn parse_page(page: &Value) -> Result<(Vec<ModelRecord>, Option<String>), SourceError> {
    let items = page
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::Parse("expected `data` array".to_owned()))?;
    let records = items
        .iter()
        .filter_map(|item| {
            let mut record = ModelRecord::new(string_at(item, "id")?);
            record.display_name = string_at(item, "display_name");
            record.created_at = timestamp_at(item, "created_at");
            record.info = ModelInfo {
                context_window: u64_at(item, "max_input_tokens"),
                max_output: u64_at(item, "max_tokens"),
                ..ModelInfo::default()
            };
            Some(record)
        })
        .collect();
    let next = if bool_at(page, "has_more") == Some(true) {
        string_at(page, "last_id")
    } else {
        None
    };
    Ok((records, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHttp;

    #[test]
    fn follows_pagination() {
        let http = FakeHttp::new()
            .route(
                "https://api.anthropic.com/v1/models?limit=1000",
                r#"{"data":[{"type":"model","id":"claude-sonnet-4-5-20250929","display_name":"Claude Sonnet 4.5","created_at":"2025-09-29T00:00:00Z"}],"has_more":true,"last_id":"claude-sonnet-4-5-20250929"}"#,
            )
            .route(
                "https://api.anthropic.com/v1/models?limit=1000&after_id=claude-sonnet-4-5-20250929",
                r#"{"data":[{"type":"model","id":"claude-3-haiku-20240307","display_name":"Claude Haiku 3","created_at":"2024-03-07T00:00:00Z"}],"has_more":false}"#,
            );
        let mut spec = SourceSpec::new(bmodels_model::SourceKind::Anthropic);
        spec.api_key_env = Some("K".into());
        let ctx = FetchContext {
            http: &http,
            api_key: Some("sk".into()),
            validators: Validators::default(),
        };
        let FetchOutcome::Fetched(fetched) = crate::testing::block_on(fetch(&spec, &ctx)).unwrap()
        else {
            panic!("expected data");
        };
        let ids: Vec<_> = fetched
            .records
            .iter()
            .map(|r| r.model_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["claude-sonnet-4-5-20250929", "claude-3-haiku-20240307"]
        );
        assert_eq!(
            fetched.records[0].display_name.as_deref(),
            Some("Claude Sonnet 4.5")
        );
        let sent = http.requests();
        assert!(sent[0].headers.contains(&("x-api-key".into(), "sk".into())));
        assert!(
            sent[0]
                .headers
                .contains(&("anthropic-version".into(), API_VERSION.into()))
        );
    }
}
