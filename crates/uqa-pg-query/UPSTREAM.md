# uqa-pg-query upstream pin

This crate is an imported snapshot of the UQA Engine PostgreSQL 18 parser chain.

- Wrapper: `https://github.com/jaepil/pg_query.rs` at `3617cb41f14f8a0ec2c5f6c6448226981bc1f9eb`
- C library: `https://github.com/jaepil/libpg_query` at `19b07f4bf8c9e44839025692888ac5207ea9a8ad`

The maintained PostgreSQL 18.4 pin corrects raw-tree, PL/pgSQL, list-comparator and generated Protobuf descriptor callback types without changing the parser version or schema. Allocator caches and top contexts have native thread-exit ownership through POSIX pthread keys or Windows FLS callbacks; explicit exit drains caches after context deletion and permits reinitialization. The owning changes are tracked in [libpg_query PR #4](https://github.com/jaepil/libpg_query/pull/4) and [pg_query.rs PR #7](https://github.com/jaepil/pg_query.rs/pull/7). Zero-field Protobuf messages unpack without NULL-pointer arithmetic. Public typed Protobuf initializers, the runtime version and Rust APIs remain unchanged.

The wrapper is MIT. `libpg_query` is BSD-3-Clause and includes PostgreSQL server source under the PostgreSQL license. See `LICENSE` and `LIBPG_QUERY-LICENSE`.

The package name is `uqa-pg-query` because crates.io already has `pg_query`. The library name remains `pg_query` so UQA Engine compiler code keeps `use pg_query::...`.

Do not edit imported sources to change parser behavior. Review and test parser updates in the two upstream repositories first, then run:

```sh
python3 scripts/sync-uqa-pg-query.py --source PATH_TO_CHECKED_OUT_WRAPPER
```

The sync script removes the obsolete Clippy compiler-plugin attributes from `build.rs` and normalizes imported Rust sources with the workspace-pinned rustfmt before writing `SHA256SUMS`; the checksums therefore describe this deterministic build adaptation and post-rustfmt snapshot rather than the upstream files byte for byte. Clippy runs through the workspace's toolchain command and is not a crate dependency. No parser behavior is changed by these adaptations. CI verifies both the checksums and workspace-wide rustfmt. A parser update must change the recorded revisions in this file, the script constants, and the checksum list in the same change.
