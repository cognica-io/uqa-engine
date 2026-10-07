# Batched key reservations

Execution computes all changed UNIQUE and PRIMARY KEY reservation identities before acquiring them, sorts and deduplicates them, and refreshes the mutation snapshot after their acquisition. No expression, callback or data read occurs between these reservations. `uqa-execution::row_locks` owns arbitration; Engine adapts session identity, cancellation and the current savepoint mark. No crate dependency or persistent format changes.

## Preservation argument

Let $K=(k_1,\ldots,k_n)$ be the already evaluated reservation sequence, $G$ the existing grants, and $C(k,G)$ the existing PostgreSQL tuple-lock conflict predicate. For a group of at most 64 keys, provisional in-process grants are hidden by the same local mutex. If every request is compatible locally and in the shared table, publication holds the shared table mutex and produces exactly the same grants, acquisition identities and savepoint marks as the original ordered acquisitions without an intervening observer. The original sequential execution admits that schedule, so the batch introduces no new visible schedule or weaker exclusion.

If any local or foreign claim conflicts, no provisional shared claim is published and all new local acquisitions from the attempt are removed in reverse order. Existing grants and their earlier marks remain unchanged. Each original request then runs through the existing acquisition method in order, preserving prefix retention, NOWAIT and SKIP LOCKED behavior, cancellation, wait attribution and deadlock detection. An infrastructure failure propagates without leaving a successful provisional acquisition attributed to the caller.

Constraint lookup follows an unconditional snapshot refresh after all reservations, including acquisitions that observed no wait. A competing commit between the caller's original snapshot and its reservation therefore remains visible. Batching is restricted to this pre-evaluated boundary; it does not move reservation work across source-row evaluation, triggers, user callbacks or document-identity occupancy checks. The ordinary single-key path is unchanged.

The transaction state transition at the constraint observation boundary is consequently the same as an allowed sequential transition. Documents, value relations, payloads, scores and ranked results receive the same inputs; no UQA algebraic operator, carrier or composition is changed. This is a scheduling refinement, not a new operator law.

## Verification and limits

Execution tests count actual shared table-lock acquisitions for 2, 64, 65 and 128 fresh reservations: exactly $\lceil n/64\rceil$, with no additional arbitration for unchanged grants. They check every local grant, savepoint release, duplicate/upgrade ownership, cancellation, ordered blocking and both NOWAIT and SKIP LOCKED against a separate process. Engine tests cover several simultaneous unique keys, conflict diagnostics, ON CONFLICT and rollback on memory, native SQLite, SQLite Key/Value and redb; existing independently checked PostgreSQL unique-key cases remain unchanged.

This reduces repeated arbitration within eligible groups. It does not establish a latency speedup, partition the shared mutex, replace hashed row identities or fence older coordination protocols. Those remaining obligations stay tracked in [#266](https://github.com/cognica-io/uqa-engine/issues/266).
