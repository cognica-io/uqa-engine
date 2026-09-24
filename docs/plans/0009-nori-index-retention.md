# Nori indexing and retention corrections

The active correction covers #141 and #142. PostgreSQL assignment-target work (#148) is paused separately. This work must fix the storage owners and restore the existing allocation/output gates without another merge exception, timing retries, or committed machine reports.

## Ownership and completion requirements

- `uqa-storage::mvcc::serializable::liveness` owns local participant and receipt lease lifetime. Releasing the final handle must return the empty registry allocation without a subsequent transaction or explicit reclamation. Failed admission must release all reservations without taking SSI/provider admission during destruction.
- `uqa-storage::inverted_index` owns in-memory corpus representation, mutation staging and retained-reader accounting. Remove redundant retained structures while preserving ordered postings, graph identity, source metadata, atomic batches, snapshots, cancellation and exact quota admission. Existing native allocation ceilings remain unchanged.
- The Nori benchmark targets and Python verifiers must support allocation/output checks without timing samples. CI must run analysis, indexing, phrase and both persistent-provider checks independently so one failure does not hide subsequent results.
- Acceptance requires existing output fingerprints, commit/rollback and reopen results, final-reader cleanup and the existing allocation ceilings. Runtime evidence uses Linux Docker. Generated diagnostics remain under ignored output directories or CI artifacts.

## Progress

- #142: reproduced the exact 48-byte leak with a minimal owner test. Local lease destruction now reclaims its registry entry and unused capacity, and participant construction precedes the list lock so failed publication can release safely. All 12 liveness tests and the redb commit/rollback, final-reader and reopen regression pass in Linux Docker. The complete persistent Nori allocation/output gate is still pending.
- #141: reviewing redundant reverse-term nodes and cached position projections in the memory index. Implementation and allocation/output acceptance remain pending.
- Independent native gate execution and complete CI acceptance remain pending. Neither issue is closed yet.
