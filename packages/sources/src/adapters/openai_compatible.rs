//! `GET {url}/models`: OpenAI and every server that copies its API (Mistral, xAI, Groq,
//! DeepSeek, Together, Cerebras, vLLM, llama.cpp, LM Studio, ...).
//!
//! Accepts `{"data": [...]}` and bare arrays (Together). Picks up the common extensions:
//! `context_window` / `context_length` / `max_context_length` / `max_model_len`, Together's
//! `display_name` and `pricing` (USD per 1M), Mistral's `capabilities` and `deprecation`,
//! and skips Groq models with `"active": false`.

use bmodels_model::{ModelInfo, ModelRecord, SourceSpec};
use serde_json::Value;

use super::{endpoint, join, send, with_headers};
use crate::http::Request;
use crate::parse::{bool_at, f64_at, json, str_at, string_at, timestamp_at, u64_at};
use crate::{FetchContext, FetchOutcome, Fetched, SourceError, Validators};

pub async fn fetch(spec: &SourceSpec, ctx: &FetchContext<'_>) -> Result<FetchOutcome, SourceError> {
    let url = join(&endpoint(spec)?, "models");
    let mut request = with_headers(Request::get(url), ctx);
    if let Some(key) = &ctx.api_key {
        request = request.header("authorization", format!("Bearer {}", key.expose()));
    }
    let response = send(ctx, &request).await?;
    Ok(FetchOutcome::Fetched(parse(&json(&response.body)?)?))
}

pub fn parse(body: &Value) -> Result<Fetched, SourceError> {
    let items = body
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())
        .ok_or_else(|| SourceError::Parse("expected `data` array".to_owned()))?;
    let mut records = Vec::with_capacity(items.len());
    let mut warnings = Vec::new();
    for item in items {
        let Some(id) = string_at(item, "id") else {
            warnings.push("skipped a model without `id`".to_owned());
            continue;
        };
        if bool_at(item, "active") == Some(false) {
            continue;
        }
        let mut record = ModelRecord::new(id);
        record.display_name = string_at(item, "display_name").or_else(|| string_at(item, "name"));
        record.created_at = timestamp_at(item, "created");
        record.info = info(item);
        records.push(record);
    }
    Ok(Fetched {
        records,
        complete: true,
        validators: Validators::default(),
        warnings,
    })
}

fn info(item: &Value) -> ModelInfo {
    let mut info = ModelInfo {
        description: string_at(item, "description"),
        owned_by: string_at(item, "owned_by").or_else(|| string_at(item, "organization")),
        context_window: [
            "context_window",
            "context_length",
            "max_context_length",
            "max_model_len",
        ]
        .iter()
        .find_map(|k| u64_at(item, k)),
        max_output: u64_at(item, "max_completion_tokens")
            .or_else(|| u64_at(item, "max_output_tokens")),
        deprecation_date: str_at(item, "deprecation").map(|d| d.get(..10).unwrap_or(d).to_owned()),
        ..ModelInfo::default()
    };
    if let Some(pricing) = item.get("pricing") {
        info.input_price = f64_at(pricing, "input").filter(|p| *p >= 0.0);
        info.output_price = f64_at(pricing, "output").filter(|p| *p >= 0.0);
    }
    if let Some(caps) = item.get("capabilities").and_then(Value::as_object) {
        for (name, value) in caps {
            if value.as_bool() == Some(true) {
                info.capabilities.insert(name.clone());
            }
        }
    }
    if let Some(kind) = str_at(item, "type").filter(|t| *t != "model") {
        info.capabilities.insert(kind.to_owned());
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_shape() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "gpt-4o", "object": "model", "created": 1_715_367_049, "owned_by": "system"},
                {"object": "model"}
            ]
        });
        let fetched = parse(&body).unwrap();
        assert!(fetched.complete);
        assert_eq!(fetched.records.len(), 1);
        assert_eq!(fetched.warnings.len(), 1);
        let r = &fetched.records[0];
        assert_eq!(r.model_id, "gpt-4o");
        assert_eq!(r.created_at, Some(1_715_367_049));
        assert_eq!(r.info.owned_by.as_deref(), Some("system"));
    }

    #[test]
    fn together_bare_array_and_groq_inactive() {
        let body = serde_json::json!([
            {"id": "meta-llama/Llama-3.3-70B-Instruct-Turbo", "type": "chat", "display_name": "Llama 3.3 70B",
             "context_length": 131_072, "pricing": {"input": 0.88, "output": 0.88}},
            {"id": "old", "active": false}
        ]);
        let fetched = parse(&body).unwrap();
        assert_eq!(fetched.records.len(), 1);
        let r = &fetched.records[0];
        assert_eq!(r.display_name.as_deref(), Some("Llama 3.3 70B"));
        assert_eq!(r.info.context_window, Some(131_072));
        assert_eq!(r.info.input_price, Some(0.88));
        assert!(r.info.capabilities.contains("chat"));
    }

    #[test]
    fn mistral_capabilities_and_deprecation() {
        let body = serde_json::json!({"data": [{
            "id": "mistral-large-2411", "max_context_length": 131_072,
            "capabilities": {"completion_chat": true, "function_calling": true, "vision": false},
            "deprecation": "2026-05-31T12:00:00Z"
        }]});
        let r = &parse(&body).unwrap().records[0];
        assert_eq!(r.info.deprecation_date.as_deref(), Some("2026-05-31"));
        assert!(r.info.capabilities.contains("function_calling"));
        assert!(!r.info.capabilities.contains("vision"));
    }
}
