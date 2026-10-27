# License options and decision

SeaWorld DB is independent of Tyvera, so its license is selected for SeaWorld's own reuse and contribution goals. No license is copied from Tyvera. The user chose the existing public `Reina-Kousaka1/seaworld.db` repository, whose initial commit already carries AGPL-3.0. That license remains unchanged.

| License | Practical effect to consider |
| --- | --- |
| Apache-2.0 | Permissive reuse and distribution, with attribution/notice conditions and an express contributor patent license. A strong default for broad reuse of an engine and language connectors. [License text](https://www.apache.org/licenses/LICENSE-2.0) |
| MIT | Short and permissive, with notice preservation and warranty disclaimer; it has no express patent grant. [OSI license text](https://opensource.org/license/mit) |
| MPL-2.0 | File-level copyleft: distributed modifications to covered files remain available under MPL, while separate files can be combined under other terms. It also includes an express patent grant. [License text](https://www.mozilla.org/MPL/2.0/) · [FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/) |
| GPLv3 | Stronger copyleft for distributed derivative works; generally requires corresponding source under GPL terms when conveying a covered work. [License text](https://www.gnu.org/licenses/gpl-3.0.en.html) · [FAQ](https://www.gnu.org/licenses/gpl-faq.en.html) |
| AGPLv3 | GPL-style copyleft plus a source-offer requirement for users interacting remotely with a modified network service. More relevant if SeaWorld wants network-service improvements to remain available. [License text](https://www.gnu.org/licenses/agpl-3.0.en.html) · [FSF FAQ](https://www.gnu.org/licenses/gpl-faq.en.html#AGPLv3InteractingRemotely) |

This is a project tradeoff summary, not legal advice. The links above point to official license texts and FAQs.

**Decision: AGPL-3.0-only, inherited from the selected repository's existing `LICENSE` file.** `Cargo.toml`, README, and source SPDX headers use `AGPL-3.0-only`. The full GNU AGPL version 3 text remains in `LICENSE`; this does not grant later AGPL versions.

## Engine dependency licenses

The only direct runtime dependency is [`getrandom` 0.4.3](https://docs.rs/crate/getrandom/0.4.3), MIT OR Apache-2.0. `proptest` 1.12.0 is test-only, licensed MIT OR Apache-2.0, declares Rust 1.88 support (below SeaWorld's Rust 1.89 MSRV), and has recent upstream release/maintenance activity. It was added for generated, shrinkable B+Tree mutation sequences; default fork/timeout features are disabled to reduce dependency surface. The exact locked registry packages and licenses are listed in [`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) and pinned by `Cargo.lock`. Recheck the graph and MSRV before dependency updates.
