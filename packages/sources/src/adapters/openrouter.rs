//! OpenRouter `GET /api/v1/models`: every model it routes to, with pricing (USD per token,
//! as strings), context length, modalities, and supported parameters.

use bmodels_model::{ModelInfo, ModelRecord, SourceSpec};
use serde_json::Value;

use super::{endpoint, send_conditional, with_spec_headers};
use crate::http::Request;
use crate::parse::{
    f64_at, json, per_million, str_at, string_at, strings_at, timestamp_at, u64_at,
};
use crate::{FetchContext, FetchOutcome, Fetched, SourceError, Validators};

pub async fn fetch(spec: &SourceSpec, ctx: &FetchContext<'_>) -> Result<FetchOutcome, SourceError> {
    let request = with_spec_headers(Request::get(endpoint(spec)?), spec);
    let Some((response, validators)) = send_conditional(ctx, request).await? else {
        return Ok(FetchOutcome::NotModified);
    };
    let mut fetched = parse(&json(&response.body)?)?;
    fetched.validators = validators;
    Ok(FetchOutcome::Fetched(fetched))
}

pub fn parse(body: &Value) -> Result<Fetched, SourceError> {
    let items = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| SourceError::Parse("expected `data` array".to_owned()))?;
    let records = items.iter().filter_map(record).collect();
    Ok(Fetched {
        records,
        complete: true,
        validators: Validators::default(),
        warnings: Vec::new(),
    })
}

fn record(item: &Value) -> Option<ModelRecord> {
    let id = string_at(item, "id")?;
    let mut record = ModelRecord::new(&id);
    // "Google: Gemini 2.5 Pro" → "Gemini 2.5 Pro"; the vendor is already in the id.
    record.display_name = str_at(item, "name").map(|n| match n.split_once(": ") {
        Some((_, rest)) if !rest.is_empty() => rest.to_owned(),
        _ => n.to_owned(),
    });
    record.created_at = timestamp_at(item, "created");
    record.url = Some(format!("https://openrouter.ai/{id}"));

    let architecture = item.get("architecture").unwrap_or(&Value::Null);
    let pricing = item.get("pricing").unwrap_or(&Value::Null);
    let top = item.get("top_provider").unwrap_or(&Value::Null);
    let params = strings_at(item, "supported_parameters");
    let mut info = ModelInfo {
        description: string_at(item, "description"),
        context_window: u64_at(item, "context_length").or_else(|| u64_at(top, "context_length")),
        max_output: u64_at(top, "max_completion_tokens"),
        input_price: per_million(f64_at(pricing, "prompt")),
        output_price: per_million(f64_at(pricing, "completion")),
        input_modalities: strings_at(architecture, "input_modalities"),
        output_modalities: strings_at(architecture, "output_modalities"),
        deprecation_date: string_at(item, "expiration_date"),
        knowledge_cutoff: string_at(item, "knowledge_cutoff"),
        open_weights: item
            .get("hugging_face_id")
            .map(|v| v.as_str().is_some_and(|s| !s.is_empty())),
        ..ModelInfo::default()
    };
    for (param, capability) in [
        ("tools", "tools"),
        ("reasoning", "reasoning"),
        ("structured_outputs", "structured_output"),
    ] {
        if params.iter().any(|p| p == param) {
            info.capabilities.insert(capability.to_owned());
        }
    }
    record.info = info;
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_fixture() {
        let body = json(include_bytes!("../../fixtures/openrouter.json")).unwrap();
        let fetched = parse(&body).unwrap();
        assert!(fetched.complete);
        assert!(fetched.records.len() >= 3);
        let first = &fetched.records[0];
        assert!(first.model_id.contains('/'));
        assert!(!first.display_name.as_deref().unwrap_or(":").contains(": "));
        assert!(first.info.context_window.is_some());
        assert!(
            first
                .url
                .as_deref()
                .unwrap()
                .starts_with("https://openrouter.ai/")
        );
    }

    #[test]
    fn prices_become_per_million_and_negative_is_unknown() {
        let body = serde_json::json!({"data": [
            {"id": "openai/gpt-4o", "name": "OpenAI: GPT-4o", "pricing": {"prompt": "0.0000025", "completion": "0.00001"},
             "supported_parameters": ["tools", "max_tokens"], "hugging_face_id": ""},
            {"id": "openrouter/auto", "pricing": {"prompt": "-1", "completion": "-1"}}
        ]});
        let fetched = parse(&body).unwrap();
        let gpt = &fetched.records[0];
        assert_eq!(gpt.display_name.as_deref(), Some("GPT-4o"));
        assert_eq!(gpt.info.input_price, Some(2.5));
        assert_eq!(gpt.info.output_price, Some(10.0));
        assert!(gpt.info.capabilities.contains("tools"));
        assert_eq!(gpt.info.open_weights, Some(false));
        assert_eq!(fetched.records[1].info.input_price, None);
    }
}
