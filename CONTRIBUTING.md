# Contributing

SeaWorld DB is experimental database-engine work. Contributions must preserve the project's independent document-native architecture and must not add a hidden SQLite, PostgreSQL, MongoDB, H2, or Derby storage dependency.

Before submitting a change:

1. Read the relevant architecture document and state which invariant the change preserves.
2. Keep file-format changes versioned and document migration and rejection behavior.
3. Add focused tests for persistence, malformed input, recovery, and boundaries where applicable. Keep generated property-test failures reproducible and preserve minimized regression cases.
4. Run `cargo fmt --all -- --check`, `cargo clippy --all-targets --features crash-test --locked -- -D warnings`, and `cargo test --all-targets --features crash-test --locked`.
5. List new dependencies, their maintenance/MSRV, and their licenses. Keep runtime dependencies limited to documented engine needs.
6. Do not claim ACID, crash-safety, or performance properties beyond the behaviors tested and documented.

Contributions are submitted under **AGPL-3.0-only**, the license stated in `LICENSE` and `Cargo.toml`. By submitting a contribution, the contributor agrees that it may be distributed under those project terms. There is no CLA and no signed-off-by/DCO requirement at this time. Do not include code or copied text from other database engines; record provenance for any externally sourced documentation or assets.

Do not add secrets, production database files, telemetry, or unrelated runtime integrations.
