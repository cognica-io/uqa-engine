# Added-column publication ordering

Status: Active. Issue [#130](https://github.com/cognica-io/uqa-engine/issues/130) is corrected on merged main `f371d200`; PR #155 is merged and its branch is removed. Local product and combined-source integration acceptance are complete. The existing ADD CONSTRAINT regression now checks the exact independently verified PostgreSQL diagnostics. Final Rust CI, review and merge remain.

## Cause and ownership

`ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT 'key1'` registers the physical text field before publishing the column schema. Reserving the generated unique-index name refreshes the command catalog. Analyzer restoration then sees a persisted field binding whose column has not yet been registered and fails with XX000. The PostgreSQL result is row `(1, 1, 'key1')`.

The SQL, Execution and Engine manifests, enabled dictionary features, dependency policy, schema publication and existing atomicity tests were inspected. SQL owns declaration analysis; Execution owns column-addition ordering, constraint identity reservation and schema publication. Engine supplies the existing transaction and field adapters. Correct the ordering in Execution without weakening analyzer validation or adding an Engine algorithm.

## Implementation and acceptance

Retain the actual provider regression before changing production code. Complete constraint identity/name reservation and declared schema publication before publishing physical fields, while retaining the original outer transaction and initializing defaults before statement completion. Verify native SQLite, SQLite Key/Value, redb and direct serialized SQLite through their public Engine constructors.

Compare successful default backfill, duplicate rejection and failure/savepoint rollback with the independent PostgreSQL 18.4 oracle in `tests/parity/pg18/added_column_oracle.sql`. Verify reopen preserves both data and uniqueness, rejected additions leave no field/analyzer/schema residue, and the same column name can subsequently be added. Run focused existing ALTER/default/generated/atomicity checks, strict affected-crate Clippy and repository ownership/dependency checks. Keep the manual, HISTORY, parity evidence and issue current; keep this correction in a separate PR after the #155 merge and branch cleanup.

The initial four-provider regression run executes twelve cases: ten fail and two direct-serialized cases pass. Native SQLite, SQLite Key/Value and redb reproduce the premature analyzer restoration; direct serialized SQLite separately accepts a duplicate-producing UNIQUE default instead of PostgreSQL 23505. Reuse the existing key-validation owner after backfill, without repeating declaration-name validation against the already published constraint. Preserve the PostgreSQL primary diagnostic and DETAIL for this same added-key boundary. The reproduction command is `cargo test -p uqa-engine --locked --test integration storage::catalog_atomicity::added_columns::`; the original run returned ten failures and two successes. Raw output stays outside Git.

The correction publishes the declared schema before physical text/vector fields and reuses the key-validation owner after backfill. Key data validation is separated from declaration/name checks; duplicate diagnostics reuse the existing permission-aware key renderer. All twelve four-provider regressions pass, including exact PostgreSQL primary message and DETAIL. Existing catalog atomicity, DDL, generated-column and UNIQUE selections pass 178 tests in total, including the twelve new cases. Strict all-target Execution/Engine Clippy passes. The manual SQL compile/execute harness, parity manifest and all repository ownership, dependency, harness, formatting, header and file-limit checks pass. Current-main and combined-source integration acceptance are recorded below; final platform CI, review and merge remain.

## Current-main integration

On merged main `e8e6e3c0`, Linux Docker passes `cargo test -p uqa-engine --locked --test integration -- storage::catalog_atomicity:: catalog::sql_ddl:: catalog::sql_generated_columns:: catalog::sql_unique_constraint:: queries::manual_sql_examples::manual_sql_examples_compile_or_execute`: 178 affected catalog, DDL, generated-column and UNIQUE cases plus the manual SQL compile/execute harness, 179 tests total. Strict `cargo clippy -p uqa-execution -p uqa-engine --all-targets --locked -- -D warnings` passes in the Rust 1.90 container with libclang. Dependency, Engine ownership, single-harness and parity-manifest checks pass.

The Docker PostgreSQL 18.4 execution of `tests/parity/pg18/added_column_oracle.sql` matches all nine records in `added_column_oracle.expected.txt`, including primary messages, DETAIL and statement/savepoint rollback. Review confirms that the existing ADD CONSTRAINT path reserves its canonical index name before shared value validation, so its diagnostics keep the same naming and permission boundaries. No product changes were needed during this integration review, and the branch does not include the unmerged #124 correction.

## Parallel integration acceptance

Independent source `eeb2935549fa3fcbee6cefb03e880d70667e36c1` started [Rust/Linux/macOS](https://github.com/cognica-io/uqa-engine/actions/runs/36080686787), [JavaScript/WASM](https://github.com/cognica-io/uqa-engine/actions/runs/36080680419) and [Python](https://github.com/cognica-io/uqa-engine/actions/runs/36080683722) while PR #155 was being verified. These runs cover the independent #130 correction on `e8e6e3c0`; the #155 runs separately cover the upstream SQLite snapshot correction. Subsequent record edits do not change product code.

The runtime and test changes from `e8e6e3c0` to `eeb29355` were applied to PR #155 source `bdf360c7` in a detached worktree, without adding a branch or PR. Linux Docker `cargo test -p uqa-engine --locked --test integration storage::catalog_atomicity::added_columns::` passes all twelve cases on that combined source. This covers default preservation, unique enforcement, failure cleanup, savepoint rollback and reopen across all four providers with the corrected SQLite snapshot owner. Raw output remains outside Git.

The branch is rebased onto the actual #155 squash merge `f371d200`. Before the subsequent diagnostic fixture correction, an empty diff outside documentation and HISTORY against the accepted combined tree confirmed identical runtime and test sources. Only the compatibility-plan ledger required conflict resolution; the #124 completion and #130 reproduction entries are both preserved. The product source remains unchanged after this alignment.

## Constraint diagnostic acceptance

The first platform run exposed an old ADD CONSTRAINT assertion that required the primary message to contain `duplicate`. PostgreSQL 18.4 returns `could not create unique index` as the primary message and reports the duplicated values in DETAIL. The existing test now verifies SQLSTATE 23505, the exact primary message and DETAIL, unchanged rows and absence of a partially published constraint for both explicit and automatically assigned index names. The product diagnostic was already correct.

The expanded Docker PostgreSQL oracle matches all twelve checked-in records. Linux Docker `cargo test -p uqa-engine --locked --test integration -- catalog::sql_on_conflict:: storage::catalog_atomicity::added_columns::` passes all thirty-three cases on merged main `f371d200`. The first Linux run separately exposed a pre-existing VACUUM fixture assumption that an unrequested column cannot already have automatic statistics; that fixture is corrected below. The remaining jobs in the superseded Rust run were cancelled after preserving both failing diagnostics, and final Rust acceptance runs on the corrected combined source. Runtime code has not changed, so the independent binding runs and upstream #155 runs retain their original source scope.

## Scoped VACUUM fixture acceptance

The VACUUM fixture explicitly runs `ANALYZE vacuum_parent` before checking column-scoped maintenance. Both inherited column counts start at two; `VACUUM (ANALYZE) ONLY vacuum_parent (a)` changes only `a` to zero and preserves `b` at two, and descendant analysis of `b` preserves `a` at zero. Explicit ANALYZE advances the existing maintenance state, so an older automatic sample is rejected by the publication check. The fixture therefore tests column and descendant selection independently of automatic-worker scheduling, without sleeps or retries.

Linux Docker `cargo test -p uqa-engine --locked --test integration storage::transaction_lifecycle::pg18_vacuum_xmin::` passes all fifteen cases. This correction changes one existing test and no product behavior. The separate commit preserves its scope; final Linux/macOS CI will verify both corrected fixtures alongside the added-column implementation.
