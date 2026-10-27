# SQLite connection-local page cache

Issue [#348](https://github.com/cognica-io/uqa-engine/issues/348) identifies the bundled `SQLITE_ENABLE_MEMORY_MANAGEMENT` option, which coordinates connection page caches through a process-wide mutex. The dependency build default must be corrected for published Rust consumers and language bindings as well as repository builds.

## Implementation and ownership

The maintained [rusqlite fork](https://github.com/jaepil/rusqlite) retains the upstream `0.39.0` Rust API and `0.37.0` SQLite/SQLCipher sources. Its only native build change removes `SQLITE_ENABLE_MEMORY_MANAGEMENT`, following upstream commit `f31f7324a82894be13e5de1665cb9af45dc5f869`. Fork revision `509cbaedf58edb0f091837895d1e0ab22a5597b7` is published as `uqa-rusqlite` 0.39.1 and `uqa-libsqlite3-sys` 0.37.1. Native SQLCipher and Emscripten plain SQLite retain their existing features. No repository-only environment override or late process-global configuration is used.

The SQLite provider owns the compiled-option and connection-local reclamation regression. Execution retains its existing temporary-storage dependency; Engine retains its existing connection adapters. The released-file verifier builds old and current consumers in separate Cargo graphs so their distinct native `links = "sqlite3"` packages cannot conflict. Dependency policy prohibits both official and maintained package identities wherever SQLite was already forbidden. The provider's driver re-export denotes the same Rust module used by its callback arguments: it adds no operation or conversion to any UQA carrier. Database formats, query values, transaction boundaries and storage operations are unchanged. The maintainer approved the distinct package/type identity; direct and transitive driver users must follow the [upgrade contract](../manual/reference/10-upgrading.md#062).

## Acceptance record

- [x] Verify upstream provenance, the one build-option change and unchanged native/Rust source bytes.
- [x] Pass 181 local SQLCipher library tests, native build-option checks, encrypted reopen, wrong-key rejection and connection-local reclamation.
- [x] Pass automatic Linux/macOS SQLite and SQLCipher tests, Emscripten execution, both archive verifications and fresh registry installation in [fork run 39321971729](https://github.com/jaepil/rusqlite/actions/runs/39321971729).
- [x] Adopt both published packages with unchanged target-specific features and extend dependency policy to the new identities.
- [x] Verify UQA provider, spill and durable file-mode cases locally against the published dependencies, plus formatting and ownership checks.
- [x] Independently verify actual Linux/macOS work, memory and output in run 39324368853: 1,144 checks / 1,644 required cases and eight process histories per platform, with zero failures, errors, skipped cases or retries. Product source is `eb54c02cbfccd47fe23d5a07edbddd92e0c225c7`; later HEAD `42a58a971` changes only its verification record.
- [x] Merge PR #1144 as `be47c26a9addbaef11f28e65d04eb918189701d3`, close #348 and remove the local/remote branch.

Current UQA local evidence: all 39 connection cases, all 11 encrypted temporary-storage/indexed-spill cases and all 13 Engine durable file-mode cases pass against the published packages. Native and Emscripten dependency graphs retain the expected features, optional foreign-handler isolation passes, and all 17 repository-policy unit tests pass. Strict Clippy for the provider, Execution and Engine passes. The actual crates.io 0.3.6 provider (archive SHA-256 `3db0f72ba7aaf9b64b00ae2b02c8e36a7273d0c969f0b3da47f95d18beb29f6b`) passes creation, current migration, old-writer rejection and two current reopens for native, Key/Value and standalone graph storage in all four plain/encrypted/compressed/combined file modes: twelve scenarios and sixty successful process executions. The current consumer also compiles its typed value import through the provider driver re-export.

These checks establish the compiled correction and behavior preservation. They do not claim a measured throughput gain, resolve historical measurement variance in #653 or close other performance issues.
