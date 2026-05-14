# Changelog

All notable changes to this crate are documented in this file.

The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this crate adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.5] - 2026-05-14

### Fixed
- `HyperForwarder` now strips body-framing and hop-by-hop
  headers (`Content-Length`, `Transfer-Encoding`,
  `Content-Encoding`, `Connection`, `Keep-Alive`, `TE`,
  `Trailer`, `Upgrade`, `Proxy-Authenticate`,
  `Proxy-Authorization`) on **both** the request and response
  sides instead of copying them verbatim. The 0.1.4 mhc swap
  buffered the body via `mhc::Response.bytes()` (which
  transparently decompresses) and re-framed it via
  `Body::from(bytes)`, but kept the upstream's
  `Content-Length` / `Transfer-Encoding` / `Content-Encoding`
  in the forwarded response. The mismatch between the
  upstream's wire framing and the new body's framing tripped
  the downstream hyper response-writer into closing the
  stream mid-flight — surfacing to the upstream caller
  (typically `mechanics-core`'s endpoint client) as
  `mhc::Error::Cancelled` and rendering in JS as a
  `request cancelled` thrown error. RFC 7230 §6.1 hop-by-hop
  headers are also stripped on both sides since they describe
  the upstream-side connection, not the new one the
  forward will travel on.

## [0.1.4] - 2026-05-14

### Fixed
- `HyperForwarder` now uses `mechanics-http-client` (hyper-rustls
  + webpki-roots + aws-lc-rs; opportunistic HTTP/3) instead of
  hyper-util's plain `HttpConnector`. The plain connector forwarded
  the incoming request's `Version` field through unchanged, so an
  inbound HTTP/2 request hit a plain-HTTP connector-bin
  (`axum::serve` on plain TCP, HTTP/1.1 only) as HTTP/2 prior
  knowledge and produced `502 upstream unavailable`. The mhc
  client rebuilds the outbound request internally and negotiates
  the wire version against the connection — HTTP/1.1 on plain
  TCP, HTTP/2 via ALPN over TLS, HTTP/3 via opportunistic QUIC
  when discovered. The `HyperForwarder` name is preserved for
  API compatibility; only the implementation changed.
- `forward_to_upstream` now explicitly resets the request's
  `Version` to `HTTP_11` before handing the request to the
  forwarder. Belt-and-braces against any future forwarder that
  honours the request's version field; mhc rebuilds anyway.
- The 502 path now logs the underlying `ForwardError` detail via
  `tracing::warn!` with `upstream` and `error` fields. The
  previous code swallowed the error with `Err(_)` so a 502 told
  operators nothing about whether the upstream was unreachable,
  responding with a parse error, timing out, or returning a
  malformed response.

### Changed
- Dropped direct `hyper` and `hyper-util` dependencies (replaced
  by mhc's higher-level Client surface). Added `tracing`,
  `mechanics-http-client`. `http-body-util` becomes load-bearing
  (for `BodyExt::collect`) instead of incidental.

## [0.1.3] - 2026-05-14

### Changed
- Internal Cargo.toml audit: `default-features = false` set on
  direct dependencies with explicit feature lists for what the
  crate actually uses. No behaviour change. (D24)

## [0.1.1]

- Added doc comments on error variant fields.

## [0.1.0] - 2026-04-23

### Added

- Added router dispatch library surface with `DispatchConfig`,
  `DispatchConfigError`, `RouterState`, `Forwarder`, and `HyperForwarder`.
- Added host-to-realm mapping for `<realm>.connector.<domain>` and
  per-realm upstream round-robin selection.
- Added wildcard axum handler that forwards requests upstream while
  preserving `Authorization` and `X-Encrypted-Payload` pass-through headers.
- Added minimal async binary entrypoint with environment-driven domain,
  realm, and upstream configuration.
- Added unit test coverage for host-based dispatch to expected upstream
  using a mock forwarder (no real network).
- Added crate README reflecting the Wave B dispatch implementation.
