# Justine Controller integration profile

Status: contract mapping only; no SeaWorld server, remote probe, or JC client
is implemented here.

SeaWorld DB is an embedded Rust engine. Its Public API 1.0.0 is
transport-neutral and does not expose a network health endpoint. A controller
must not open the same writable database file as a running owner process merely
to check health. This branch does not change the API or storage behavior.

The reserved peer identity is `urn:jc:db:seaworld-db`. A future separately
reviewed transport must use TLS 1.3 mTLS, a durable replay sequence, bounded
health values, and a non-mutating operation that does not disclose database
content. JC's current profile reports `unknown/no_verified_transport` and
never opens a SeaWorld file.

`feature/jc-integration` isolates this work from the stable `main` branch and
Public API 1.0.0. No stable release or production integration is changed.
