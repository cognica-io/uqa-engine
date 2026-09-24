# Added-column publication ordering

Status: Active. Issue [#130](https://github.com/cognica-io/uqa-engine/issues/130) is reproduced on merged main `2892d5bc`; PostgreSQL 18.4 accepts the original statement and preserves the existing row. Work branch: `fix/pg18-added-column-publication`. PR #154 remains the sole open correction PR.

## Cause and ownership

`ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT 'key1'` registers the physical text field before publishing the column schema. Reserving the generated unique-index name refreshes the command catalog. Analyzer restoration then sees a persisted field binding whose column has not yet been registered and fails with XX000. The PostgreSQL result is row `(1, 1, 'key1')`.

The SQL, Execution and Engine manifests, enabled dictionary features, dependency policy, schema publication and existing atomicity tests were inspected. SQL owns declaration analysis; Execution owns column-addition ordering, constraint identity reservation and schema publication. Engine supplies the existing transaction and field adapters. Correct the ordering in Execution without weakening analyzer validation or adding an Engine algorithm.

## Implementation and acceptance

Retain the actual provider regression before changing production code. Complete constraint identity/name reservation and declared schema publication before publishing physical fields, while retaining the original outer transaction and initializing defaults before statement completion. Verify native SQLite, SQLite Key/Value, redb and direct serialized SQLite through their public Engine constructors.

Compare successful default backfill, duplicate rejection and failure/savepoint rollback with the independent PostgreSQL 18.4 oracle in `tests/parity/pg18/added_column_oracle.sql`. Verify reopen preserves both data and uniqueness, rejected additions leave no field/analyzer/schema residue, and the same column name can subsequently be added. Run focused existing ALTER/default/generated/atomicity checks, strict affected-crate Clippy and repository ownership/dependency checks. Keep the manual, HISTORY, parity evidence and issue current; open no second PR while #154 remains active.

The initial four-provider regression run executes twelve cases: ten fail and two direct-serialized cases pass. Native SQLite, SQLite Key/Value and redb reproduce the premature analyzer restoration; direct serialized SQLite separately accepts a duplicate-producing UNIQUE default instead of PostgreSQL 23505. Reuse the existing key-validation owner after backfill, without repeating declaration-name validation against the already published constraint. Preserve the PostgreSQL primary diagnostic and DETAIL for this same added-key boundary. Evidence: `/private/tmp/uqa-added-column-reproduction.log`; raw output stays outside Git.
