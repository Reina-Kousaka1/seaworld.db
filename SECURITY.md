# Security policy

SeaWorld DB is experimental software and is not suitable for production use. Only the current `main` branch and the current `0.0.x` development line are supported; there are no stable release branches or guaranteed patch windows.

Report vulnerabilities through GitHub Private Vulnerability Reporting for this repository, using the private report flow from the Security tab. Do not post exploit details, malicious database files, secrets, or personal data in public issues. If the private report option is not visible, ask a repository maintainer to enable it before sending sensitive details. This repository does not publish a private email address or response-time commitment.

A useful report includes:

- affected commit or release and operating system;
- the API or file operation involved;
- a minimal reproducer and whether the database path/file must be attacker-controlled;
- expected versus observed behavior and any impact on confidentiality, integrity, or availability;
- sanitized logs only, with user data and credentials removed.

The maintainer should acknowledge reports, coordinate a fix and disclosure with the reporter, and publish an advisory after affected users have a reasonable opportunity to update. No response SLA is promised for this experimental project.

Current controls and known limitations are described in [the security model](docs/security-model.md). CRC32C checksums are not authentication. Networking, authentication, authorization, TLS, rate limiting, production-grade resource governance, and a dedicated fuzzing campaign are not implemented. No custom cryptography is planned.
