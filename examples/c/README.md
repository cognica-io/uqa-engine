# C examples

These five programs exercise the embedded C source API with the same fixtures and expected results as the other [public binding examples](../README.md). Build the source library first; native distribution packaging is tracked in the [C implementation ledger](../../docs/plans/0028-c-bindings.md).

```sh
cargo build -p uqa-ffi --no-default-features
cc -std=c11 -Wall -Wextra -Werror -DUQA_USE_SHARED \
  -Icrates/uqa-ffi/include examples/c/unified_search.c \
  -Ltarget/debug -luqa_ffi -lm -pthread \
  -Wl,-rpath,"$PWD/target/debug" -o /tmp/uqa-unified-search
/tmp/uqa-unified-search
```

The command above works with the native Linux and macOS shared artifact. Use `c++ -x c++ -std=c++17` instead of `cc -std=c11` to compile the same sources as C++17. Keep the library loaded until all owned objects and callbacks have been released.

| Program | Behavior |
| --- | --- |
| [unified_search.c](unified_search.c) | Relational filters, lexical and Bayesian retrieval, vectors, evidence pooling, operator joins, graph traversal, and a scalar recency callback |
| [vector_knn.c](vector_knn.c) | Typed vector parameters, exact/HNSW/IVF/DiskANN search, canonical scores, rollback, committed replacement, and persistent reopen |
| [graph_cypher.c](graph_cypher.c) | Graph creation, fixed traversals, MERGE, SET, typed relational joins, and nested traversal predicates |
| [storage_transactions.c](storage_transactions.c) | Persistent reopen, rollback, savepoints, parameter binding, and independent sibling sessions |
| [extensibility.c](extensibility.c) | Immutable scalar, streaming table, and aggregate functions with explicit state and registration cleanup |

Four programs default to memory. To use persistent storage, supply `sqlite-native`, `sqlite-kv`, or `redb`, followed by a new database path. The storage example requires these arguments. For example, `./storage_transactions sqlite-kv /tmp/accounts.uqa` exercises SQLite Key/Value. Use a fresh path for each run: examples create their schema and do not delete an existing database.

All calls check `uqa_status`; diagnostics are borrowed and require no destruction. The small [example.h](example.h) helper exits on a failed example check, and keeps assertions active in release builds. Successful paths explicitly release values, results, registrations, sessions and Engines. Callback implementations propagate failures through the ABI and release their own allocations without throwing or jumping across the boundary.

Run both C11 and C++17 consumers on every applicable storage provider with the actual built shared library:

```sh
# macOS; use libuqa_ffi.so on Linux.
python3 scripts/verify-c-examples.py --library target/debug/libuqa_ffi.dylib
```

The runner executes 38 scenarios: both languages on Memory, native SQLite, SQLite Key/Value and redb, with the persistence-only example on the three durable providers. It prints the tested library's SHA-256. `--sanitize` instruments the consumer programs with AddressSanitizer and UndefinedBehaviorSanitizer; this does not instrument Rust or establish full-library sanitizer coverage. `--static --link-args '...'` accepts an actual static archive and its platform dependencies.
