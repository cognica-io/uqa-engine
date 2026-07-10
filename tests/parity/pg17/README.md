# PG17 differential probes

`run_diff.py` executes every probe in `probes.sql` against a real
PostgreSQL 17 instance and against the `usql` release binary, then
reports divergences in three categories:

- `engine-error`: PostgreSQL answers, the engine rejects (missing
  feature).
- `engine-accepts`: PostgreSQL rejects, the engine answers (missing
  guard, e.g. division by zero).
- `value-mismatch`: both answer, values differ after normalization
  (booleans, float formatting, jsonb spacing are normalized away).

## Prerequisites

- A PostgreSQL 17 container named `uqa-pg17-age` with user `postgres`,
  database `uqa_compat`:

  ```sh
  docker run -d --name uqa-pg17-age \
    -e POSTGRES_PASSWORD=uqa -e POSTGRES_DB=uqa_compat \
    -p 15432:5432 apache/age:release_PG17_1.6.0
  ```

  (The Apache AGE image doubles as the AGE 1.6.0 ground truth.)

- A release build of the CLI: `cargo build --release -p uqa-cli`.

## Run

```sh
python3 tests/parity/pg17/run_diff.py
```

The summary line reports `total/match/diff`. Error-vs-error rows count
as matches (message text is not compared). Update `probes.sql` freely:
one probe per line, `--` comments skipped; probes must be
side-effect-free single statements.
