//! `models.dev/api.json`: `{provider_id: {name, doc, models: {model_id: {...}}}}`.
//!
//! Listing ids are `provider/model` and the provider is the vendor hint. `providers` limits
//! which providers are read (models.dev lists ~200 resellers of the same models).

use bmodels_model::{ModelInfo, ModelRecord, SourceSpec};
use serde_json::Value;

use super::{endpoint, send_conditional, with_headers};
use crate::http::Request;
use crate::parse::{bool_at, f64_at, json, string_at, strings_at, timestamp_at, u64_at};
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
        .ok_or_else(|| SourceError::Parse("expected an object of providers".to_owned()))?;
    let mut warnings = Vec::new();
    let selected: Vec<(&String, &Value)> = if providers.is_empty() {
        all.iter().collect()
    } else {
        providers
            .iter()
            .filter_map(|p| {
                let found = all.get_key_value(p);
                if found.is_none() {
                    warnings.push(format!("models.dev has no provider `{p}`"));
                }
                found
            })
            .collect()
    };
    let mut records = Vec::new();
    for (provider_id, provider) in selected {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (model_key, model) in models {
            let model_id = string_at(model, "id").unwrap_or_else(|| model_key.clone());
            records.push(record(provider_id, &model_id, model));
        }
    }
    Ok(Fetched {
        records,
        complete: true,
        validators: Validators::default(),
        warnings,
    })
}

fn record(provider: &str, model_id: &str, model: &Value) -> ModelRecord {
    let mut record = ModelRecord::new(format!("{provider}/{model_id}"));
    record.vendor_hint = Some(provider.to_owned());
    record.display_name = string_at(model, "name");
    record.created_at = timestamp_at(model, "release_date");
    let limit = model.get("limit").unwrap_or(&Value::Null);
    let cost = model.get("cost").unwrap_or(&Value::Null);
    let modalities = model.get("modalities").unwrap_or(&Value::Null);
    let mut info = ModelInfo {
        description: string_at(model, "description"),
        context_window: u64_at(limit, "context").filter(|c| *c > 0),
        max_output: u64_at(limit, "output").filter(|c| *c > 0),
        input_price: f64_at(cost, "input"),
        output_price: f64_at(cost, "output"),
        input_modalities: strings_at(modalities, "input"),
        output_modalities: strings_at(modalities, "output"),
        knowledge_cutoff: string_at(model, "knowledge"),
        open_weights: bool_at(model, "open_weights"),
        ..ModelInfo::default()
    };
    for (flag, capability) in [
        ("reasoning", "reasoning"),
        ("tool_call", "tools"),
        ("attachment", "attachments"),
        ("structured_output", "structured_output"),
    ] {
        if bool_at(model, flag) == Some(true) {
            info.capabilities.insert(capability.to_owned());
        }
    }
    if string_at(model, "status").as_deref() == Some("deprecated") {
        info.capabilities.insert("deprecated".to_owned());
    }
    record.info = info;
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_fixture_with_provider_filter() {
        let body = json(include_bytes!("../../fixtures/models_dev.json")).unwrap();
        let fetched = parse(&body, &["openai".into(), "nope".into()]).unwrap();
        assert_eq!(fetched.warnings, ["models.dev has no provider `nope`"]);
        assert_ne!(fetched.records.len(), 0);
        let r = fetched
            .records
            .iter()
            .find(|r| r.model_id == "openai/gpt-4o-2024-08-06")
            .unwrap();
        assert_eq!(r.vendor_hint.as_deref(), Some("openai"));
        assert_eq!(r.info.context_window, Some(128_000));
        assert_eq!(r.info.input_price, Some(2.5));
        assert!(r.info.capabilities.contains("tools"));
        assert_eq!(r.created_at, Some(1_722_902_400));
        assert!(
            fetched
                .records
                .iter()
                .all(|r| r.model_id.starts_with("openai/"))
        );
    }
}
