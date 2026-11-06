//! Hugging Face Hub: the newest models of each author in `authors`
//! (`GET /api/models?author=X&sort=createdAt&direction=-1&limit=N`).
//!
//! A window, not a catalog: results are never complete, so models scrolling out of the window
//! are not "removed".

use bmodels_model::{ModelInfo, ModelRecord, SourceSpec};
use serde_json::Value;

use super::{encode, endpoint, send, with_spec_headers};
use crate::http::Request;
use crate::parse::{json, string_at, strings_at, timestamp_at};
use crate::{FetchContext, FetchOutcome, Fetched, SourceError, Validators};

/// Models per author when `limit` is unset.
const DEFAULT_LIMIT: usize = 30;

pub async fn fetch(spec: &SourceSpec, ctx: &FetchContext<'_>) -> Result<FetchOutcome, SourceError> {
    if spec.authors.is_empty() {
        return Err(SourceError::Config(
            "HUGGINGFACE sources need at least one entry in `authors`".to_owned(),
        ));
    }
    let base = endpoint(spec)?;
    let limit = spec.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 1_000);
    let mut records = Vec::new();
    for author in &spec.authors {
        let url = format!(
            "{base}?author={}&sort=createdAt&direction=-1&limit={limit}",
            encode(author)
        );
        let mut request = with_spec_headers(Request::get(url), spec);
        if let Some(key) = &ctx.api_key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        let response = send(ctx, &request).await?;
        records.extend(parse(&json(&response.body)?)?);
    }
    Ok(FetchOutcome::Fetched(Fetched {
        records,
        complete: false,
        validators: Validators::default(),
        warnings: Vec::new(),
    }))
}

pub fn parse(body: &Value) -> Result<Vec<ModelRecord>, SourceError> {
    let items = body
        .as_array()
        .ok_or_else(|| SourceError::Parse("expected an array of models".to_owned()))?;
    Ok(items
        .iter()
        .filter(|item| item.get("private").and_then(Value::as_bool) != Some(true))
        .filter_map(|item| {
            let id = string_at(item, "id").or_else(|| string_at(item, "modelId"))?;
            let mut record = ModelRecord::new(&id);
            record.display_name = id.split_once('/').map(|(_, name)| name.to_owned());
            record.created_at = timestamp_at(item, "createdAt");
            let mut info = ModelInfo {
                open_weights: Some(true),
                ..ModelInfo::default()
            };
            if let Some(pipeline) = string_at(item, "pipeline_tag") {
                info.capabilities.insert(pipeline);
            }
            let tags = strings_at(item, "tags");
            if let Some(license) = tags.iter().find_map(|t| t.strip_prefix("license:")) {
                info.description = Some(format!("license: {license}"));
            }
            record.info = info;
            Some(record)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_fixture() {
        let records =
            parse(&json(include_bytes!("../../fixtures/huggingface.json")).unwrap()).unwrap();
        assert_ne!(records.len(), 0);
        let r = &records[0];
        assert!(r.model_id.starts_with("Qwen/"));
        assert!(r.created_at.is_some());
        assert_eq!(r.info.open_weights, Some(true));
        assert!(!r.info.capabilities.is_empty());
    }
}
