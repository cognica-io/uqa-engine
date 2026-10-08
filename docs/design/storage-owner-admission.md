# Storage owners and coordination upgrades

The shared row table cannot observe native row locks held by a pre-table binary. SQLite already retains a physical owner lease for every pool and retained resource. Record initialization must use that existing admission boundary to exclude an incompatible live owner before importing or upgrading its records. Execution continues to own row claims; SQLite owns physical owner admission. No dependency direction, SQL operator, user data format or synchronization level changes.

A current SQLite owner records coordination protocol 2 in its existing owner lease; predecessor owners record zero. Before raw, native or Key/Value record initialization, the initializer holds owner admission and requires every live lease to carry protocol 2. It retains admission through the complete initialization transaction. Initial native catalog restoration retains the same guard until commit or rollback. New owner attachment cannot interleave with that interval. Redb already excludes a second file owner through its native database lock and needs no SQLite admission code.

Let A be the existing owner-admission mutex and L the live owner records. Under A, initialization checks that every member of L declares the current protocol. An incompatible owner already present causes an error before any initialization write. An owner arriving later cannot enter until publication or rollback releases A. After a successful current-format publication, a pre-table binary rejects the newer durable format before it can execute record operations. A failed initialization preserves its predecessor state and releases admission; retry requires incompatible owners to leave. The shared row table's separate format check continues to exclude incompatible table-aware coordinators.

Owner lifetime remains the lifetime of its pool and retained resources, not a statement or logical transaction. Dropping an admission guard never drops an owner lease; dropping a lease does not need admission. Process death releases native liveness, so a dead predecessor cannot prevent reopening. Memory and cancellation checks remain in the owning lease transport. In-process fallback uses its existing owner registry and introduces no cross-process claim.

Binaries predating physical owner leases do not participate in this fence. Upgrading those legacy versions still requires closing every owner. Direct raw SQLite manipulation is outside the versioned storage contract. This change does not claim controlled throughput qualification for the row table's mutex.

- [x] Inspect SQLite/common Storage manifests, ownership policy, record initialization, initial native restoration and lease transport.
- [x] Implement protocol-tagged owners and retained initialization admission.
- [x] Verify live predecessor rejection across 12 storage/mapping combinations, unchanged state on rejection, separate-process death, concurrent current owners, cancellation, memory accounting and commit/rollback admission probes.
- [x] Pass all 61 connection cases and 10 restoration failure/process cases; the ignored predecessor-peer entry point executes through its parent tests. Strict SQLite library/test Clippy, rustfmt, dependency/Engine ownership, harness, file-size/header checks and 19 regression-runner tests pass.
- [x] Update HISTORY and upgrade guidance; automatic inventory includes the three new admission cases (32 checks / 88 required cases). Commit and push logical units.
- [ ] Complete source/CI review, merge and remove the branch; preserve the remaining performance ledger.

The remaining work continues under #266, #347, #348, #261 and #125. This unit advances #266's mixed-version safety condition; it does not close unrelated performance acceptance requirements.

The last pre-table source inspected (`5a7e0c61f` parent) uses SQLite record format 55, while current source publishes format 59. The predecessor header validator rejects that newer marker. Physical owner leases and their admission byte already existed before the shared row table; the fence preserves their namespace, slot layout and liveness addresses and changes only the tag written by current owners. No timing acceptance is inferred from these correctness checks.
