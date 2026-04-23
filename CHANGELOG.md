# Changelog

All notable changes to this crate are documented in this file.

The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this crate adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
