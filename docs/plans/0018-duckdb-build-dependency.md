# DuckDB build dependency correction

Issue [#918](https://github.com/cognica-io/uqa-engine/issues/918) blocks the dependency gate because the optional DuckDB handler resolves `libduckdb-sys 1.10505.0` with yanked ZIP 6.0.0. Updating to the published 1.10506.0 pair retains that dependency. This correction is separate from SQL range semantics and preserves every unfinished original extension record.

## Ownership and delivery

The maintainer selected a fork under [`jaepil/duckdb-rs`](https://github.com/jaepil/duckdb-rs). It retains upstream commit `88f673c5cdf09e287c010a910db53b97e523a9fa`, the exact Rust/native implementation and DuckDB 1.5.5 source archive (`e11f1209cdbb2a99b2ea2d348de21bdfb01dc494b55d1271df94b09c52bf229c`). The separately named packages are `uqa-duckdb 1.10505.1` and `uqa-libduckdb-sys 1.10505.1`, with ZIP 9.0.3 and Rust 1.90. Original MIT attribution is preserved.

UQA's `uqa-fdw` remains the handler owner. Its Cargo alias will select the published fork while leaving independent `arrow` and `duckdb` feature forwarding unchanged. Neither Engine algorithms nor provider formats change. The explicit compatibility consequence approved with the fork choice is distinct Rust package/type identity, including `FDWError::DuckDB`; the original `links = "duckdb"` prevents combining the official and fork native sys packages. A root-only Git patch would not propagate to published consumers and cannot satisfy this delivery.

## Current evidence

- Fork package changes are committed as `13dcb0d`; build-before-publish CI is `5e3cbd8` on the fork's main branch.
- The temporary dependency probe passes the unchanged UQA advisory policy with ZIP 9.0.3. Actual native queries and a Parquet roundtrip pass against DuckDB 1.5.5.
- The renamed fork packages pass parameterized queries, Arrow output, rollback, a Parquet roundtrip and persisted reopening locally. This functional check reused the existing native library; it is not a fresh bundled-build claim.
- [Automatic fork run 38858002109](https://github.com/jaepil/duckdb-rs/actions/runs/38858002109) is verifying Linux and macOS ZIP download/extraction, fresh bundled builds and both registry archives. Publication remains pending and is performed only by tag-triggered CI after both platforms pass.

## Completion conditions

1. Verify both fork archives, preserved upstream Rust/native source and actual functional results on Linux and macOS before either registry upload; then verify an independent registry consumer.
2. Pin the published packages in UQA, update dependency-identity checks and document the Rust type/link consequence. Keep `yanked = "deny"` and native handler feature isolation intact.
3. Run affected FDW ownership/feature tests and dependency checks. Verify the delivered source's actual automatic Linux/macOS work, memory and output reports, not just archive jobs.
4. Merge the focused UQA correction, close #918 and preserve all original extension tasks in the successor PR and progress ledger.

These steps do not close the complete temporal, user-defined range, package registry, SQL extension, host carrier or language SDK records. No UQA version or release is changed by this dependency repair.
