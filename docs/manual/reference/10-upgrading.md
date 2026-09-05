# Upgrading to UQA Engine 0.2.0

Version 0.2.0 includes SQL object and privilege lifecycle changes, durable expression and unique indexes, expanded sequences and PL/pgSQL, native cross-process notifications, and a Node.js HTTP client that runs without native addons. The [release history](../../../HISTORY.md#020---2026-09-05) records the changes; the [compatibility guide](../sql/09-compatibility.md) defines the verified PostgreSQL 18 surface and the behavior still being implemented.

## Package versions

Update the UQA packages used by one application together. Rust's `0.1` dependency requirement does not select `0.2.0`; change the requirement explicitly and regenerate the application's lockfile.

| Environment | Versioned installation |
| --- | --- |
| Embedded Rust | `cargo add uqa@0.2.0` |
| Rust HTTP client | `cargo add uqa-client@0.2.0` |
| Python and `usql` | `python -m pip install --upgrade uqa==0.2.0` |
| Embedded Node.js | `npm install @cognica-io/uqa@0.2.0` |
| Node.js HTTP only | `npm install --omit=optional @cognica-io/uqa@0.2.0` |
| Browser WASM | `npm install @cognica-io/uqa-wasm@0.2.0` |

The Rust workspace requires Rust 1.90 or newer. Python requires Python 3.8 or newer, and the Node.js package requires Node.js 16 or newer. The Node.js root package selects an exact-version native optional package for embedded execution; deploy the root and native packages from the same release. Deploy the Browser WASM JavaScript module and `uqa.wasm` from the same package together, including when updating a browser cache.

The [GitHub release](https://github.com/cognica-io/uqa-engine/releases/tag/v0.2.0) contains the Python and npm archives, standalone Node.js addons, and the status of publication to crates.io, PyPI, and npm. Rust applications using Git dependencies should select `tag = "v0.2.0"` consistently for every UQA dependency.

## Custom Rust storage implementations

The storage traits changed in this minor release. Applications implementing `uqa_storage::DocumentStore` must implement `put_stored` and `get_stored` using `StoredDocument`. A record keeps its public field map separate from `DocumentMetadata`, including tuple `xmin`. Preserve metadata through scans, rewrites, snapshots, and persistence; storing it as a user field can collide with application data. The default `put` implementation replaces public fields while preserving existing metadata, while a new tuple version uses `put_stored` with explicit metadata.

The B-tree methods on `uqa_storage::PersistentStorageBackend` now use `ValueIndexKey::Column` and `ValueIndexKey::Index`. Preserve both namespaces even when the enclosed names are equal. Named expression indexes store composite `Value::Row` keys; SQL expression binding and evaluation belong to the engine. Update custom backend signatures and physical key encoding before compiling against 0.2.0. See [Storage internals](../internals/03-storage.md) and the trait definitions in [`document_store.rs`](../../../crates/uqa-storage/src/document_store.rs) and [`backend.rs`](../../../crates/uqa-storage/src/backend.rs).

## Persistent database migration

Opening an older supported database performs the required provider and catalog migrations. This release adds typed tuple metadata, richer object and column identities, ownership and ACL records, bound routine and rule dependencies, and expression-index metadata. Initial open owns migration writes; later catalog refresh validates the persisted representation. The shipped SQLite and key-value providers handle their storage migrations through the normal engine open path.

1. Stop writers, close every engine using the database, and create a recoverable backup through the [storage backup procedure](04-storage-and-security.md#backups-and-copies).
2. Open a copy with the exact 0.2.0 application and its selected provider, encryption key, and compression configuration.
3. Execute representative reads, writes, role and privilege checks, stored routines and views, and retrieval queries. Verify indexes, transaction rollback, and close-and-reopen behavior with the application's data.
4. Update every process sharing the database before reopening the original file. Register process-local runtime callbacks again when the application starts.
5. If the application must return to an older binary, restore the pre-upgrade backup. Do not rely on an older binary reading a file migrated by 0.2.0.

Keep migration failures visible and resolve them before admitting writes. Retain encryption keys and any external rollback anchor according to the [storage and security contract](04-storage-and-security.md).

## Node.js HTTP clients

`HttpEngine`, SQL parameter helpers, and streaming execute in JavaScript without a native addon. Existing imports from `@cognica-io/uqa` continue to expose these APIs; `@cognica-io/uqa/http` is the explicit HTTP entry point for both CommonJS and ESM. An HTTP-only deployment can omit optional dependencies. Embedded `Engine` use still requires the platform's native package.

Use an explicit URL and token or `HttpEngine.fromEnv()` when deployment configuration already supplies credentials. The asynchronous `local()` and `cloud()` constructors require the installed `uqa` CLI and resolve a project once. Keep using the documented parameter wrappers for vectors and tensors, JavaScript `bigint` for exact signed 64-bit integers, and `Buffer` or `Uint8Array` for binary values. The [HTTP Engine reference](09-http-engine.md) describes errors, response limits, streaming, and cancellation.
