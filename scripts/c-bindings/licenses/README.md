# Pinned dependency license sources

The published `alloc-stdlib` 0.2.2, `flatbuffers` 25.12.19 and `thrift` 0.17.0 crate archives omit their upstream root license files. These copies retain the unmodified license and applicable notice texts from each crate's published `.cargo_vcs_info.json` revision. [sources.json](sources.json) records the exact upstream URL, revision and SHA-256 for every file.

The native package license collector uses these copies only for the matching package version and source revision, and verifies their hashes before installation. It reads other dependency licenses and bundled native notices from the selected Cargo package sources, and includes the installed Rust toolchain's own copyright inventory. A new package without license files fails collection until its source is reviewed and pinned.
