//! One module per [`bmodels_model::SourceKind`], plus shared request helpers.

pub mod anthropic;
pub mod feed;
pub mod gemini;
pub mod huggingface;
pub mod litellm;
pub mod models_dev;
pub mod ollama;
pub mod openai_compatible;
pub mod openrouter;

use std::fmt::Write as _;

use bmodels_model::SourceSpec;

use crate::http::{Request, Response};
use crate::{FetchContext, SourceError, Validators};

/// Maximum pages followed for paginated APIs (a guard against cursor loops).
pub const MAX_PAGES: usize = 50;

/// The spec's URL, else the kind's default.
pub fn endpoint(spec: &SourceSpec) -> Result<String, SourceError> {
    spec.url
        .clone()
        .or_else(|| crate::default_url(spec.kind).map(str::to_owned))
        .ok_or_else(|| SourceError::Config(format!("kind {} requires `url`", spec.kind)))
}

/// Joins a base URL and a path segment with exactly one `/`.
pub fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Adds the spec's extra headers.
pub fn with_spec_headers(mut request: Request, spec: &SourceSpec) -> Request {
    for (name, value) in &spec.headers {
        request = request.header(name, value);
    }
    request
}

/// Sends `request` and fails on non-2xx statuses.
pub async fn send(ctx: &FetchContext<'_>, request: &Request) -> Result<Response, SourceError> {
    let response = ctx.http.get(request).await?;
    check(&request.url, response)
}

/// Turns a non-2xx response into [`SourceError::Status`].
pub fn check(url: &str, response: Response) -> Result<Response, SourceError> {
    if response.is_success() {
        return Ok(response);
    }
    let detail: String = response.text().chars().take(200).collect();
    Err(SourceError::Status {
        status: response.status,
        url: url.split('?').next().unwrap_or(url).to_owned(),
        detail: detail.split_whitespace().collect::<Vec<_>>().join(" "),
    })
}

/// A conditional GET using the context's validators. Returns `None` on `304 Not Modified`.
pub async fn send_conditional(
    ctx: &FetchContext<'_>,
    request: Request,
) -> Result<Option<(Response, Validators)>, SourceError> {
    let request = request
        .header_opt("if-none-match", ctx.validators.etag.as_deref())
        .header_opt("if-modified-since", ctx.validators.last_modified.as_deref());
    let response = ctx.http.get(&request).await?;
    if response.status == 304 {
        return Ok(None);
    }
    let response = check(&request.url, response)?;
    let validators = Validators {
        etag: response.header("etag").map(str::to_owned),
        last_modified: response.header("last-modified").map(str::to_owned),
    };
    Ok(Some((response, validators)))
}

/// Percent-encodes a query parameter value.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_and_encodes() {
        assert_eq!(join("https://a/v1/", "/models"), "https://a/v1/models");
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
    }
}
