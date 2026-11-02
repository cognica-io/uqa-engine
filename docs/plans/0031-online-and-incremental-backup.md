# Online and incremental backup implementation plan

Status: implementation in progress under [#1322](https://github.com/cognica-io/uqa-engine/issues/1322). The [design](../design/online-and-incremental-backup.md) was established against UQA Engine 0.6.3 (`1b5c22971d75b8db0e6900c8aa7b7e5613962bcb`) and merged in [#1320](https://github.com/cognica-io/uqa-engine/pull/1320). Implementation depends on current storage/snapshot/restore owners, not on a replication or CDC implementation, journal, or API.

## Current scope and progress

- [x] Audit current snapshot, autonomous metadata, storage-provider, memory Engine, restore, encryption, feature, and dependency boundaries.
- [x] Define online full backup, content-based incremental comparison, complete independent inventory roots, verification, retention, and fresh-database restore.
- [x] Separate full-backup delivery from incremental comparison and optional fingerprint acceleration, with owner-specific PR boundaries and acceptance.
- [x] Verify document links, paragraph/fence layout, independent scope, and repository dependency, capability, and integration-harness policies.
- [ ] Implement and qualify the delivery units below. [PR #1323](https://github.com/cognica-io/uqa-engine/pull/1323) implements Common Storage image identities, bounded inventory/root codecs and protected frames with owner tests and a [preservation argument](../design/backup-image-format.md); final-source platform qualification and delivery of this unit remain pending. [PR #1324](https://github.com/cognica-io/uqa-engine/pull/1324) implements local pack publication, verification and prepared-operation recovery in a dependent unit. Provider capture/import, fresh-target activation and public backup operations remain unimplemented.

The image contract unit passes 22 focused cases and all 1,009 Common Storage library cases, including admission under a writer's own frame limit when entries were constructed under a larger allowance. Strict Clippy for the library/tests, ownership and dependency policies, integration-harness and source-size checks, and 36 regression-runner verifier cases pass. Two exact work/memory/output gates are registered for automatic Linux/macOS qualification. The preceding Linux execution exposed cross-allowance snapshot retention; [PR #1325](https://github.com/cognica-io/uqa-engine/pull/1325) supplies the independently tested owner correction and is now this unit's prerequisite. Final-source platform execution and merge remain unchecked; these local codec results do not qualify a provider image.

The current scope is embedded Rust and a local repository, with same-provider restore for memory, native SQLite, SQLite KV, and redb. Source operations continue during backup export. Completed archives restore without a running source. Cross-provider conversion, network storage, arbitrary-time recovery, in-place replacement, scheduling, and language-binding expansion remain separate work. No release date or version is assigned.

## Delivery order and PR boundaries

Deliver usable online full backup and fresh-target restore before requiring incremental support. Incremental backup adds an inventory comparison over the same image/repository format; source fingerprint metadata can subsequently reduce payload reads. It does not require a persistent operation log or source history retained between completed backups.

| Unit | Owning crates | Dependencies | Completion evidence |
| --- | --- | --- | --- |
| Image and repository contracts | Common Storage | Existing owner/dependency audit | Versioned unit/cut/root types, bounded inventory format, explicit provider capabilities, strict parsers, format fixtures, and lifecycle errors |
| Complete backup metadata snapshots | Common Storage and each provider's metadata owners | Image contract | One cut covers records, namespaces, sequences, watermarks, definitions and artifacts; large metadata is captured without an unbounded writer-gate copy |
| Native SQLite online image export/import | SQLite, existing Storage image owners | Image and metadata snapshot contracts | Actual native mapping, stable logical read windows, fresh import, restored identity, and all four SQLite file modes; split export/import if needed |
| SQLite KV online image export/import | SQLite KV and Common Storage | Image and metadata snapshot contracts | Independent KV coverage, complete metadata and artifact inventory, fresh import and all four file modes |
| redb online image export/import | redb and Common Storage | Image and metadata snapshot contracts | Shared live owner, bounded reads, data/metadata consistency, fresh import and exact restore history; no independent opener workaround |
| Memory online image export/import | Memory Storage, Graph, Analysis/ML, Execution owners; Engine adapters | Image contract and committed memory capture | Actual `Engine::new()` coverage, joint immutable generations, catalog/data/resource codecs and fresh memory import; split by owning data model |
| Local repository full-backup publication | Common Storage | Image/root contract | Immutable packs, streaming inventory, protection, durable root publication, unknown-outcome resolution, and cleanup with injected failures |
| Restore orchestration and verification | Common Storage, provider import owners, Engine adapters | Provider import and repository format | Persisted external restore intent, incomplete-target admission, full integrity verification, existing `DatabaseRestore` integration, new identity, and retry preserving later writes |
| Embedded full-backup lifecycle | Engine and `uqa-api` adapters | Verified source/import/repository owners | Export authority, retained handles, cancellation, bounded progress, supported-provider capability reporting, and an executed local Rust backup/restore example |
| Full-backup qualification and documentation | Owning tests and Engine integration harness | Public full backup | Four Engine providers, eight SQLite mapping/file-mode combinations, concurrent writer proof by deterministic interleavings, crash/restore cases, and actual platform qualification |
| Incremental inventory merge | Common Storage | Qualified full-image/repository format | Complete child inventory, changed/new/deleted/tombstone units, immutable reference reuse, parent compatibility, branching, and fallback-read accounting |
| Snapshot-qualified fingerprints | Provider and memory image owners | Image digest contract | Exact-version hashes, atomic update or validated backfill, missing/stale/hash-mismatch behavior, and reduced unchanged-payload reads; split by provider |
| Repository retention and standalone copy | Common Storage | Completed roots and job leases | Parent-independent restore, concurrent publication/GC safety, bounded deletion of dead packs, source-independent copy, and key-rotation copy |
| Incremental qualification and documentation | Owning tests and Engine integration harness | Incremental lifecycle, retention and relevant fingerprint capability | Exact restoration after deletions/DDL/reopen, parent removal, corruption/failure tests, resource counters and controlled performance evidence |

Each row is a coherent work boundary, not permission to exceed 10,000 additions plus deletions. Split large rows by owner, provider, or lifecycle before review, and use actual prerequisite branches as dependent PR bases. Each implementation branch has a draft PR from its first push. Complete scope, focused local verification, review corrections, and this ledger before marking a PR ready. Checked planning items do not mark any runtime unit complete.

Full support for a provider is advertised only after its complete image/export/import contract passes. A format parser or reference store does not qualify the public Engine. Delivery may proceed provider by provider with explicit unsupported capabilities, but complete planned delivery still requires all four providers and all selected SQLite modes. Full-backup APIs do not wait for fingerprint optimization.

## Source inventory before implementation

| Area | Sources to inspect and extend |
| --- | --- |
| Record visibility and bounded access | [snapshot trait](../../crates/uqa-storage/src/mvcc/view.rs), [persistence](../../crates/uqa-storage/src/mvcc/persistence.rs), [SQLite reads](../../crates/uqa-storage-sqlite/src/mvcc/read.rs), and [redb reads](../../crates/uqa-storage-redb/src/mvcc/read.rs) |
| Autonomous state | [sequence durability](../design/sequence-publication-durability.md), identifier watermark methods, and [sequence obligation tests](../../crates/uqa-storage/tests/cases/mvcc_sessions/sequence_durability.rs) |
| Memory and sessions | [actual construction](../../crates/uqa-engine/src/open/lifecycle.rs), [transaction publication](../../crates/uqa-engine/src/transactions/publication.rs), and [retained storage snapshots](../../crates/uqa-storage/src/read_only_snapshot.rs) |
| Native/KV/redb history restore | [restore request](../../crates/uqa-storage/src/mvcc/restore.rs), [SQLite restore](../../crates/uqa-storage-sqlite/src/mvcc/restore.rs), [redb restore](../../crates/uqa-storage-redb/src/mvcc/restore.rs), and their existing tests |
| Query-observable state | [storage contract](../manual/internals/03-storage.md), [DiskANN restore acceptance](../design/diskann-backup-restoration.md), and [shared restore fixture](../../crates/uqa-storage/src/key_value/conformance/origins/binding/restore.rs) |
| Physical mode and locking | [SQLite connection modes](../../crates/uqa-storage-sqlite/src/connection.rs), provider lease/reclamation owners, and actual memory/redb sharing rules |
| Feature/layering policy | Affected manifests, [workspace policy](../../scripts/workspace-dependency-policy.json), and [Engine capability policy](../../scripts/engine-capability-policy.json) |

Before coding an image adapter, inventory every required state plane, its capture boundary, its owner, unit addressing, streaming encoder, import method, and tests. Include metadata outside `CommitSequence`, native mapping/data namespaces, graph state, model/resources, postings/statistics, evaluated index keys, vector generations and uncovered changes. Classify transient authority separately. Unknown state must cause capability rejection instead of an archive silently missing it.

Recheck the current manifests and enabled features at implementation time. No external source/fork/patch change or private SQLite/redb API is part of this plan. Existing public dependency APIs and UQA-owned persistence interfaces are the implementation boundary.

## Acceptance matrix

| Contract | Required evidence | Primary owner |
| --- | --- | --- |
| Consistent online capture | Committed/private separation, transaction crossing cut, failed statements/savepoints, concurrent DDL and DROP/TRUNCATE, autonomous allocation/sequence races, and writes before/during/after export | Snapshot and transaction owners; Engine public integration |
| Writer progress | Deterministic test barriers prove other sessions commit between export batches; compressed/rollback-journal modes do not retain a whole-export read transaction; slow repository never holds source writer admission | Provider and source lifecycle owners |
| Complete state | Catalog identities/security, ordinary and unlogged contents, no temporary/session state, watermarks, graph membership/properties, FTS scores, expression keys, model resources, and exact selected vector artifacts | Existing data-model/image owners |
| Full archive | Empty/large databases, large metadata collections/values, bounded inventory, immutable pack installation, duplicate/malformed IDs, missing codecs and required resources | Common Storage and provider codecs |
| Incremental comparison | Inserts, repeated/no-op updates, deletes after source tombstone reclamation, delete/recreate, TRUNCATE, namespace moves, parent-only/current-only units, missing/stale digests and exact hash reuse | Common Storage plus image adapters |
| Lineage and roots | Incompatible source/provider/mapping, source restore, old format, branched parents, deleting every ancestor manifest, and restoring the child from its complete object graph | Repository and provider format owners |
| Publication failures | Disk full, permission/I/O failure, cancellation, partial pack/inventory/root writes, synchronization/rename failure, process loss, and lost completion replies | Repository state machine |
| Restore failures | Wrong key, truncated/corrupt/missing objects, unsupported image, malicious paths/lengths, interrupted import/migration, SQLite intent boundaries, redb commit uncertainty, and retry after subsequent target writes | Repository parser and provider restore owners |
| Retention | Parent being read while removed, create/publish concurrent with collection, restore/verify/copy leases, abandoned jobs, orphan objects, partially live packs and bounded deletion | Repository ownership/GC |
| Security | Export/import authority, protected metadata, no plaintext spill for protected exports, nonce separation, tamper/reorder rejection, key rotation with old references and absent external bindings | Storage security and Engine authority adapters |
| Resource bounds | Small fixed allowances over large state/units, long snapshot/history pressure, source cancellation/drop, partial cleanup, backpressure, bounded hashing/compression and no all-database collection | Each allocating/retained-resource owner |
| Recovery and continued use | Persistent source restart, full source loss after artifact completion, fresh target reopen, independent old/new database identities, new ID allocation, later writes/rebuilds and held queries | Providers and Engine integration |

Run through actual memory, native SQLite, SQLite KV, and redb engines. For each SQLite mapping, test plain, SQLCipher encrypted, compressed, and compressed-encrypted source files, plus every advertised destination protection conversion. Same-provider restore is the acceptance matrix; cross-provider import is not implied by a shared archive envelope.

Persistent tests reopen real files and inject process/commit failures at authoritative boundaries. Memory tests import into a new actual memory Engine and verify that an unfinished source snapshot cannot survive source loss. redb concurrency uses its supported existing owner; native/KV SQLite also exercise separate permitted openers. Tests belong to the owning crate's existing single executable and its modules.

Use independent PostgreSQL 18 fixtures for SQL effects touched by capture/import integration, including sequences, unlogged persistence, and transactional DDL. UQA image bytes, approximate-index state, and repository semantics have their own owner assertions. Record the algebraic preservation argument required by [AI_POLICY.md](../../AI_POLICY.md#prove-feature-additions) with product delivery; finite hash/query fixtures do not replace it.

## Performance and completion gates

Measure full capture, unchanged incremental, sparse updates, heavy churn/deletions, large values, DDL, FTS/vector updates, slow repository output, and restore. Report source metadata visited, source payload bytes read, bytes hashed, new/reused/stored bytes, pack retention amplification, snapshot history, memory/spill peaks, read-window work, and source commit progress. Incremental output reduction and source-read reduction are separate assertions. An unchanged dataset using certified fingerprints must not fetch every payload; a missing-fingerprint fixture must report its full reads truthfully.

Complete incremental qualification covers both paths on each built-in provider: exact snapshot-qualified fingerprints for unchanged-payload reuse, and correct bounded fallback when a fingerprint is absent. Delivering full backup does not wait for that optimization; advertising reduced source reads does require its owner implementation and measured counters.

Add exact work/memory/output gates in the existing automatic regression inventory. For product deliveries, inspect actual final-source Linux and macOS results, including bounded work, memory, and correctness, plus affected FDW configurations. Timing claims require a controlled host with an independently established noise bound. Do not reduce application input, retry uncontrolled runs until they pass, or infer qualification from a build/archive job.

Run the relevant dependency, Engine capability, integration-harness, formatting, and owner checks before committing. Draft implementation PRs collect the complete correction batch before the ready-for-review transition starts affected CI. A discovered product correction returns the PR to draft. Repository events trigger CI; no manual dispatch, rerun, or cancellation is part of this plan.

- [ ] Deliver and qualify full online capture plus fresh-target restore for each advertised provider.
- [ ] Deliver incremental comparison with complete independent roots and explicit read-cost accounting.
- [ ] Verify fingerprint capabilities, retention/GC, standalone copy, protection and all failure boundaries.
- [ ] Execute a public Rust local backup/verify/restore example with the actual built Engine API.
- [ ] Update the manual, HISTORY, discovery map, and examples for verified released behavior only.
- [ ] Record final-source qualification and delivery in each implementation PR and this ledger, then remove merged branches.

The original design PR delivered documentation only. The image/repository codec unit is now in implementation and qualification; no codec result is evidence of a complete source capture or restore. This plan retains every product unit above independently of other features.
