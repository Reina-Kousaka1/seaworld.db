# Security policy

SeaWorld DB `v1.0.0` is the project's current stable release line. **SeaWorld
DB 1.1 Beta** (`v1.1.0-beta`) is the current pre-release; the earlier
`v1.1.0-beta.1` remains a historical release and is not changed by this
release. `main` is the primary security-fix target; reports affecting released
V1 artifacts are also welcome. There are no guaranteed patch windows or
response-time commitments.

Stable release status describes the documented public API and compatibility
contract. It does not expand SeaWorld's published durability, platform, or
production-readiness boundaries. Review the documented limitations before using
SeaWorld with production data.

Report vulnerabilities through GitHub Private Vulnerability Reporting for this repository, using the private report flow from the Security tab. Do not post exploit details, malicious database files, secrets, or personal data in public issues. If the private report option is not visible, ask a repository maintainer to enable it before sending sensitive details. This repository does not publish a private email address or response-time commitment.

A useful report includes:

- affected commit or release and operating system;
- the API or file operation involved;
- a minimal reproducer and whether the database path/file must be attacker-controlled;
- expected versus observed behavior and any impact on confidentiality, integrity, or availability;
- sanitized logs only, with user data and credentials removed.

The maintainer should acknowledge reports, coordinate a fix and disclosure with the reporter, and publish an advisory after affected users have a reasonable opportunity to update. No response SLA is promised.

Current controls and known limitations are described in [the security model](docs/security-model.md). CRC32C checksums are not authentication. Networking, authentication, authorization, TLS, rate limiting, production-grade resource governance, and a dedicated fuzzing campaign are not implemented. No custom cryptography is planned.
