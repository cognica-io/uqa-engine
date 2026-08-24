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
```

The runner executes PostgreSQL and UQA concurrently by default. `--backend postgres` and `--backend uqa` select one side for diagnosis. Canonical transcript updates require the PostgreSQL-only backend and use an atomic file replacement; regenerate only from the pinned PostgreSQL 18.4 + AGE oracle, then review the checked-in JSON diff:

```sh
python3 tests/parity/pg18/run_routines_stateful.py --backend postgres --update-expected
```

Every fixture case starts with `-- @case <name> <ok|rows|error>` and ends with `-- @end`; this explicit framing allows routine bodies to contain semicolons without making the runner guess SQL statement boundaries. The runner replaces `__UQA_STATEFUL_SCHEMA__` with an isolated generated schema name and rejects an expected transcript whose fixture SHA-256 or ordered case modes are stale.
