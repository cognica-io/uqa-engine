# Lifetime of key reservation identities

SQL key reservations previously inserted every distinct digest into the database manager's permanent relation maps. A deterministic owner fixture acquires and releases 256 different keys and finds all 256 identity entries still retained. The shared claim table already releases those rows; the leaked objects are the local names used to reach it. Issue #588 tracks the reproduction.

Execution owns a scoped key identity handle. An identity registry retains weak references for transient key entries and permanent entries for existing stable-ID callers and table identities. SQL adapters retain handles while constructing requests. Acquisition retains a handle across waiting, and a granted row retains it until its final local and cross-process release. Engine only forwards these resources and lock requests. No crate dependency, feature, SQL operator or persistent format changes.

For a digest d, let O(d) be its pending request, waiter, held-grant and native-release owners. While O(d) is nonempty, every such owner retains the same interned identifier i and its complete digest. The registry allocates i monotonically and never reuses it. Final handle destruction removes both interning directions only if the record still names i and is transient. Concurrent re-interning after the last strong owner disappears therefore allocates a different identifier, and an older destructor cannot remove it. A permanent-ID request promotes the existing entry before returning, preserving the prior stable-ID API.

Every native release retains the identity until the shared claim and its pin have been released. Savepoint undo that removes only an upgrade keeps the earlier grant's handle. Failed provisional batches discard their grants while pending requests still own their handles. Cancellation and failed waits retain identity through wait-edge cleanup. Full digest equality, existing conflict modes, acquisition marks and query-visible errors are unchanged. The local registry retains transient entries only for live owners, rather than for all keys encountered during the database lifetime.

- [x] Reproduce 256 retained entries after 256 completed key reservations and register #588.
- [x] Inspect Execution/Engine manifests, ownership policy, grant, wait, batch, release and Engine adapter paths.
- [x] Implement scoped identity ownership in Execution and retain it in Engine's two key-reservation adapters.
- [x] Verify reclamation, a 4,096-key batch and capacity release, permanent-ID promotion, duplicate/upgrade/savepoint ownership, cancellation, batch rollback, waiting ownership and 256 waves of concurrent reuse.
- [x] Pass focused Execution/Engine tests, rustfmt, strict Clippy and ownership/dependency checks.
- [ ] Update HISTORY and automatic regression inventory, push logical commits, review and merge; close #588 and clean up.

The broader performance ledger remains #266, #347, #348, #261 and #125. Controlled timing qualification is separate from the retained-entry and operation-count assertions in this change.

All 145 row-lock owner cases and 16 Engine composite-UNIQUE cases pass. The full owner run first exposed a crash-fixture failure (limit 11 versus expected 13); #590 adds peer-observed limits before death, retains the input handle through termination and requires abnormal exit. The expected reservation and restart values are unchanged, and the strengthened focused case plus the full owner suite pass. This is a test-boundary correction, not a claimed product allocator change. The automatic inventory now contains 36 checks / 92 required cases, including this recovery case.
