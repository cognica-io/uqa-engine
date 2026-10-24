# UQA C bindings

This crate implements the embedded C ABI described in the [C binding design](../../docs/design/c-bindings.md). Implementation and qualification are in progress; the [implementation ledger](../../docs/plans/0028-c-bindings.md) records the complete scope and remaining work. The checked-in headers are not a released interface yet.

The Cargo library target is `uqa_ffi`, avoiding a collision with the existing Rust `uqa` facade. Planned native distribution packages will install the library as `uqa` and include the headers under `include/uqa`.

All public functions return `uqa_status`. Opaque handle typedefs include their pointers, strings use C strings or explicit-length variants, diagnostics are borrowed from the calling thread, and acquisition/release names stay paired. SQL execution accepts native multi-statement and empty messages.

Choose the backend with `uqa_engine_open_memory`, `uqa_engine_open` (native SQLite), `uqa_engine_open_sqlite_kv`, or `uqa_engine_open_redb`. Each returns an owned `uqa_engine` released with `uqa_engine_close`. Persistent Engines support independent sibling sessions through `uqa_engine_open_session`. Both SQLite backends accept the configured encryption and compression combinations; memory and redb reject those settings. `uqa_engine_open_auto` detects native SQLite encryption/compression, not the provider. See the [C source API reference](../../docs/manual/reference/08-bindings-and-extensions.md#c-source-api).

Run `cargo test -p uqa-ffi --no-default-features --test integration` for the current source ABI suite. On Unix, its single Rust harness compiles and executes the same seven native fixtures as C11 and C++17 clients against all four backends. Persistent cases also verify sibling isolation, savepoint recovery, retained subscriptions, close/reopen, rollback on close and SQLite encryption/compression. Static linkage, additional platforms, feature combinations and release qualification remain tracked in the implementation ledger.
