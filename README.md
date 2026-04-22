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
- Forward the request (headers + body) to a healthy upstream using a
  simple strategy (round-robin or least-connections); stream the
  response back to the client.
- Surface basic dispatch-level errors (no upstreams, bad request
  authority, upstream connect failure) as well-typed HTTP responses.

## Non-responsibilities

- Parsing, verifying, or inspecting connector authorization tokens
  (`Authorization: Bearer <COSE_Sign1 ...>`).
- Parsing, decrypting, or inspecting encrypted payloads
  (`X-Encrypted-Payload: <COSE_Encrypt0 ...>`).
- Rate limiting or replay protection — stateless replay suppression
  is out of scope for v1; any rate limiting belongs in the bin that
  wraps the library.
- Any per-tenant business logic.

Both of the in-transit envelopes travel through the router as opaque
bytes. That's by design: the router is deployed in the realm's
front-door position but is deliberately cryptographically uninvolved,
which keeps its trust surface minimal.

## Current status

**Placeholder.** The routing core lands with Phase 5 Wave B
(hybrid-KEM + COSE_Encrypt0). Until then the crate is published at
`0.0.0` as a name reservation only. See
[`ROADMAP.md`](../ROADMAP.md) §5 and
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
