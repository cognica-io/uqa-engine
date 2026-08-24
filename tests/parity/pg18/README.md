# PG18 differential probes

`run_diff.py` validates `manifest.json`, executes every probe in `probes.sql` against a real PostgreSQL 18 instance and against the `usql` release binary, then reports divergences in three categories:

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
