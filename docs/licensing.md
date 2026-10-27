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

**Decision: AGPL-3.0, inherited from the selected repository's existing `LICENSE` file.** The full license text is preserved unchanged from the repository's initial commit.

## Engine dependency licenses

The Rust implementation adds [`getrandom` 0.4.3](https://docs.rs/crate/getrandom/0.4.3) under its MIT OR Apache-2.0 dual license. Its locked transitive crates are [`cfg-if` 1.0.5](https://docs.rs/crate/cfg-if/1.0.5) and [`libc` 0.2.191](https://docs.rs/crate/libc/0.2.191), both MIT OR Apache-2.0, and [`r-efi` 6.0.0](https://docs.rs/crate/r-efi/6.0.0), which offers MIT OR Apache-2.0 OR LGPL-2.1-or-later. These are dependencies of the application, not copied code. The exact dependency graph is pinned by `Cargo.lock` and should be rechecked when it changes.
