//! The HTTP seam. Adapters only see [`Http`], so they run against recorded fixtures in tests
//! ([`crate::testing::FakeHttp`]) and against `switchy_http` in production ([`SwitchyHttp`]).

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;

use crate::SourceError;

/// User agent sent with every request.
pub const USER_AGENT: &str = concat!(
    "bmodels/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/BSteffaniak/bmodels)"
);

/// A GET request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Absolute URL including the query string.
    pub url: String,
    /// Headers, in order.
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// A request with no headers.
    #[must_use]
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: Vec::new(),
        }
    }

    /// Adds a header.
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Adds a header when `value` is `Some`.
    #[must_use]
    pub fn header_opt(self, name: &str, value: Option<&str>) -> Self {
        match value {
            Some(v) => self.header(name, v),
            None => self,
        }
    }
}

/// A response with its whole body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Headers with lowercase names.
    pub headers: BTreeMap<String, String>,
    /// Body bytes.
    pub body: Vec<u8>,
}

impl Response {
    /// A `200 OK` response with `body`.
    #[must_use]
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: BTreeMap::new(),
            body: body.into(),
        }
    }

    /// A header by lowercase name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// Whether the status is 2xx.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// The body as (lossy) UTF-8.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Performs GET requests.
#[async_trait]
pub trait Http: Send + Sync {
    /// Sends `request` and reads the whole body. Non-2xx statuses are returned, not errors.
    ///
    /// # Errors
    ///
    /// * [`SourceError::Http`] on connection failures and timeouts
    async fn get(&self, request: &Request) -> Result<Response, SourceError>;
}

/// [`Http`] over `switchy_http` (reqwest).
pub struct SwitchyHttp {
    client: switchy_http::Client,
    timeout: Duration,
}

impl std::fmt::Debug for SwitchyHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwitchyHttp")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl SwitchyHttp {
    /// Creates a client whose requests (including reading the body) time out after
    /// `timeout`.
    ///
    /// # Errors
    ///
    /// * [`SourceError::Http`] if the HTTP client cannot be initialized (e.g. no TLS backend)
    pub fn new(timeout: Duration) -> Result<Self, SourceError> {
        let client = switchy_http::Client::builder()
            .timeout(timeout)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| SourceError::Http(format!("failed to create HTTP client: {e}")))?;
        Ok(Self { client, timeout })
    }
}

#[async_trait]
impl Http for SwitchyHttp {
    async fn get(&self, request: &Request) -> Result<Response, SourceError> {
        let mut builder = self.client.get(&request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let exchange = async {
            let mut response = builder.send().await?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                .collect();
            let body = response.bytes().await?.to_vec();
            Ok::<_, switchy_http::Error>(Response {
                status,
                headers,
                body,
            })
        };
        exchange.await.map_err(|error| {
            SourceError::Http(if error.is_timeout() {
                format!(
                    "timed out after {}",
                    bmodels_model::format_duration(self.timeout)
                )
            } else {
                error.to_string()
            })
        })
    }
}
