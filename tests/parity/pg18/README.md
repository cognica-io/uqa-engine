# PG18 differential probes

`run_diff.py` validates `manifest.json`, executes every probe in `probes.sql` against a real PostgreSQL 18 instance and against the `usql` release binary, then reports divergences in four categories:

- `engine-error`: PostgreSQL answers, the engine rejects (missing feature).
- `engine-accepts`: PostgreSQL rejects, the engine answers (missing guard, e.g. division by zero).
- `sqlstate-mismatch`: both engines reject, but with different SQLSTATE codes.
- `value-mismatch`: both answer, values differ after normalization (boolean display and numerically equivalent float formatting are normalized; JSON and JSONB output text is compared exactly).

## Prerequisites

- A PostgreSQL 18 container named `uqa-pg18` with user `postgres`, database `uqa`:

  ```sh
  docker run -d --name uqa-pg18 \
    -e POSTGRES_PASSWORD=uqa -e POSTGRES_DB=uqa \
    -p 15432:5432 postgres:18
  ```

- A release build of the CLI: `cargo build --release -p uqa-cli`.

## Run

```sh
python3 tests/parity/pg18/run_diff.py --validate-manifest
python3 tests/parity/pg18/run_diff.py
```

Manifest schema version 2 records the pinned parser chain, oracle provenance, milestone titles and exit gates, exact single ownership of every evidence item, positive evidence, and every currently tracked incomplete gate. The validator derives milestone states from owned item statuses, synchronizes the plan ledger and manual snapshot, rejects malformed ownership, stale wrapper revisions, duplicate or orphaned items, verified items with open issues, and any complete-compatibility claim made before M6 and every item are complete.

The differential summary line reports `total/match/diff`, and any difference makes the runner exit nonzero. Error rows match only when their SQLSTATE codes match; message text is not compared. At this revision `probes.sql` contains 793 probes. Update it freely: one probe per line, `--` comments skipped; probes must be side-effect-free single statements. Set `UQA_PG_CONTAINER`, `UQA_PG_DATABASE`, or `UQA_USQL` to override the defaults while keeping both systems under test in equivalent contexts.

## MERGE and RETURNING oracle

[`merge_returning_oracle.md`](merge_returning_oracle.md) records the pinned PostgreSQL 18.4 container provenance, full-join candidate results, clause-order and visibility SQLSTATEs, repeated-target cardinality behavior, all mutation row images, `DO NOTHING`, source-column NULLs, `merge_action()`, and source-before-target `RETURNING *` layout used by the focused compiler and engine tests.

## Stateful routine oracle

`run_routines_stateful.py` executes the 129 delimited cases in `routines_stateful.sql` against PostgreSQL 18.4 with Apache AGE and UQA, then compares both results with `routines_stateful.expected.json`. It covers polymorphic and variadic resolution, pseudo-type declaration validation, user `pg_proc` identity, ALTER lifecycle, persisted concrete bindings, routine ownership and security, session portals, transitive function and procedure `DROP CASCADE` effects, and exact SQLSTATEs.

The same runner's `--suite roles` mode executes the 100 cases in `roles_stateful.sql` and compares them with `roles_stateful.expected.json`. It covers PostgreSQL 18 role-membership defaults and option changes, independent grantors, ADMIN dependency `RESTRICT` and `CASCADE`, cycles, CREATE ROLE membership clauses, legacy ALTER GROUP, transactions and reopen, `pg_auth_members`, CREATEROLE attribute-delegation limits, transitive SET, INHERIT, and ADMIN behavior, all six name/OID `pg_has_role` overloads and privilege modes, stable-function generated-column rejection, membership-aware routine ownership and ACLs, unauthorized replacement and drop, SECURITY INVOKER role persistence, SECURITY DEFINER restrictions, role-drop dependencies, and exact SQLSTATEs.

The same runner's `--suite constraints` mode executes `constraints_stateful.sql` and compares it with `constraints_stateful.expected.json`. The 162-case transcript covers named CHECK, foreign-key, and `NOT NULL` `NOT VALID` state, validation and enforcement failure atomicity, supported ALTER forms, inferred primary-key references, exact referenced-key and physical-partition event identity, directional and temporal cross-type keys, initially-deferred outer-commit and savepoint behavior, `SET CONSTRAINTS` name resolution and trigger recreation, dependency-aware drops and pending-event precedence, multi-action rollback, catalog flags, and exact SQLSTATEs.

The `--suite type-temporal` mode executes `type_temporal_stateful.sql` and compares it with `type_temporal_stateful.expected.json`. It covers built-in range and multirange identity, canonical values and operators, polymorphic range routine resolution, failure-atomic type rewrites, `WITHOUT OVERLAPS`, aggregate `PERIOD` coverage, catalog persistence, and exact SQLSTATEs.

The `--suite triggers` mode executes the 584 cases in `triggers_stateful.sql` and compares them with `triggers_stateful.expected.json`. It covers trigger creation and executable replacement, row and statement execution, `WHEN` validation and timing, generated-row images, zero-row updates, `TRUNCATE`, constraint-trigger immediate and deferred execution, `SET CONSTRAINTS`, captured row images, savepoint and commit lifecycle, trigger-owned constraints, partition clones, catalogs, enable and independent rename lifecycle, `session_replication_role` mode selection and foreign-key suppression, dependency drops, queued-event cancellation, transition-relation definition validation, typed setwise execution for multi-row and zero-row mutations, `INSERT SELECT`, `ON CONFLICT`, `UPDATE FROM`, `MERGE`, partition-moving UPDATE, UPDATE FROM, and MERGE row-trigger and transition lifecycles including cancellation and destination mutation, partition and inheritance descendants, cumulative direct multi-foreign-key actions, statement-global event ordering across independently prepared multi-row conflict-update cascade trees, PostgreSQL's recursive chain and branching cascade waves with coalesced, split, and trailing empty sets, statement-start source and target snapshots across `BEFORE` statement-trigger writes, `INSTEAD OF` view-trigger definition validation, alphabetical row chaining, suppression, statement timing, row-image `RETURNING`, `UPDATE FROM`, `DELETE USING`, rename, drop, enable-mode rejection, catalog and reopen behavior, automatically updatable nested view `INSERT`, implicit leading-column values and queries, scalar, `EXISTS`, and `IN` subqueries in view projections and predicates, correlated and unqualified view-definition references, local-alias collision avoidance, statement snapshots, `OLD` and `NEW` computed row images, check options, rewrite-rule images, `ON CONFLICT`, `UPDATE FROM`, `DELETE USING`, `MERGE`, public-row-type name boundaries with fixed star expansion, correlated scalar-subquery binding across the complete containing DML namespace, including target and `excluded` rows, `FROM` or `USING` sources, and explicit `OLD` and `NEW` `RETURNING` aliases, ordinary source relations named `old` or `new` and source-only hidden-name resolution including unaliased derived sources, target-before-source `UPDATE FROM` and `DELETE USING` plus source-before-target `MERGE RETURNING *` layout, computed projections and physical partition `tableoid` row images, base-trigger routing, automatic `MERGE` user-rule rejection, nested-layer view `ALSO` and `INSTEAD` rule suppression and action ordering including nonautomatic rule-backed inner-view boundaries, qualification-before-action row projection, provider-level `RETURNING` subqueries, pre-routing NULL `tableoid`, suppressed `UPDATE` and `DELETE` zero command counts, final-action `INSERT` rule command counts, base-rule defaults, actual rule-provided `RETURNING` selection, direct rule-plus-trigger ordering, conditional-INSTEAD rejection, unconditional-rule input, source, assignment, and view-materialization laziness, omitted computed-column NULL rule images, computed-column action inputs, `CASE` short-circuiting, rule action event cardinality, view-rule `ON CONFLICT`, uniqueness-before-check-option error order, duplicate view mappings, rule-backed information-schema flags, original view-trigger suppression, and suppressed INSERT identity/default behavior, duplicate ordinary `id` persistence across reopen, defined-but-suppressed `INSTEAD OF` triggers, inner-to-outer `LOCAL` and `CASCADED` check options after `BEFORE` triggers including `MERGE`, failure atomicity, system and no-writable-column handling, materialized-view relation errors, information-schema flags, target-list SRF and CREATE or ALTER check-option rejection, non-updatable shape errors, canonical catalog deparsing, nested-routine scope isolation, persistence guards, and exact SQLSTATEs.

The trigger suite's view `MERGE` slice covers direct and nested automatic-to-trigger targets, INSERT, UPDATE, DELETE, and DO NOTHING actions, action-path selection and errors, statement-trigger order, current, OLD, and NEW row images, NULL suppression, repeated candidates, replication-mode suppression, user-rule rejection, final check options, hidden target rows, failure atomicity, and statement-start snapshots.

The `--suite rules` mode executes the 194 rewrite-rule cases in `rules_stateful.sql` and compares them with `rules_stateful.expected.json`. It covers `OLD` and `NEW` binding including nullable integer row images, collision-free and correlated LATERAL action sources, PostgreSQL CTE, set-operation member, conditional set-operation action, and `ON CONFLICT` reference-scope errors, qualified and unqualified conditions, alphabetical action ordering, `ALSO`, conditional and unconditional `INSTEAD`, `NOTHING`, set-oriented action and statement-trigger cardinality, INSERT SELECT, row-independent UPDATE and DELETE action qualification cardinality including no-predicate, false-predicate, empty-target, `UPDATE FROM`, and `DELETE USING` cases, positional DML `RETURNING` provider validation, lazy projection evaluation, action row images, aliases, UPDATE-provider `UPDATE FROM` source columns, DELETE-provider `DELETE USING` source columns, view-target action validation, canonical recursion detection, DML restrictions, `session_replication_role` mode selection, `pg_rewrite` and `pg_rules`, enable and rename lifecycle, persistence-safe replacement, token-safe column dependency rewrites, reserved `_RETURN` naming, view `_RETURN` replacement and protection, materialized-view rejection, and exact SQLSTATEs.

The `--suite transactions` mode executes the 55 cases in `transaction_stateful.sql` and compares them with `transaction_stateful.expected.json`. It covers execution-free relation and column validation at `DECLARE`, lazy row evaluation, deferred execution errors and transaction cleanup, typed zero-movement and one-row incremental portal execution, declaration-time relation, virtual-catalog, view-name, nested-view-plan, literal-regclass sequence, routine-definition, and inheritance binding, `AccessShare` relation locking, `DECLARE`-time snapshots including this transaction's own changes, fixed-snapshot relation lifetimes and transaction-local writes across transactional DDL, volatile cursor routines observing earlier cursor writes, stable relation OIDs across rename and `TRUNCATE` with replacement after drop and recreation, PostgreSQL holdable-cursor rewind and materialization timing, deferred-constraint revalidation after materialization, targeted `VACUUM FULL`, read-only and post-write nontransactional `ANALYZE`, snapshot acquisition by `PREPARE`, and `pg_attribute` missing-value behavior for fast and volatile added-column defaults.

## Transaction catalog visibility oracle

`transaction_catalog_visibility_oracle.sql` uses `dblink` to commit catalog and storage changes from a sibling PostgreSQL 18.4 session after a `REPEATABLE READ` snapshot is established. The checked-in transcript verifies that the fixed snapshot uses current committed view and routine definitions, resolves a relation created after snapshot acquisition with no snapshot-visible rows, and treats a concurrently truncated relation as empty. The paired `pg18_fixed_snapshot_*` Rust integration tests exercise the same catalog identities and row-visibility boundaries in UQA, including transaction-local DDL and rollback.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/transaction_catalog_visibility_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/transaction_catalog_visibility_oracle.expected.txt -
cargo test --locked -p uqa-engine --test integration pg18_fixed_snapshot
```

The PostgreSQL side keeps one generated schema across case-specific `psql` connections. The UQA side keeps one temporary database file and deliberately reopens it for every case, so the same comparison also verifies durable routine, view, generated-column, catalog, and ALTER state. Successful observation cases use COPY text rows; type-sensitive cases project `pg_typeof(...)`; expected failures compare SQLSTATE exactly.

Build the pinned PostgreSQL 18.4 and Apache AGE 1.8.0 oracle from AGE commit `b570cf7c1486863f77c14e9c0e07b0e9bfd01bf4`; `Dockerfile.pg18-age` also pins the PostgreSQL multi-platform image digest used for the checked-in transcript:

```sh
repo_root=$(git rev-parse --show-toplevel)
oracle_source=$(mktemp -d)
git -C "$oracle_source" init
git -C "$oracle_source" remote add origin https://github.com/apache/age.git
git -C "$oracle_source" fetch --depth=1 origin b570cf7c1486863f77c14e9c0e07b0e9bfd01bf4
git -C "$oracle_source" checkout --detach FETCH_HEAD
docker build --file "$repo_root/tests/parity/pg18/Dockerfile.pg18-age" --tag uqa-pg18-age:1.8.0 "$oracle_source"
docker run -d --name uqa-pg18-age -e POSTGRES_PASSWORD=uqa -e POSTGRES_DB=postgres uqa-pg18-age:1.8.0
```

Build the current CLI before running the oracle:

```sh
cargo build --release -p uqa-cli --bin usql
python3 tests/parity/pg18/run_routines_stateful.py
python3 tests/parity/pg18/run_routines_stateful.py --suite roles
python3 tests/parity/pg18/run_routines_stateful.py --suite constraints
python3 tests/parity/pg18/run_routines_stateful.py --suite type-temporal
python3 tests/parity/pg18/run_routines_stateful.py --suite triggers
python3 tests/parity/pg18/run_routines_stateful.py --suite rules
python3 tests/parity/pg18/run_routines_stateful.py --suite transactions
```

The runner executes PostgreSQL and UQA concurrently by default. `--backend postgres` and `--backend uqa` select one side for diagnosis. Canonical transcript updates require the PostgreSQL-only backend and use an atomic file replacement; regenerate only from the pinned PostgreSQL 18.4 + AGE oracle, then review the checked-in JSON diff:

```sh
python3 tests/parity/pg18/run_routines_stateful.py --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite roles --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite constraints --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite type-temporal --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite triggers --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite rules --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite transactions --backend postgres --update-expected
```

Every fixture case starts with `-- @case <name> <ok|rows|error>` and ends with `-- @end`; this explicit framing allows routine bodies to contain semicolons without making the runner guess SQL statement boundaries. The runner replaces `__UQA_STATEFUL_SCHEMA__` and the role-suite placeholders with isolated generated names and rejects an expected transcript whose fixture SHA-256 or ordered case modes are stale.

## Routine security and cursor oracle

[`routine_security_cursor_oracle.md`](routine_security_cursor_oracle.md) records the PostgreSQL 18.4 with Apache AGE owner, EXECUTE ACL, `SECURITY DEFINER`, dynamic `current_user` versus stable `session_user`, routine configuration, planner-support metadata, `refcursor` type identity, and cross-call session-portal results used by the focused compiler and engine tests.

## Protocol client matrix

`clients/run.sh` builds pinned psycopg, pgx, and node-postgres images, provisions a password-authenticated role in a running PostgreSQL 18.4 container, checks the deterministic operation/version evidence from each driver against `clients/expected.json`, and reruns the same operations against the server fixture assembled from `uqa-pg-wire`. The matrix covers prepared reuse, binary formats, COPY in and out, failed-transaction rollback recovery, and one-connection pool reuse; it also runs the existing PostgreSQL 18 psql/libpq protocol 3.0/3.2 suite.

The default container name is `pg-parity`, the default published PostgreSQL port is `15432`, and the runner uses the Docker runtime's host-gateway alias. Override `UQA_PG18_WIRE_CONTAINER`, `UQA_PG18_ORACLE_PORT`, or `UQA_PG18_DOCKER_HOST` for another local runtime, then run:

```sh
bash tests/parity/pg18/clients/run.sh
```
