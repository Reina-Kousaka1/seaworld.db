# Third-party notices

SeaWorld DB does not copy source code from another database engine. The following registry packages are resolved in `Cargo.lock`. License identifiers were read from the manifests reported by `cargo metadata --locked` for this lockfile. Keep this list synchronized with dependency updates.

| Package | Version | Use | Declared license |
| --- | ---: | --- | --- |
| `getrandom` | 0.4.3 | Runtime: database UUID bytes | MIT OR Apache-2.0 |
| `cfg-if` | 1.0.5 | Transitive runtime | MIT OR Apache-2.0 |
| `libc` | 0.2.191 | Transitive runtime | MIT OR Apache-2.0 |
| `r-efi` | 6.0.0 | Transitive runtime on UEFI targets | MIT OR Apache-2.0 OR LGPL-2.1-or-later |
| `proptest` | 1.12.0 | Dev-only property testing | MIT OR Apache-2.0 |
| `autocfg` | 1.5.1 | Transitive dev dependency | Apache-2.0 OR MIT |
| `bitflags` | 2.13.2 | Transitive dev dependency | MIT OR Apache-2.0 |
| `chacha20` | 0.10.2 | Transitive dev dependency | MIT OR Apache-2.0 |
| `core_detect` | 1.0.0 | Transitive dev dependency | MIT/Apache-2.0 |
| `cpufeatures` | 0.3.1 | Transitive dev dependency | MIT OR Apache-2.0 |
| `num-traits` | 0.2.19 | Transitive dev dependency | MIT OR Apache-2.0 |
| `rand` | 0.10.3 | Transitive dev dependency | MIT OR Apache-2.0 |
| `rand_core` | 0.10.1 | Transitive dev dependency | MIT OR Apache-2.0 |
| `rand_xorshift` | 0.5.0 | Transitive dev dependency | MIT OR Apache-2.0 |
| `regex-syntax` | 0.8.11 | Transitive dev dependency | MIT OR Apache-2.0 |
| `unarray` | 0.1.4 | Transitive dev dependency | MIT OR Apache-2.0 |

These dependencies are not linked into a distributed SeaWorld binary except `getrandom` and its runtime transitive dependencies. `proptest` and its dependency tree are used only while testing. This notice summarizes metadata; it does not replace upstream license texts. Consult the corresponding crate source/archive for full terms. No dependency license is copied or substituted by SeaWorld's `AGPL-3.0-only` license.
