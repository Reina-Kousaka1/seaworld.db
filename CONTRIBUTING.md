# Contributing

SeaWorld DB is experimental database-engine work. Contributions should preserve the independent storage and query architecture and must not add a hidden SQLite, PostgreSQL, MongoDB, H2, or Derby storage dependency.

Before submitting a change:

1. Read the relevant architecture document and state which invariant the change preserves.
2. Keep file-format changes versioned and document migration or rejection behavior.
3. Add focused tests for persistence, malformed input, and boundary conditions where applicable.
4. Run `./scripts/test.ps1` and address compiler warnings.
5. List new dependencies and their licenses in the change description. Prefer the JDK standard library unless a dependency has a clear, documented benefit.

Do not claim ACID, crash-safety, or performance properties without tests that demonstrate them. Do not add secrets, production database files, or unrelated runtime integrations.

The repository is licensed under AGPL-3.0 as stated in `LICENSE`. Contributions must be submitted under terms compatible with that license. No separate contributor agreement is currently defined.
