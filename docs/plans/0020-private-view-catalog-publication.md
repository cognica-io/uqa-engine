# Private view catalog publication

Issue [#899](https://github.com/cognica-io/uqa-engine/issues/899) reports a missing materialized-view owner after a role rename wait. The original Linux log does not distinguish the live-session assertion from the reopened database. Successful repetitions do not close the issue.

## Scope and ownership

Preserve private regular and materialized view definitions when a fixed-isolation transaction observes a peer catalog commit. Storage owns exact private-record provenance, including deleted keys and both ends of a rename. Execution combines those records with the fresh committed catalog and preserves existing ACL/incarnation rules. Engine retains session snapshots and exposes those owners; no catalog algorithm moves into Engine. No storage format, dependency, Cargo feature or SQL syntax changes are required.

## Task record

- [x] Inspect the CI failure, crate features, dependency policy and ownership boundaries.
- [x] Reproduce the missing private view with a deterministic peer-publication/catalog-lookup schedule: all twelve regular/materialized, fixed-isolation and persistent-provider combinations fail before the correction.
- [ ] Add exact view-record provenance to native SQLite and shared Key/Value catalogs.
- [ ] Preserve private view creation, replacement, rename, deletion and materialized rows during catalog refresh; retain committed peer changes and ACL behavior.
- [ ] Verify owner incarnation, commit/reopen, rollback and savepoint behavior, with independent PostgreSQL 18 expectations.
- [ ] Commit and push the implementation; review it and verify the actual Linux/macOS automatic regression results for that source revision.
- [ ] Merge, close #899, remove the owned branch and return the isolated checkout to main.

## Evidence boundary

The deterministic schedule calls the existing catalog synchronization adapter after the peer commits and before another SQL command can refresh the private read view. This represents publication between a command's snapshot acquisition and a nested catalog lookup. The defect is the absent private view, not merely a join choosing the wrong role name. Generated logs stay outside the repository. The separate controlled EC2 qualification of merged PR #897 remains tracked in that PR and must finish with the reusable instance stopped.
