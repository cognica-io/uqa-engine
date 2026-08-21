# uqa-pg-query upstream pin

This crate is an imported snapshot of the UQA Engine PostgreSQL 18 parser chain.

- Wrapper: `https://github.com/jaepil/pg_query.rs` at `516b3a03fed42e606ce01bc8b5a864a1698c210d`
- C library: `https://github.com/jaepil/libpg_query` at `898cd71c96375d6d4219916996701571dbe2b239`

The wrapper is MIT. `libpg_query` is BSD-3-Clause and includes PostgreSQL server source under the PostgreSQL license. See `LICENSE` and `LIBPG_QUERY-LICENSE`.

The package name is `uqa-pg-query` because crates.io already has `pg_query`. The library name remains `pg_query` so UQA Engine compiler code keeps `use pg_query::...`.

Do not edit imported sources to change parser behavior. Review and test parser updates in the two upstream repositories first, then run:

```sh
python3 scripts/sync-uqa-pg-query.py --source PATH_TO_CHECKED_OUT_WRAPPER
```

CI runs `python3 scripts/sync-uqa-pg-query.py --check` against `SHA256SUMS`. A parser update must change the recorded revisions in this file, the script constants, and the checksum list in the same change.
