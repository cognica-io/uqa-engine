# HNSW bounded storage

Status: active. The implementation starts from main `0631ceee6`. HNSW currently materializes canonical vectors, decoded graph nodes, mutation copies and persistence deltas in memory. Native SQLite, SQLite Key/Value and redb reject graphs exceeding the shared session retention allowance; memory indexes encounter the same materialization in controlled captures. The allowance is independent of SQL `work_mem` and defaults to 64 MiB.

## Required result

An HNSW graph larger than its memory allowance must remain constructible, readable, mutable and reopenable with bounded resident graph storage. Preserve the existing deterministic graph, raw vector bits, cosine scores, document/tensor identities, deletion and compaction rules, fixed snapshots, transaction rollback, cancellation and atomic publication. A genuinely indivisible record, requested result or minimum operation workspace that exceeds its allowance may still fail; graph cardinality alone must not require resident corpus copies. No increase or bypass of the existing allowance qualifies as a fix.

## Ownership and implementation order

| Unit | Owner and change | State | Exit evidence |
| --- | --- | --- | --- |
| Shared graph storage | `uqa-storage`: bounded ordered maps, encrypted temporary pages, immutable roots, shared HNSW traversal and mutation over resident or spilled nodes; stream persistence deltas | In progress | Exact graph and score equivalence across forced spill, forked roots, mutation, compaction, failed I/O, cancellation and final resource release |
| Provider streaming | `uqa-storage`, `uqa-storage-sqlite`: stream canonical vectors, nodes and edges through common construction/restoration; include native SQLite, standalone SQLite, SQLite Key/Value, redb and commit-time HNSW merging | Pending | Corpus larger than allowance can build, query, mutate and reopen on each durable provider; corruption checks remain enforced |
| Engine acceptance and documentation | Engine retains resource controls and transaction adapters only; update public storage contract and history | Pending | Public SQL and memory-provider regressions, savepoint/transaction undo, retained readers, focused policy checks, automatic CI and merged PRs |

Keep one reviewable PR open for this work at a time. Commit shared primitives, graph integration and provider integration separately. Do not report the first unit as the complete fix. Rebase on concurrent changes to MVCC private-overlay spilling rather than duplicating that implementation. The final acceptance includes the actual publication path, not just an isolated graph builder.

## Representation and preservation argument

The ordered graph representation stores node identities, active `(document, ordinal)` identities and pending persistence changes. Resident roots share immutable paths under a child of the original allowance. On reaching the resident allowance, an ordered map transfers to an authenticated temporary file. Disk roots use immutable path copies, so publishing one root never changes a previously retained root. Reads materialize at most one record per handle, and graph consumers retain only their explicitly charged operation workspace. Existing `TemporaryFile` owns encryption, random keys and final-handle cleanup; no provider dependency is added to common Storage.

Let $M$ be a finite ordered map and $R$ its resident or disk representation. Define $\alpha(R)$ by traversing live keys and decoding their values. The representation invariant is $\alpha(R)=M$, with identical keys and bitwise identical vector coordinates. Lookup and ordered successor return the same value and least greater key under either representation. An insertion copies only the search path and publishes the resulting root after every required write succeeds; therefore its abstraction is $M[k\mapsto v]$, while every old root still abstracts to its original map. Deletion removes only the selected binding. A failed publication leaves the original root authoritative. Transferring keys in order to a new file preserves each binding and therefore preserves $\alpha$.

The HNSW algorithm's state is its metadata plus these maps. For any fixed input sequence, each lookup, ordered iteration and update observes the same abstract maps in both representations. Induction over the existing algorithm's steps therefore preserves chosen levels, candidate ordering and tie breaks, reciprocal edges, active identities and metadata. Raw floating-point values use their IEEE-754 bits in the temporary codec, so normalization and final canonical cosine scoring receive identical inputs. Consequently the exposed document support and decorated ranked results, including tensor deduplication and stable ties, are unchanged; replacing storage does not introduce a new algebraic operator or authorize an optimizer rewrite.

Provider adapters must supply one fixed canonical/graph view, preserve validation of complete tensor ordinals and canonical bit equality, and stage the resulting metadata and node changes in the existing transaction. Streaming does not permit partial catalog publication. Cancellation and I/O failures retain their existing failure/rollback boundary. Tests exercise these proof obligations but do not replace the argument.

## Independent reference

PostgreSQL 18 has no built-in HNSW access method. The relevant extension reference is [pgvector HNSW construction](https://github.com/pgvector/pgvector/blob/v0.8.2/src/hnswbuild.c#L490-L513), which flushes its graph and continues disk-based construction when `maintenance_work_mem` is exhausted. This establishes the expected continuation behavior, not identical graph topology between different HNSW implementations. UQA graph and score preservation is checked against the existing deterministic resident algorithm; the provider acceptance must additionally prove bounded memory and actual disk use.
