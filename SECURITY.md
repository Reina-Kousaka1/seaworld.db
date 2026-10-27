# Security policy

SeaWorld DB is experimental and is not suitable for untrusted database files or production use. The first implementation validates headers, record bounds, checksums, text encoding, and document nesting limits, but it does not provide a complete security boundary.

Please report suspected vulnerabilities privately to the repository maintainer rather than opening a public issue with exploit details. Include the affected revision, a minimal reproduction, and whether the input file or path must be attacker-controlled. The maintainer will publish a reporting address before inviting public contributions.

Networking, authentication, authorization, TLS, and rate limiting are not implemented. No custom cryptography is planned.
