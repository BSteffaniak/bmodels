//! Test doubles for [`Http`], usable by this crate's tests and by downstream crates (the
//! engine's sync tests).

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::SourceError;
use crate::http::{Http, Request, Response};

/// Serves canned responses by exact URL and records every request.
#[derive(Debug, Default)]
pub struct FakeHttp {
    routes: Mutex<BTreeMap<String, Result<Response, SourceError>>>,
    requests: Mutex<Vec<Request>>,
}

impl FakeHttp {
    /// An empty fake (every request is a 404).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Serves `200 OK` with `body` for `url`.
    #[must_use]
    pub fn route(self, url: &str, body: impl Into<Vec<u8>>) -> Self {
        self.set(url, Ok(Response::ok(body)));
        self
    }

    /// Serves `response` (or a transport error) for `url`, replacing any previous route.
    pub fn set(&self, url: &str, response: Result<Response, SourceError>) {
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(url.to_owned(), response);
    }

    /// Every request received so far.
    #[must_use]
    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Http for FakeHttp {
    async fn get(&self, request: &Request) -> Result<Response, SourceError> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        self.routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&request.url)
            .cloned()
            .unwrap_or_else(|| {
                Ok(Response {
                    status: 404,
                    headers: BTreeMap::new(),
                    body: b"not found".to_vec(),
                })
            })
    }
}

/// Runs a future to completion on a fresh current-thread runtime.
///
/// # Panics
///
/// * If the runtime cannot be created
pub fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("test runtime")
        .block_on(future)
}
