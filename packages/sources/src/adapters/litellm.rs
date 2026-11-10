//! LiteLLM's `model_prices_and_context_window.json`: `{model: {litellm_provider, mode,
//! max_input_tokens, input_cost_per_token, deprecation_date, supports_*: bool, ...}}`.
//!
//! The best public source of deprecation dates. `providers` filters on `litellm_provider`.

use bmodels_model::{ModelInfo, ModelRecord, SourceSpec};
use serde_json::Value;

use super::{endpoint, send_conditional, with_headers};
use crate::http::Request;
use crate::parse::{f64_at, json, per_million, str_at, string_at, u64_at};
use crate::{FetchContext, FetchOutcome, Fetched, SourceError, Validators};

pub async fn fetch(spec: &SourceSpec, ctx: &FetchContext<'_>) -> Result<FetchOutcome, SourceError> {
    let request = with_headers(Request::get(endpoint(spec)?), ctx);
    let Some((response, validators)) = send_conditional(ctx, request).await? else {
        return Ok(FetchOutcome::NotModified);
    };
    let mut fetched = parse(&json(&response.body)?, &spec.providers)?;
    fetched.validators = validators;
    Ok(FetchOutcome::Fetched(fetched))
}

pub fn parse(body: &Value, providers: &[String]) -> Result<Fetched, SourceError> {
    let all = body
        .as_object()
        .ok_or_else(|| SourceError::Parse("expected an object of models".to_owned()))?;
    let records = all
        .iter()
        .filter(|(key, _)| key.as_str() != "sample_spec")
        .filter_map(|(key, model)| {
            let provider = str_at(model, "litellm_provider")?;
            if !providers.is_empty() && !providers.iter().any(|p| p == provider) {
                return None;
            }
            Some(record(key, provider, model))
        })
        .collect();
    Ok(Fetched {
        records,
        complete: true,
        validators: Validators::default(),
        warnings: Vec::new(),
    })
}

fn record(key: &str, provider: &str, model: &Value) -> ModelRecord {
    let mut record = ModelRecord::new(key);
    record.vendor_hint = Some(provider.to_owned());
    let mut info = ModelInfo {
        context_window: u64_at(model, "max_input_tokens").or_else(|| u64_at(model, "max_tokens")),
        max_output: u64_at(model, "max_output_tokens"),
        input_price: per_million(f64_at(model, "input_cost_per_token")),
        output_price: per_million(f64_at(model, "output_cost_per_token")),
        deprecation_date: string_at(model, "deprecation_date"),
        ..ModelInfo::default()
    };
    if let Some(fields) = model.as_object() {
        for (field, value) in fields {
            if let Some(capability) = field.strip_prefix("supports_")
                && value.as_bool() == Some(true)
            {
                info.capabilities.insert(capability.to_owned());
            }
        }
    }
    if let Some(mode) = str_at(model, "mode") {
        info.capabilities.insert(format!("mode:{mode}"));
    }
    record.info = info;
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_fixture() {
        let body = json(include_bytes!("../../fixtures/litellm.json")).unwrap();
        let fetched = parse(&body, &["openai".into()]).unwrap();
        let gpt = fetched
            .records
            .iter()
            .find(|r| r.model_id == "gpt-4o")
            .unwrap();
        assert_eq!(gpt.vendor_hint.as_deref(), Some("openai"));
        assert_eq!(gpt.info.context_window, Some(128_000));
        assert_eq!(gpt.info.input_price, Some(2.5));
        assert_eq!(gpt.info.output_price, Some(10.0));
        assert!(gpt.info.deprecation_date.is_some());
        assert!(gpt.info.capabilities.contains("vision"));
        assert!(gpt.info.capabilities.contains("mode:chat"));
        assert!(fetched.records.iter().all(|r| r.model_id != "sample_spec"));
        assert!(
            fetched
                .records
                .iter()
                .all(|r| r.vendor_hint.as_deref() == Some("openai"))
        );
    }
}
