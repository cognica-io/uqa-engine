# storage-transactions

```sh
cargo run -p example-storage-transactions
```

Durable storage on `uqa-storage-redb`, the pure-Rust backend, and the transaction semantics layered on it.

- **Reopen.** Pass 1 writes and drops the engine; pass 2 reopens the same file and finds the rows. redb is a single file, so that is the whole durability story.
- **Rollback.** An explicit transaction that is rolled back leaves no trace.
- **Savepoints.** `SAVEPOINT` / `ROLLBACK TO SAVEPOINT` / `RELEASE` undo work after a marker while keeping work before it, with the enclosing transaction staying open and atomic throughout. redb cannot create native savepoints after a transaction has opened a table, so the backend keeps a transaction-local undo journal instead.
- **Session isolation.** A second session over the same provider sees committed writes; uncommitted state stays private.

redb is not an encrypted format. For encryption at rest use `Engine::open_encrypted`; see [`compressed-vfs-security.md`](../../../docs/design/compressed-vfs-security.md) and the [Key/Value storage design](../../../docs/design/kv-storage-backends.md).

The example writes to a process-unique path under the system temp directory and removes it on exit.
