use std::{future::Future, pin::Pin, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderValue, Request, Response, StatusCode, Uri, header},
    routing::any,
};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};

use crate::config::{DispatchConfig, DispatchConfigError};

/// Future type returned by `Forwarder` implementations.
pub type ForwardFuture = Pin<Box<dyn Future<Output = Result<Response<Body>, ForwardError>> + Send>>;

/// Forward one fully-rewritten upstream request.
pub trait Forwarder: Send + Sync {
    /// Forward one request to an upstream and return its response.
    fn forward(&self, request: Request<Body>) -> ForwardFuture;
}

/// Errors returned by the concrete forwarder implementation.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ForwardError {
    /// Upstream request could not be completed.
    #[error("upstream request failed: {detail}")]
    UpstreamUnavailable { detail: String },
}

/// Hyper-based forwarder for production use.
#[derive(Clone)]
pub struct HyperForwarder {
    client: Client<HttpConnector, Body>,
}

impl HyperForwarder {
    /// Construct a new hyper-based forwarder.
    pub fn new() -> Self {
        let connector = HttpConnector::new();
        let client = Client::builder(TokioExecutor::new()).build(connector);
        Self { client }
    }
}

impl Default for HyperForwarder {
    fn default() -> Self {
        Self::new()
    }
}

impl Forwarder for HyperForwarder {
    fn forward(&self, request: Request<Body>) -> ForwardFuture {
        let client = self.client.clone();
        Box::pin(async move {
            client
                .request(request)
                .await
                .map(|response| response.map(Body::new))
                .map_err(|err| ForwardError::UpstreamUnavailable {
                    detail: err.to_string(),
                })
        })
    }
}

/// Shared state for router dispatch handlers.
#[derive(Clone)]
pub struct RouterState {
    config: Arc<DispatchConfig>,
    forwarder: Arc<dyn Forwarder>,
}

impl RouterState {
    /// Construct router state from config + forwarder.
    pub fn new(config: DispatchConfig, forwarder: Arc<dyn Forwarder>) -> Self {
        Self {
            config: Arc::new(config),
            forwarder,
        }
    }
}

/// Build the axum router for dispatching all incoming requests.
pub fn router(state: RouterState) -> Router {
    Router::new()
        .fallback(any(dispatch_request))
        .with_state(state)
}

/// Dispatch one incoming request by host to a realm upstream.
pub async fn dispatch_request(
    State(state): State<RouterState>,
    mut request: Request<Body>,
) -> Response<Body> {
    let host = match request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    {
        Some(host) => host,
        None => {
            return response_with_status(StatusCode::BAD_REQUEST, "missing or invalid host header");
        }
    };

    let upstream = match state.config.select_upstream_for_host(host) {
        Ok(upstream) => upstream,
        Err(DispatchConfigError::HostMismatch { .. }) => {
            return response_with_status(
                StatusCode::BAD_REQUEST,
                "host does not match connector domain",
            );
        }
        Err(DispatchConfigError::UnknownRealm { .. }) => {
            return response_with_status(StatusCode::NOT_FOUND, "unknown connector realm");
        }
        Err(_) => {
            return response_with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "router configuration is invalid",
            );
        }
    };

    let rewritten_uri = match rewrite_uri(request.uri(), &upstream) {
        Ok(uri) => uri,
        Err(_) => {
            return response_with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to rewrite upstream URI",
            );
        }
    };
    *request.uri_mut() = rewritten_uri;

    if let Some(authority) = upstream.authority()
        && let Ok(host_header) = HeaderValue::from_str(authority.as_str())
    {
        request.headers_mut().insert(header::HOST, host_header);
    }

    match state.forwarder.forward(request).await {
        Ok(response) => response,
        Err(_) => response_with_status(StatusCode::BAD_GATEWAY, "upstream unavailable"),
    }
}

fn rewrite_uri(original: &Uri, upstream: &Uri) -> Result<Uri, ()> {
    let mut parts = original.clone().into_parts();
    parts.scheme = upstream.scheme().cloned();
    parts.authority = upstream.authority().cloned();
    Uri::from_parts(parts).map_err(|_| ())
}

fn response_with_status(status: StatusCode, body: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(Body::from(body))
        .unwrap_or_else(|_| Response::new(Body::from(body)))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use tower::util::ServiceExt;

    #[derive(Clone, Debug)]
    struct CapturedRequest {
        uri: Uri,
        authorization: Option<HeaderValue>,
        encrypted_payload: Option<HeaderValue>,
    }

    #[derive(Clone, Default)]
    struct MockForwarder {
        captured: Arc<Mutex<Option<CapturedRequest>>>,
    }

    impl Forwarder for MockForwarder {
        fn forward(&self, request: Request<Body>) -> ForwardFuture {
            let captured = self.captured.clone();
            Box::pin(async move {
                let snapshot = CapturedRequest {
                    uri: request.uri().clone(),
                    authorization: request.headers().get(header::AUTHORIZATION).cloned(),
                    encrypted_payload: request.headers().get("X-Encrypted-Payload").cloned(),
                };
                *captured.lock().expect("mutex lock should succeed") = Some(snapshot);

                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from("ok"))
                    .expect("response build should succeed"))
            })
        }
    }

    #[tokio::test]
    async fn host_dispatches_to_expected_realm_upstream() {
        let mut config = DispatchConfig::new("example.com").expect("config should initialize");
        config
            .insert_realm(
                "llm",
                vec![
                    "http://upstream.llm.internal:8080"
                        .parse()
                        .expect("URI should parse"),
                ],
            )
            .expect("realm insertion should succeed");

        let mock_forwarder = MockForwarder::default();
        let app = router(RouterState::new(config, Arc::new(mock_forwarder.clone())));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/chat/completions?stream=true")
                    .header(header::HOST, "llm.connector.example.com")
                    .header(header::AUTHORIZATION, "Bearer token")
                    .header("X-Encrypted-Payload", "deadbeef")
                    .body(Body::from("request"))
                    .expect("request build should succeed"),
            )
            .await
            .expect("router should handle request");

        assert_eq!(response.status(), StatusCode::OK);

        let captured = mock_forwarder
            .captured
            .lock()
            .expect("mutex lock should succeed")
            .clone()
            .expect("forwarder should have captured one request");

        let expected_uri: Uri = "http://upstream.llm.internal:8080/v1/chat/completions?stream=true"
            .parse()
            .expect("URI should parse");
        assert_eq!(captured.uri, expected_uri);
        assert_eq!(
            captured.authorization,
            Some(HeaderValue::from_static("Bearer token"))
        );
        assert_eq!(
            captured.encrypted_payload,
            Some(HeaderValue::from_static("deadbeef"))
        );
    }
}
