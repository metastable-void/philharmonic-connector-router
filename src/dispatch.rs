use std::{future::Future, pin::Pin, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderName, HeaderValue, Request, Response, StatusCode, Uri, Version, header},
    routing::any,
};
use mechanics_http_client::Client as MhcClient;

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
    UpstreamUnavailable {
        /// Human-readable description of the upstream failure.
        detail: String,
    },
}

/// Production forwarder for the connector router.
///
/// Built on `mechanics-http-client` so the router can reach connector-bin
/// instances over HTTP/1.1, HTTP/2, or opportunistic HTTP/3 without
/// inspecting the request body.
#[derive(Clone)]
pub struct HyperForwarder {
    client: MhcClient,
}

impl HyperForwarder {
    /// Construct a new forwarder. `mechanics-http-client` init can
    /// only fail at the aws-lc-rs / rustls crypto-provider step,
    /// which represents a build-time misconfiguration of the
    /// process's default crypto provider rather than a runtime
    /// condition — surface it loudly via `expect` per the narrow
    /// init-time exception in CONTRIBUTING.md §10.3.
    pub fn new() -> Self {
        let client = MhcClient::builder()
            .pool_max_idle_per_host(0)
            .build()
            .expect("mechanics-http-client init must not fail (crypto provider setup)");
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
            let (parts, body) = request.into_parts();
            let mut builder = client.request(parts.method, parts.uri.to_string());
            for (name, value) in parts.headers.iter() {
                if should_skip_forwarded_header(name) {
                    continue;
                }
                builder = builder.header(name.clone(), value.clone());
            }
            let response = builder.body_streaming(body).send().await.map_err(|error| {
                ForwardError::UpstreamUnavailable {
                    detail: error.to_string(),
                }
            })?;

            let status = response.status();
            let upstream_headers = response.headers().clone();
            let body = response
                .into_body()
                .map_err(|error| ForwardError::UpstreamUnavailable {
                    detail: error.to_string(),
                })?;

            let mut hyper_response = Response::new(Body::new(body));
            *hyper_response.status_mut() = status;
            *hyper_response.version_mut() = Version::HTTP_11;
            let response_headers = hyper_response.headers_mut();
            for (name, value) in upstream_headers.iter() {
                if should_skip_response_header(name) {
                    continue;
                }
                response_headers.append(name.clone(), value.clone());
            }
            Ok(hyper_response)
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

/// Maximum request-body size accepted by the dispatcher.
///
/// Raised from axum's 2 MiB default so that workflows passing large
/// request bodies (notably `vector_search` corpora — a 1024-dim f32
/// CorpusItem JSON-encodes to ~10-12 KiB, putting the practical limit
/// around 170 items at the 2 MiB default) can pass through the
/// per-realm dispatcher to the connector service. The receiving
/// service still enforces logical caps inside each implementation;
/// this is just the HTTP envelope ceiling.
pub const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Build the axum router for dispatching incoming requests.
///
/// Two dispatch modes:
/// - **Path-based**: `/{realm}` — the lowerer embeds the realm in the URL.
///   No hostname assumptions needed.
/// - **Host-based** (fallback): `Host: <realm>.connector.<domain_suffix>` —
///   for deployments with per-realm DNS.
pub fn router(state: RouterState) -> Router {
    Router::new()
        .route("/{realm}", any(dispatch_by_path))
        .fallback(any(dispatch_by_host))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state)
}

/// Dispatch by realm extracted from URL path (`/{realm}`).
pub async fn dispatch_by_path(
    State(state): State<RouterState>,
    Path(realm): Path<String>,
    mut request: Request<Body>,
) -> Response<Body> {
    let upstream = match state.config.select_upstream_for_realm(&realm) {
        Ok(upstream) => upstream,
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

    let rewritten_uri = match strip_path_realm(request.uri(), &realm) {
        Ok(uri) => uri,
        Err(_) => {
            return response_with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to rewrite realm path",
            );
        }
    };
    *request.uri_mut() = rewritten_uri;

    forward_to_upstream(state.forwarder.as_ref(), request, &upstream).await
}

/// Dispatch by realm extracted from `Host` header
/// (`<realm>.connector.<domain_suffix>`).
pub async fn dispatch_by_host(
    State(state): State<RouterState>,
    request: Request<Body>,
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

    forward_to_upstream(state.forwarder.as_ref(), request, &upstream).await
}

/// Dispatch a request to the upstream for the given realm.
///
/// This is the non-axum entry point — callers extract the realm
/// from the URL path themselves and call this directly, bypassing
/// axum's router/nest machinery.
pub async fn dispatch_to_realm(
    config: &DispatchConfig,
    forwarder: &dyn Forwarder,
    realm: &str,
    request: Request<Body>,
) -> Response<Body> {
    let upstream = match config.select_upstream_for_realm(realm) {
        Ok(upstream) => upstream,
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

    dispatch_to_upstream(forwarder, request, &upstream).await
}

/// Dispatch a request to an already-selected upstream URI.
///
/// This is useful when callers need to select the upstream while
/// holding a configuration lock, then drop that lock before awaiting
/// network I/O.
pub async fn dispatch_to_upstream(
    forwarder: &dyn Forwarder,
    request: Request<Body>,
    upstream: &Uri,
) -> Response<Body> {
    forward_to_upstream(forwarder, request, upstream).await
}

async fn forward_to_upstream(
    forwarder: &dyn Forwarder,
    mut request: Request<Body>,
    upstream: &Uri,
) -> Response<Body> {
    let rewritten_uri = match rewrite_uri(request.uri(), upstream) {
        Ok(uri) => uri,
        Err(_) => {
            return response_with_status(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to rewrite upstream URI",
            );
        }
    };
    *request.uri_mut() = rewritten_uri;

    // Defensive: force HTTP/1.1 on the outbound request. Any inbound
    // axum HTTP/2 request would otherwise propagate its `Version`
    // field into the forwarder and (with the previous hyper-util
    // forwarder) attempt HTTP/2 prior knowledge against an
    // `axum::serve` listener on plain TCP. mhc rebuilds the request
    // internally so this is belt-and-braces; remove if the trait
    // moves to a higher-level RequestBuilder shape.
    *request.version_mut() = Version::HTTP_11;

    if let Some(authority) = upstream.authority()
        && let Ok(host_header) = HeaderValue::from_str(authority.as_str())
    {
        request.headers_mut().insert(header::HOST, host_header);
    }

    match forwarder.forward(request).await {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(
                upstream = %upstream,
                error = %err,
                "connector router: upstream forward failed; returning 502"
            );
            response_with_status(StatusCode::BAD_GATEWAY, "upstream unavailable")
        }
    }
}

/// Headers that must not survive a forwarding hop. Covers the RFC
/// 7230 §6.1 hop-by-hop list; end-to-end representation headers
/// stay with the streamed body.
fn should_skip_forwarded_header(name: &HeaderName) -> bool {
    name == header::HOST
        || name == header::CONTENT_LENGTH
        || name == header::TRANSFER_ENCODING
        || name == header::CONNECTION
        || name == header::PROXY_AUTHENTICATE
        || name == header::PROXY_AUTHORIZATION
        || name == header::TE
        || name == header::TRAILER
        || name == header::UPGRADE
        || name.as_str().eq_ignore_ascii_case("keep-alive")
}

/// Response-side counterpart: hop-by-hop headers + framing headers
/// whose values are owned by this forwarding hop. Representation
/// headers such as `Content-Encoding` stay with the streamed body.
fn should_skip_response_header(name: &HeaderName) -> bool {
    name == header::CONTENT_LENGTH
        || name == header::TRANSFER_ENCODING
        || name == header::CONNECTION
        || name == header::PROXY_AUTHENTICATE
        || name == header::PROXY_AUTHORIZATION
        || name == header::TE
        || name == header::TRAILER
        || name == header::UPGRADE
        || name.as_str().eq_ignore_ascii_case("keep-alive")
}

fn rewrite_uri(original: &Uri, upstream: &Uri) -> Result<Uri, ()> {
    let mut parts = original.clone().into_parts();
    parts.scheme = upstream.scheme().cloned();
    parts.authority = upstream.authority().cloned();
    Uri::from_parts(parts).map_err(|_| ())
}

fn strip_path_realm(original: &Uri, realm: &str) -> Result<Uri, ()> {
    let path_and_query = original
        .path_and_query()
        .map(|value| value.as_str())
        .ok_or(())?;
    let prefix = format!("/{realm}");
    let rest = path_and_query.strip_prefix(&prefix).ok_or(())?;
    let rewritten_path_and_query = if rest.is_empty() {
        "/".to_string()
    } else if rest.starts_with('/') {
        rest.to_string()
    } else if rest.starts_with('?') {
        format!("/{rest}")
    } else {
        return Err(());
    };

    let mut parts = original.clone().into_parts();
    parts.path_and_query = Some(rewritten_path_and_query.parse().map_err(|_| ())?);
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
    use std::convert::Infallible;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use super::*;
    use axum::body::Bytes;
    use http_body::Frame;
    use http_body_util::BodyExt;
    use tokio::net::TcpListener;
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

    struct PendingAfterFirstBody {
        sent: bool,
    }

    impl http_body::Body for PendingAfterFirstBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            if self.sent {
                return Poll::Pending;
            }
            self.sent = true;
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"{")))))
        }
    }

    #[tokio::test]
    async fn hyper_forwarder_dials_before_request_body_finishes() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("test listener should have a local address");
        let request = Request::builder()
            .method("POST")
            .uri(format!("http://{addr}/"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::new(PendingAfterFirstBody { sent: false }))
            .expect("request should build");

        let forwarder = HyperForwarder::new();
        let forward_task = tokio::spawn(async move { forwarder.forward(request).await });

        tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("forwarder should dial before the request body reaches EOF")
            .expect("test listener should accept one connection");
        forward_task.abort();
    }

    #[tokio::test]
    async fn hyper_forwarder_returns_before_response_body_finishes() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let addr = listener
            .local_addr()
            .expect("test listener should have a local address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener
                .accept()
                .await
                .expect("test listener should accept one connection");
            write_all(
                &stream,
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: text/plain\r\n\r\n5\r\nhello\r\n",
            )
            .await;
            tokio::time::sleep(Duration::from_secs(60)).await;
        });

        let request = Request::builder()
            .method("GET")
            .uri(format!("http://{addr}/"))
            .body(Body::empty())
            .expect("request should build");

        let response = tokio::time::timeout(
            Duration::from_secs(1),
            HyperForwarder::new().forward(request),
        )
        .await
        .expect("forwarder should return after upstream response headers")
        .expect("forwarder should accept streaming upstream response");
        assert_eq!(response.status(), StatusCode::OK);

        let mut body = response.into_body();
        let frame = tokio::time::timeout(Duration::from_secs(1), body.frame())
            .await
            .expect("first response chunk should arrive")
            .expect("response body should have a frame")
            .expect("response frame should succeed");
        let data = frame.into_data().expect("frame should be data");
        assert_eq!(data, Bytes::from_static(b"hello"));

        server.abort();
    }

    async fn write_all(stream: &tokio::net::TcpStream, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            stream
                .writable()
                .await
                .expect("test stream should become writable");
            match stream.try_write(bytes) {
                Ok(0) => panic!("test stream closed while writing response"),
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("test response write failed: {error}"),
            }
        }
    }

    #[test]
    fn response_header_filter_preserves_representation_encoding() {
        assert!(!should_skip_response_header(&header::CONTENT_ENCODING));
        assert!(should_skip_response_header(&header::CONTENT_LENGTH));
        assert!(should_skip_response_header(&header::TRANSFER_ENCODING));
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

    #[tokio::test]
    async fn path_dispatches_to_expected_realm_upstream() {
        let mut config = DispatchConfig::new("example.com").expect("config should initialize");
        config
            .insert_realm(
                "prod",
                vec![
                    "http://connector-prod:3002"
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
                    .uri("/prod?trace=true")
                    .header(header::AUTHORIZATION, "Bearer cose-token")
                    .header("X-Encrypted-Payload", "abcdef")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{\"prompt\":\"hi\"}"))
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

        let expected_uri: Uri = "http://connector-prod:3002/?trace=true"
            .parse()
            .expect("URI should parse");
        assert_eq!(captured.uri, expected_uri);
        assert_eq!(
            captured.authorization,
            Some(HeaderValue::from_static("Bearer cose-token"))
        );
    }

    #[tokio::test]
    async fn nested_dispatched_from_fallback_handler() {
        let mut config = DispatchConfig::new("example.com").expect("config should initialize");
        config
            .insert_realm(
                "prod",
                vec![
                    "http://connector-prod:3002"
                        .parse()
                        .expect("URI should parse"),
                ],
            )
            .expect("realm insertion should succeed");

        let mock_forwarder = MockForwarder::default();
        let connector = Router::new().nest(
            "/connector",
            router(RouterState::new(config, Arc::new(mock_forwarder.clone()))),
        );

        let outer = Router::new().fallback(any(move |request: Request<Body>| async move {
            connector.oneshot(request).await.unwrap()
        }));

        let response = outer
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connector/prod")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request build should succeed"),
            )
            .await
            .expect("outer fallback should dispatch to nested connector");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "double-oneshot via fallback should reach connector handler"
        );
    }

    #[tokio::test]
    async fn nested_path_dispatches_through_oneshot() {
        let mut config = DispatchConfig::new("example.com").expect("config should initialize");
        config
            .insert_realm(
                "prod",
                vec![
                    "http://connector-prod:3002"
                        .parse()
                        .expect("URI should parse"),
                ],
            )
            .expect("realm insertion should succeed");

        let mock_forwarder = MockForwarder::default();
        let nested = Router::new().nest(
            "/connector",
            router(RouterState::new(config, Arc::new(mock_forwarder.clone()))),
        );

        let response = nested
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/connector/prod")
                    .header(header::AUTHORIZATION, "Bearer cose-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request build should succeed"),
            )
            .await
            .expect("nested router should handle request");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "nested /connector/prod should match /{{realm}} route"
        );
    }
}
