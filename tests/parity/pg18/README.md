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

The differential summary line reports `total/match/diff`, and any difference makes the runner exit nonzero. Error rows match only when their SQLSTATE codes match; message text is not compared. Update `probes.sql` freely: one probe per line, `--` comments skipped; probes must be side-effect-free single statements. Set `UQA_PG_CONTAINER`, `UQA_PG_DATABASE`, or `UQA_USQL` to override the defaults while keeping both systems under test in equivalent contexts.

## MERGE and RETURNING oracle

[`merge_returning_oracle.md`](merge_returning_oracle.md) records the pinned PostgreSQL 18.4 container provenance, full-join candidate results, clause-order and visibility SQLSTATEs, repeated-target cardinality behavior, all mutation row images, `DO NOTHING`, source-column NULLs, `merge_action()`, and source-before-target `RETURNING *` layout used by the focused compiler and engine tests.

## Stateful routine oracle

`run_routines_stateful.py` executes the delimited cases in `routines_stateful.sql` against PostgreSQL 18.4 with Apache AGE and UQA, then compares both results with `routines_stateful.expected.json`. It covers polymorphic and variadic resolution, pseudo-type declaration validation, user `pg_proc` identity, ALTER lifecycle, persisted concrete bindings, bounded function `DROP CASCADE` effects, and no-dependent procedure CASCADE removal.

The same runner's `--suite constraints` mode executes `constraints_stateful.sql` and compares it with `constraints_stateful.expected.json`. The 84-case transcript covers named CHECK, foreign-key, and `NOT NULL` `NOT VALID` state, validation and enforcement failure atomicity, supported ALTER forms, inferred primary-key references, exact referenced-key identity, directional and temporal cross-type keys, initially-deferred outer-commit and savepoint behavior, dependency-aware drops, multi-action rollback, catalog flags, and exact SQLSTATEs.

The `--suite type-temporal` mode executes `type_temporal_stateful.sql` and compares it with `type_temporal_stateful.expected.json`. It covers built-in range and multirange identity, canonical values and operators, polymorphic range routine resolution, failure-atomic type rewrites, `WITHOUT OVERLAPS`, aggregate `PERIOD` coverage, catalog persistence, and exact SQLSTATEs.

The `--suite triggers` mode executes the 36 cases in `triggers_stateful.sql` and compares them with `triggers_stateful.expected.json`. It covers trigger creation and executable replacement, row and statement execution, `WHEN` validation and timing, generated-row images, zero-row updates, `TRUNCATE`, catalogs, enable and rename lifecycle, dependency drops, and exact SQLSTATEs.

The `--suite rules` mode executes the 177 rewrite-rule cases in `rules_stateful.sql` and compares them with `rules_stateful.expected.json`. It covers `OLD` and `NEW` binding including nullable integer row images, collision-free and correlated LATERAL action sources, PostgreSQL CTE, set-operation member, conditional set-operation action, and `ON CONFLICT` reference-scope errors, qualified and unqualified conditions, alphabetical action ordering, `ALSO`, conditional and unconditional `INSTEAD`, `NOTHING`, set-oriented action and statement-trigger cardinality, INSERT SELECT, positional DML `RETURNING` provider validation, lazy projection evaluation, action row images, aliases, UPDATE-provider `UPDATE FROM` source columns, DELETE-provider `DELETE USING` source columns, view-target action validation, canonical recursion detection, DML restrictions, `pg_rewrite` and `pg_rules`, enable and rename lifecycle, persistence-safe replacement, token-safe column dependency rewrites, reserved `_RETURN` naming, view `_RETURN` replacement and protection, materialized-view rejection, and exact SQLSTATEs.

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
python3 tests/parity/pg18/run_routines_stateful.py --suite constraints
python3 tests/parity/pg18/run_routines_stateful.py --suite type-temporal
python3 tests/parity/pg18/run_routines_stateful.py --suite triggers
python3 tests/parity/pg18/run_routines_stateful.py --suite rules
```

The runner executes PostgreSQL and UQA concurrently by default. `--backend postgres` and `--backend uqa` select one side for diagnosis. Canonical transcript updates require the PostgreSQL-only backend and use an atomic file replacement; regenerate only from the pinned PostgreSQL 18.4 + AGE oracle, then review the checked-in JSON diff:

```sh
python3 tests/parity/pg18/run_routines_stateful.py --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite constraints --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite type-temporal --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite triggers --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite rules --backend postgres --update-expected
```

Every fixture case starts with `-- @case <name> <ok|rows|error>` and ends with `-- @end`; this explicit framing allows routine bodies to contain semicolons without making the runner guess SQL statement boundaries. The runner replaces `__UQA_STATEFUL_SCHEMA__` with an isolated generated schema name and rejects an expected transcript whose fixture SHA-256 or ordered case modes are stale.

## Routine security and cursor oracle

[`routine_security_cursor_oracle.md`](routine_security_cursor_oracle.md) records the PostgreSQL 18.4 with Apache AGE owner, EXECUTE ACL, `SECURITY DEFINER`, dynamic `current_user` versus stable `session_user`, routine configuration, planner-support metadata, `refcursor` type identity, and cross-call session-portal results used by the focused compiler and engine tests.

## Protocol client matrix

`clients/run.sh` builds pinned psycopg, pgx, and node-postgres images, provisions a password-authenticated role in a running PostgreSQL 18.4 container, checks the deterministic operation/version evidence from each driver against `clients/expected.json`, and reruns the same operations against the server fixture assembled from `uqa-pg-wire`. The matrix covers prepared reuse, binary formats, COPY in and out, failed-transaction rollback recovery, and one-connection pool reuse; it also runs the existing PostgreSQL 18 psql/libpq protocol 3.0/3.2 suite.

The default container name is `pg-parity`, the default published PostgreSQL port is `15432`, and the runner uses the Docker runtime's host-gateway alias. Override `UQA_PG18_WIRE_CONTAINER`, `UQA_PG18_ORACLE_PORT`, or `UQA_PG18_DOCKER_HOST` for another local runtime, then run:

```sh
bash tests/parity/pg18/clients/run.sh
```
