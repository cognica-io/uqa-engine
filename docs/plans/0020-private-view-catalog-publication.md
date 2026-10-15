# Private view catalog publication

Issue [#899](https://github.com/cognica-io/uqa-engine/issues/899) reports a missing materialized-view owner after a role rename wait. The original Linux log does not distinguish the live-session assertion from the reopened database. Successful repetitions do not close the issue.

## Scope and ownership

Preserve private regular and materialized view definitions when a fixed-isolation transaction observes a peer catalog commit. Storage owns exact private-record provenance, including deleted keys and both ends of a rename. Execution combines those records with the fresh committed catalog and preserves existing ACL/incarnation rules. Engine retains session snapshots and exposes those owners; no catalog algorithm moves into Engine. No storage format, dependency, Cargo feature or SQL syntax changes are required.

## Task record

- [x] Inspect the CI failure, crate features, dependency policy and ownership boundaries.
- [x] Reproduce the missing private view with a deterministic peer-publication/catalog-lookup schedule: all twelve regular/materialized, fixed-isolation and persistent-provider combinations fail before the correction.
- [x] Add exact view-record provenance to native SQLite and shared Key/Value catalogs.
- [x] Preserve private view creation, replacement, rename, deletion and materialized rows during catalog refresh; retain committed peer changes and ACL behavior.
- [x] Verify owner incarnation, commit/reopen, rollback and savepoint behavior, with independent PostgreSQL 18 expectations.
- [ ] Commit and push the implementation; review it and verify the actual Linux/macOS automatic regression results for that source revision.
- [ ] Merge, close #899, remove the owned branch and return the isolated checkout to main.

## Evidence boundary

The deterministic schedule replays the existing catalog-epoch publication step after the creator has already observed the role rename's physical commit and installed its private view, then calls the existing catalog synchronization adapter. COMMIT releases the owner's logical lock before publishing that epoch, so a waiter can encounter exactly this ordering. No additional durable commit can incidentally repair the cache. With the original Execution merge restored, all twelve cases fail the same ownership JOIN after COMMIT (zero rows instead of one); the private view is missing, not merely assigned to the wrong role. The original historical log still cannot identify which of its two observations failed. Generated logs stay outside the repository. The separate controlled EC2 qualification of merged PR #897 remains tracked in that PR and must finish with the reusable instance stopped.

The independent PostgreSQL 18.4 Docker capture is `tests/parity/pg18/capture_private_view_refresh.py`; its compact expected fixture records six two-session commit/rollback/savepoint schedules and is consumed by the Engine regression. Storage provenance reads exact physical view keys, including tombstones; Execution asks once per distinct relation and overlays only private definitions before applying the existing incarnation-aware ACL merge. The owning tests cover unchanged committed-map sharing and error propagation. These correctness checks do not establish a latency improvement.

The correction is reviewed in [PR #900](https://github.com/cognica-io/uqa-engine/pull/900). Local verification passes the 33 new cases and the 126 original owner-wait cases plus existing private column-ACL and delegated-view-owner regressions. Formatting, crate dependency/ownership, single-harness, source-size/header, legal-file and deterministic-inventory checks pass. The automatic inventory now requires 787 cases in 533 checks; platform artifacts remain pending.
