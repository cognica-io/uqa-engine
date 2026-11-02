# Replication and change data capture implementation plan

Status: design and implementation planning only. The [design proposal](../design/replication-and-change-data-capture.md) defines the intended contract; no replication or CDC implementation is claimed for UQA Engine 0.6.3. The baseline is `1b5c22971d75b8db0e6900c8aa7b7e5613962bcb`. The scope is a transport-independent core with one source authority, independent CDC consumers, and read-only replicas. Networking, distributed failover, multi-writer operation, PostgreSQL replication protocol support, and language-binding expansion are not prerequisites for this core.

## Current delivery

- [x] Audit common commit preparation and receipts, native SQLite/KV and redb physical publication, the separate memory Engine path, sequence durability, relevant tests, manifests, and dependency policy.
- [x] Define operation-level CDC, portable replication effects, journal identity and durability, snapshot handoff, consumer retention, replica admission, and owner boundaries.
- [x] Divide implementation by owning behavior and explicit prerequisites, with acceptance criteria and a 10,000-changed-line ceiling for each PR.
- [ ] Deliver the implementation units below. None has been started by this design change; checked planning items are not runtime verification.

This ledger is the current implementation scope. New implementation PRs link their unit here, preserve its acceptance requirements, and record focused verification and actual final-source CI evidence before marking it complete. No release date or version is reserved by this plan.

## Implementation order

The dependency graph is contracts/codecs → transactional capture and physical publication → retention/export → snapshot/import and apply → public lifecycle → complete qualification. Source identity, nontransactional sequence durability, and memory publication are required integration work, not follow-up reliability enhancements. Product source activation remains unavailable until every admitted write path has coverage and every managed object has a compatible codec.

Each row below is a scope boundary for a PR, not permission to exceed the repository limit. Count additions plus deletions against the actual base before pushing and before review. If one row approaches 10,000 changed lines, split it further by codec, provider, or owning operation, with independently verifiable contracts and prerequisite branches as bases. Do not accumulate all rows on one implementation branch. New implementation branches have draft PRs from their first push, and completed branches are removed after delivery.

| Delivery unit | Owners | Prerequisites | Acceptance required before completion |
| --- | --- | --- | --- |
| Journal identities and envelope codec | Common Storage; Core only for existing shared primitives | This design | Bounded versioned framing, stable publication/event identities, digests, malformed-input rejection, format fixtures, and a conservative provider capability |
| Transactional event accumulator | Common Storage and Execution's command/savepoint owners | Envelope contract | Ordered accepted effects, atomic event/mutation admission, spill, rollback marks, sealed fingerprints, and receipt-safe retries; no public source activation |
| SQL value and catalog codecs | SQL and Execution | Envelope contract | Lossless value/schema descriptions, stable logical IDs, dependency closure, source images, compatible built-in identities, and encode/decode fixtures |
| SQL and document mutation capture | Execution and existing document mutation owners | Accumulator and SQL codecs | INSERT/UPDATE/DELETE, TRUNCATE, COPY, MERGE/upsert, trigger/cascade/deferred effects, catalog changes, and direct managed APIs produce the same committed semantics |
| Graph change and state codecs | Graph and existing Storage graph owners | Envelope and accumulator contracts | Graph identity, membership/property/topology changes, rollback, snapshot state, and no duplicate events from underlying physical writes |
| Search/vector artifact codecs | Existing Storage/index owners, with Analysis resource descriptors | Envelope contract | Evaluated expression keys, postings, IVF/HNSW/DiskANN artifacts, generations and resource references preserve query-observable state without callbacks; split by index family |
| Model and remaining catalog-resource codecs | ML, SQL, Execution, and existing resource owners | Envelope contract | Permanent owned resources, security metadata and external-binding requirements are classified, bounded, and capability-checked; split by owner if needed |
| Native SQLite journal publication | SQLite and Common Storage | Sealed envelope and accumulator contracts | One physical commit for records/materializations, journal, receipt and progress; outcome recovery, migration, encryption, capacity admission, and cross-process coordination |
| SQLite KV journal publication | SQLite KV and Common Storage | Native/shared SQLite persistence contract | Independent KV coverage, canonical batch/journal atomicity, migration, and failure/reopen tests; native SQLite passing is insufficient |
| redb journal publication | redb and Common Storage | Sealed envelope and accumulator contracts | Immediate-durability data/journal/progress commits, recovery, format fences and capacity admission through public redb APIs |
| Memory Engine publication | Memory data/catalog owners, Execution publication gates, Engine adapters | Accumulator and required owner codecs | Actual `Engine::new()` jointly publishes data/catalog/journal after fallible preparation; multi-session snapshots, rollback, apply and bounded retention; reference-store tests alone are insufficient |
| Autonomous state and export frontier | Sequence/identity owners, Common Storage, provider adapters | Physical publication and relevant owner codecs | Rollback-independent state, imported watermarks, durable sequence-prefix/export certificates, metadata-only barriers, and snapshot-cut coordination; split provider work as needed |
| Durable subscriptions and reclamation | Common Storage and providers; Engine retained leases | Physical journal publication | Create/drop versus open/close, monotonic complete-transaction acknowledgements, exclusive reader generations, crash recovery, quota rejection, explicit expiry, and bounded pruning |
| Consistent source snapshot export | Existing owner snapshot codecs, Common Storage, Engine adapters | Complete state codecs, durable frontiers, subscriptions | Data/catalog/artifact cut and log-retention handoff are atomic; bounded chunks, manifests, concurrent source writes, export cancellation and invalidation |
| Replica import lifecycle | Existing application owners, Common Storage, providers, Engine state | Snapshot format and target publication | Empty target remains unreadable until complete verified import and checkpoint activation; reopen/resume/failure handling and stable logical identity ranges |
| Ordered replica apply | Execution, Graph, index/model owners, Common Storage, provider adapters | Target publication, owner effect codecs, import lifecycle | One visible transaction plus source checkpoint, no user-code replay, exact retry, bounded duplicate history, divergence/gap detection and uncertain-outcome resolution; split owning apply adapters |
| Embedded public lifecycle and complete coverage | Engine and `uqa-api` | All relevant units above | Capture activation drains prior writers, every opener obeys mode, capability negotiation covers all objects/paths, source/replica mode is exclusive, reader cancellation and restart are correct |
| Qualification and public documentation | Owner tests, Engine's existing integration harness, manuals/examples | Completed public core | Four-provider source tests, sixteen source/target pairs, persistent crash matrices, automatic Linux/macOS correctness/bounded-work/memory results, disabled/enabled performance evidence, and verified Rust examples |

Provider persistence can be implemented and tested against sealed owner-neutral envelopes before all semantic codecs exist. Such infrastructure does not advertise a public replication source. Independent owner codecs can be reviewed on separate branches; dependent diffs use the prerequisite branch as their base until it merges.

## Required source audit before implementation

The design audit identifies the foundation, but each delivery unit must inspect the exact current source and enabled features before editing. Changes made after the 0.6.3 baseline may alter ownership or interfaces.

| Area | Entry points and existing evidence to extend |
| --- | --- |
| Commit identity and private writes | [persistence contract](../../crates/uqa-storage/src/mvcc/persistence.rs), [prepared commit](../../crates/uqa-storage/src/mvcc/commit.rs), [private batch](../../crates/uqa-storage/src/mvcc/session/batch.rs), and [common session tests](../../crates/uqa-storage/tests/cases/mvcc_sessions.rs) |
| Native SQLite and KV | [physical publication](../../crates/uqa-storage-sqlite/src/mvcc/write.rs), [record owner](../../crates/uqa-storage-sqlite/src/mvcc/mod.rs), and [MVCC test modules](../../crates/uqa-storage-sqlite/tests/cases/mvcc.rs) |
| redb | [record owner](../../crates/uqa-storage-redb/src/mvcc/mod.rs) and [provider contract harness](../../crates/uqa-storage-redb/tests/store_contract.rs) |
| Memory and session state | [construction](../../crates/uqa-engine/src/open/lifecycle.rs), [transaction finish](../../crates/uqa-engine/src/transactions/frame_finish.rs), [publication](../../crates/uqa-engine/src/transactions/publication.rs), and [state ownership](../design/engine-state-ownership.md) |
| Sequences and identities | [durability contract](../design/sequence-publication-durability.md), [sequence obligation tests](../../crates/uqa-storage/tests/cases/mvcc_sessions/sequence_durability.rs), [identifier tests](../../crates/uqa-storage/tests/cases/mvcc_sessions/identifiers.rs), and [native sequence tests](../../crates/uqa-storage-sqlite/tests/cases/mvcc/native_sequences.rs) |
| Trigger and rule effects | [trigger tests](../../crates/uqa-engine/tests/catalog/sql_triggers.rs), [rule tests](../../crates/uqa-engine/tests/catalog/sql_rules.rs), and the corresponding Execution mutation owners; `session_replication_role` is not an apply implementation |
| Vector and graph state | [storage contracts](../manual/internals/03-storage.md), [vector index design](../design/vector-indexes.md), [graph tests](../../crates/uqa-storage/tests/cases/mvcc_sessions/graph.rs), and [DiskANN tests](../../crates/uqa-storage/tests/cases/mvcc_sessions/diskann.rs) |
| Layering and features | Each affected `Cargo.toml`, [workspace policy](../../scripts/workspace-dependency-policy.json), [Engine capability policy](../../scripts/engine-capability-policy.json), and the manual's owner boundaries |

Produce a write-path inventory before public activation. For each path, record its semantic owner, event admission point, savepoint marks, physical publication boundary, snapshot codec, replica apply codec, and tests. Include SQL, managed non-SQL APIs, internal maintenance, autonomous sequence/identifier writes, migrations, and third-party provider defaults. A missing row in that inventory is an uncovered capability, not grounds for assuming the normal SQL path is sufficient.

## Acceptance matrix

| Contract | Required cases | Primary test owner |
| --- | --- | --- |
| Framing and values | Empty/large/malformed envelopes, truncated or reordered chunks, unknown required versions, length overflow, decompression limits, nested/array/domain/composite values, NULL versus missing, numeric/temporal fidelity | Codec-owning crates |
| Atomic source publication | Rejection before write, failure before/after physical commit, lost receipt reply, acknowledged/reclaimed receipt interactions, no journal-only/data-only visibility, net-zero row transactions, event-only publications | Common Storage and each provider |
| Capture semantics | Implicit and explicit transactions, multi-statement input, SAVEPOINT/ROLLBACK TO, statement errors, triggers and deferred triggers, cascades, COPY, MERGE/upsert, no-op updates, create/drop in one transaction, and direct managed APIs | Execution and other mutation owners; Engine end-to-end adapter cases |
| Activation and modes | Transactions writing before activation, read-to-write promotion across activation, peer opener mode discovery, incompatible objects, raw managed writes, disabling with retained consumers, and replica write refusal through every entry point | Owning admission state machines and Engine integration |
| Autonomous state | Aborted consumers, savepoint undo, cached `nextval`, `setval`, allocation reuse prevention, sequence generation changes/exhaustion, deferred SQLite durability, read-only/rollback completion barriers, and export of only the durable prefix | Existing sequence/identity owners and providers |
| Snapshot handoff | Writes before/during/after cut, transactional DDL, drop/recreate, autonomous state races, schema/resource dependencies, partial import, source restart, target restart, cancelled exports, expiry, and exact manifest resume | Snapshot owners and Engine integration |
| Apply and recovery | Duplicate current/recent publication, stale replay after digest pruning, divergent duplicate, missing predecessor, wrong source/epoch, capability mismatch, cancellation, data/checkpoint crash boundaries, and local uncertain outcomes | Storage/provider owners and semantic apply owners |
| CDC consumers | Two independent consumers, one slow or disconnected consumer, reader lease exclusivity, stale reader acknowledgement, read without ack, lost ack reply, incomplete-transaction ack, monotonicity, drop, quota exhaustion, invalidation, and reclamation races | Common Storage and provider durability tests |
| Search and model observations | Already evaluated index keys after routine changes, analyzer/resource identity, graph membership, vector updates/deletes, IVF/HNSW/DiskANN generation/topology changes, and deterministic queries after snapshot and replay | Existing index/Graph/Analysis/ML owners |
| Resources and cleanup | Large single transaction/value/DDL/resource, bounded reader batches, staging/spill failure, disk-full before commit, cancellation at every retained state, encrypted spill, close/drop/reopen, and no leaked leases or pins | Each allocating/lifecycle owner |
| Restore and migration | Supported predecessor migration, old-binary open fence, new stream epoch on source restore/clone, no copied local leases/receipts, restored replica checkpoint validation, missing retained suffix, and replay format rejection before mutation | Provider migration and state owners |
| Access | Export/import capability checks, revoked reader authority, historical/before-image confidentiality, security metadata dependencies, external binding mismatch, and no data leakage in diagnostics | Engine authorization adapters and owner codecs |

Source behavior is exercised through actual memory, native SQLite, SQLite KV, and redb engines. The same source fixture is replayed into each of the four target providers, yielding sixteen combinations, including each provider's self-pair. Compare owned logical state and deterministic query results, not physical file bytes. Persistent source and target cases reopen the actual database; the memory cases assert loss of continuity after a new engine is created instead of claiming crash persistence.

Cross-process SQLite tests cover separately opened native and KV providers. redb tests cover the supported shared database owner and real process restart, without inventing unsupported simultaneous file openers. Persistent crash tests inject faults around data/journal commit, durable-frontier publication, snapshot activation, target progress commit, acknowledgement, and pruning. Every injected uncertain result must be resolved by durable authority rather than by retrying SQL.

Independent PostgreSQL 18 fixtures verify SQL behavior affected by capture integration, such as trigger order, rollback, sequence effects, and row images. They do not serve as an oracle for this UQA replication format or justify introducing PostgreSQL behavior differences. Codec goldens and source-to-target equivalence tests independently verify the UQA stream.

## Performance and verification gates

Define disabled-capture baselines and enabled workloads before optimization: small row commits, batched writes, a large transaction exceeding memory allowances, a large value, DDL, sequence-heavy ingestion, graph/vector updates, concurrent writers, snapshot export under writes, and a lagging consumer at its quota. Record bounded work, allocated/retained bytes, spill and journal bytes, publication count, physical synchronization count, and output correctness. Memory and disk limits are functional assertions; noisy elapsed time cannot substitute for them.

Use the repository's automatic Linux/macOS performance regression checks for product deliveries and verify the actual reports for the final source. Controlled timing claims require a controlled host and an independently established noise bound. Do not lower source workloads, split application input, retry uncontrolled runs until they pass, or count build/archive jobs as performance evidence. Native sequence capture must preserve the existing durability optimization while exporting a safe prefix.

Tests stay in their owning crates and use each crate's existing single test executable. Extend modules under the current harness rather than adding another top-level test target. Run the relevant dependency and capability checks before committing; add focused format, lifecycle, and provider tests with the implementation that owns them. Register required regression cases when the behavior is implemented, not as placeholder passing checks in this design change.

Feature delivery also includes the written algebraic preservation argument required by [AI_POLICY.md](../../AI_POLICY.md#prove-feature-additions), covering the existing carriers and observations affected by capture or apply. The present design identifies those obligations; it does not claim that an unimplemented protocol has been proved or qualified by finite tests.

For each implementation PR: finish its scope and local focused verification while draft, reconcile this plan and linked tasks, review the source, and then mark it ready to trigger the affected checks. If qualification finds a product defect, return the PR to draft and complete that correction batch. Check actual final-source Linux/macOS results and any affected FDW configurations before merge. Let configured repository events trigger CI; do not manually dispatch, rerun, or cancel workflows.

## Migration and public delivery gates

- [ ] Specify envelope, owner-codec, provider-format, and capability compatibility fixtures before assigning format versions.
- [ ] Add provider migration/open fences so older code cannot write a capture-enabled or replica database without respecting its journal and mode.
- [ ] Demonstrate a complete codec/write-path inventory; reject uncovered objects or mutations before source activation/publication.
- [ ] Verify all sixteen provider pairs and the durable-source/target recovery matrix with bounded resources.
- [ ] Verify the Rust API and an executable local transfer example through the public Engine path, with no network service.
- [ ] Update the manual, feature discovery, HISTORY, examples, and API documentation only for the delivered behavior and verified provider coverage.
- [ ] Record actual final-source automatic qualification, merge completed units, reconcile related tasks, and remove merged branches.

Third-party providers can opt into the full capability after passing the same conformance contract. Existing ordinary provider operations remain supported when capture is disabled. Dependency upgrades, private SQLite hooks, source patches, and package forks are not part of this plan; physical persistence uses existing public interfaces.

## Separate future work

Network adapters can later transport the same versioned snapshot/journal frames and return confirmed checkpoints. They own authentication, encryption in transit, connection recovery, retry scheduling, backpressure, and remote status. They do not own source commit atomicity, retention, replica apply admission, or acknowledgement semantics.

Other separate work includes language-binding adapters, table/row/column CDC projections, sink connectors, PostgreSQL-compatible publication/subscription and catalogs, synchronous replication policies, writable promotion with authority fencing, cascading replication, and multi-source conflict handling. These items are not unchecked prerequisites hidden inside the network-free core. The core's complete implementation scope remains the delivery table and acceptance matrix above.
