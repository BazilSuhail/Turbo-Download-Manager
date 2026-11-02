# Third-party notices

Copyright (c) 2026 Entity Studios.

Turbo Download Manager (TDM) is built on the open-source Rust crates below. This list covers the direct dependencies declared in `Cargo.toml`, with the licence each crate declares in its `Cargo.toml` (checked against the crate sources in the Cargo registry). Each crate's own licence file, shipped in its package, governs that crate. Transitive dependencies are not listed here; they are covered by `Cargo.lock`.

| Crate | Version | Licence |
|---|---|---|
| dirs | 6.0.0 | MIT OR Apache-2.0 |
| futures-util | 0.3.34 | MIT OR Apache-2.0 |
| iced | 0.14.0 | MIT |
| open | 5.4.5 | MIT |
| quick-xml | 0.42.0 | MIT |
| reqwest | 0.13.5 | MIT OR Apache-2.0 |
| serde | 1.0.229 | MIT OR Apache-2.0 |
| serde_json | 1.0.154 | MIT OR Apache-2.0 |
| sha2 | 0.10.9 | MIT OR Apache-2.0 |
| thiserror | 2.0.21 | MIT OR Apache-2.0 |
| tokio | 1.54.1 | MIT |

Where a crate is offered under "MIT OR Apache-2.0", you may use it under either licence. The full texts of MIT and Apache-2.0 are in [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

The cargo-packager installer template in `packaging/windows/installer.nsi` is derived from cargo-packager's stock NSIS template, which is licensed under Apache-2.0 OR MIT.
