# philharmonic-connector-router

Per-realm HTTP dispatcher for the Philharmonic connector layer.

This crate is the pure dispatcher tier of the Philharmonic connector
triangle: it terminates TLS on `<realm>.connector.<your-domain>`,
routes incoming requests to a connector service instance inside that
realm, and does **nothing else**. It never verifies tokens, never
decrypts payloads, and never reads the request body. Authorization
and decryption both happen at the connector service on the far side.

Part of the Philharmonic workspace:
https://github.com/metastable-void/philharmonic-workspace

## Responsibilities

- Accept HTTPS on one or more configured listen addresses.
- Map the authority component (`<realm>.connector.<...>`) to an
  upstream pool of `philharmonic-connector-service` instances for
  that realm.
- Forward the request (headers + body) to a realm upstream using a
  simple round-robin strategy.
- Surface dispatch-level errors (`400` host mismatch, `404` unknown
  realm, `502` upstream unavailable, `500` invalid router config).

## Non-responsibilities

- Parsing, verifying, or inspecting connector authorization tokens
  (`Authorization: Bearer <COSE_Sign1 ...>`).
- Parsing, decrypting, or inspecting encrypted payloads
  (`X-Encrypted-Payload: <COSE_Encrypt0 ...>`).
- Rate limiting or replay protection.
- Any per-tenant business logic.

Both in-transit envelopes travel through the router as opaque bytes.
That is deliberate: the router is deployed in the realm's front-door
position but is cryptographically uninvolved, which keeps its trust
surface minimal.

## Wave B surface

Library API:

- `DispatchConfig`: connector-domain suffix plus per-realm upstream pools.
- `RouterState`: shared runtime state with config + `Forwarder`.
- `router(...)`: wildcard axum router entrypoint.
- `dispatch_request(...)`: host-based upstream dispatch handler.
- `HyperForwarder`: default hyper-based forwarding implementation.

Binary (`src/main.rs`) environment contract:

- `PHILHARMONIC_ROUTER_LISTEN` (optional, default `127.0.0.1:3000`)
- `PHILHARMONIC_ROUTER_DOMAIN` (required; for example `example.com`)
- `PHILHARMONIC_ROUTER_REALM` (required; one realm for minimal deployment)
- `PHILHARMONIC_ROUTER_UPSTREAMS` (required; comma-separated upstream URIs)

The binary is intentionally minimal and only wires library primitives.

## Current status

Wave B dispatch skeleton is implemented and tested at crate version
`0.0.0` pending connector-triangle Gate-2 review and coordinated
publish/version changes at workspace level.

See [`ROADMAP.md`](../ROADMAP.md) §5 and
[`docs/design/08-connector-architecture.md`](../docs/design/08-connector-architecture.md)
for the full triangle architecture.

Sibling crates in the triangle:

- [`philharmonic-connector-common`](../philharmonic-connector-common/) —
  shared vocabulary (token claims, realm model, wrapper types).
- [`philharmonic-connector-client`](../philharmonic-connector-client/) —
  lowerer-side mint + encrypt.
- [`philharmonic-connector-service`](../philharmonic-connector-service/) —
  realm-side verify + decrypt + dispatch to implementations.

## License

Dual-licensed under `Apache-2.0 OR MPL-2.0`. See
[LICENSE-APACHE](LICENSE-APACHE) and [LICENSE-MPL](LICENSE-MPL).

SPDX-License-Identifier: `Apache-2.0 OR MPL-2.0`

## Contributing

This crate is developed as a submodule of the Philharmonic
workspace. Workspace-wide development conventions — git workflow,
script wrappers, Rust code rules, versioning, terminology — live
in the workspace meta-repo at
[metastable-void/philharmonic-workspace](https://github.com/metastable-void/philharmonic-workspace),
authoritatively in its
[`CONTRIBUTING.md`](https://github.com/metastable-void/philharmonic-workspace/blob/main/CONTRIBUTING.md).
