# Security policy

SeaWorld DB is experimental and is not suitable for production use. The engine validates headers, record bounds, checksums, text encoding, identifiers, and document nesting limits before accepting persisted records. This is not a complete security boundary: CRC32C detects accidental damage, not malicious edits; no adversarial fuzzing campaign or formal resource-governance system is in place.

Please report suspected vulnerabilities privately to the repository maintainer rather than opening a public issue with exploit details. Include the affected revision, a minimal reproduction, and whether the input file or path must be attacker-controlled. The maintainer will publish a reporting address before inviting public contributions.

Networking, authentication, authorization, TLS, and rate limiting are not implemented. No custom cryptography is planned.
