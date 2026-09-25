# Native DiskANN implementation plan

Status: Implementation in progress. Configuration/reference fixtures merged in [PR #157](https://github.com/cognica-io/uqa-engine/pull/157) as `95f278f0`, navigation metrics/PQ in [PR #158](https://github.com/cognica-io/uqa-engine/pull/158) as `4b14fb74`, and node/page encodings in [PR #159](https://github.com/cognica-io/uqa-engine/pull/159) as `d9d78aad`; their feature branches are removed. Generation metadata/codecs are in progress on `feature/diskann-generation-metadata`. The completion ledger remains twelve units: two complete, one in progress, nine pending. Implementation baseline: main `7faeabe3`, inspected on 2026-09-25. Numerical primitives and codecs do not add SQL support or establish runtime acceptance.

The [DiskANN design](../design/diskann-vector-index.md) defines the intended behavior and the pinned NeurIPS 2019 paper/reference inputs. Implement Vamana, product quantization, and paged beam search directly in Rust. Mathematical proofs are outside this requested plan. The [manual](../manual/README.md) remains authoritative for existing behavior; update the design when an implementation decision changes its proposed contract.

## Outcome and fixed boundaries

Deliver `CREATE INDEX ... USING diskann` with memory execution, native SQLite, SQLite Key/Value, and redb through the supported shared database owner. Rust, Python, Node.js, and real browser WASM must execute the same SQL and preserve results across their actual persistence lifecycle. The graph and full vectors must remain paged when their corpus exceeds the cache or build allowance; a persisted index that reconstructs the entire graph on open is incomplete.

Preserve canonical raw-vector cosine scores, document-level top-k, complete tensor maxima for selected documents, exact vector-threshold search, ordinary post-KNN filtering, and existing hybrid calibration/fusion. DiskANN may approximate candidate membership; it may not substitute PQ estimates for scores, silently return too few eligible documents, or convert storage errors into empty results.

Deliver ordinary inserts, tensor replacements, deletes, savepoints, independent writers, retained snapshots, rebuilds, restore, and recovery using an immutable base plus versioned changes. Keep the existing PostgreSQL 18 contracts for shared DDL, privileges, validation order, isolation, and diagnostics. PostgreSQL's built-in access methods do not supply a DiskANN oracle: use the paper/reference for the algorithm, canonical UQA search for scores, and PostgreSQL 18 for the SQL behavior it defines.

FreshDiskANN/IP-DiskANN, filtered graph traversal, OPQ, distributed execution, GPU/MLX acceleration, and unmanaged raw-file sidecars are outside this implementation. Extension packages and user-defined types are not prerequisites. The first delivery uses the existing feature structure and introduces no external ANN runtime.

## Inspected baseline and reuse decisions

The relevant `Cargo.toml` files, including target-specific dependencies and enabled/default features, [dependency policy](../../scripts/workspace-dependency-policy.json), [storage ownership](../manual/internals/03-storage.md), [verification rules](../manual/internals/09-verification.md), existing vector implementations, and owning tests were inspected before defining these units.

| Existing boundary | Implementation decision |
| --- | --- |
| [Physical index selection](../../crates/uqa-storage/src/vector_index/config/types.rs) has BruteForce, IVF, and HNSW variants | Introduce validated DiskANN configuration and explicit create/restore routing; audit every selection consumer before public enablement. |
| [VectorIndex](../../crates/uqa-storage/src/vector_index.rs) provides controlled queries and snapshots | Extend this contract only where paged ownership requires it; preserve controls through wrappers rather than materializing a substitute index. |
| [HNSW metric](../../crates/uqa-storage/src/hnsw_index/metric.rs) and [IVF training](../../crates/uqa-storage/src/ivf_index/math.rs) have different mathematical objectives | Add nonnegative Euclidean navigation and Euclidean PQ training in Storage; preserve existing HNSW/IVF results. |
| [Key/Value HNSW](../../crates/uqa-storage/src/key_value/hnsw_index.rs) loads a complete graph | Reuse transaction/identity boundaries, but implement a separate page reader, resident-code owner, and bounded cache. |
| [Common vector MVCC](../../crates/uqa-storage/src/mvcc/vector/layout.rs) currently dispatches IVF/HNSW record layouts | Extend the shared mutation/visibility protocol for document changes and immutable generations without reconstructing DiskANN at publication. |
| [SQL options](../../crates/uqa-sql/src/schema/indexes/options.rs) and [Execution creation](../../crates/uqa-execution/src/schema/indexes/creation.rs) separate parsing from physical construction | Keep parsing in SQL; resolve dimensions before finalizing PQ defaults in Execution through Storage validation, preserving diagnostic order. |
| [Serializable vector observations](../../crates/uqa-execution/src/serializable/vector.rs) and [snapshot retention](../../crates/uqa-execution/src/query/table_snapshot/vector_metadata.rs) already own their behavior | Extend those owners; Engine supplies the original session, table, transaction, and retained handles. |
| [Vector-search workload](../../benchmarks/vector-search/README.md) uses `retrieval_workloads` and a manifest | Add DiskANN and a correctness-only path to the existing harness; do not create another benchmark executable. |

Storage depends internally on Core and Analysis; ML already depends on Storage. PQ and graph algorithms belong in `uqa-storage`, with no Storage-to-ML or provider dependency. SQLite physical records/encryption belong in `uqa-storage-sqlite`; common Key/Value layout belongs in Storage; redb owns only its physical adapter. SQL, Planner, and Engine retain their current internal dependency budgets of two, three, and fourteen. Engine must not gain graph, numerical, page-layout, scoring, or rebuild algorithms.

Scoring already depends on Storage and owns probability transforms, reusable calibration models, and diagnostics; Storage must not acquire a reverse Scoring dependency. Operators owns query-pool fitting, Execution owns runtime identity validation/routing, and Fusion owns signed evidence combination with one prior. The [score-to-probability contract](../design/diskann-vector-index.md#score-to-probability-contract) specifies how DiskANN uses those existing boundaries.

Engine defaults enable neither analyzer; Python, Node.js, and WASM default to Nori and Kuromoji. DiskANN must work independently of those dictionary features. Preserve target-specific native/Emscripten provider capabilities; a browser adapter cannot acquire native file or worker assumptions through a shared interface.

Use Storage modules such as `diskann_index/{config,metric,pq,prune,build,format,pages,search,changes,rebuild}.rs`, splitting further by enduring ownership when necessary. These are proposed locations, not files already present. Provider and Execution changes stay with their existing owners. Add tests to existing unit modules and the crate's single integration harness; redb and Execution can use their existing library test surface without creating a new integration executable.

## Dependency order and progress ledger

The order below is the default implementation sequence. Each unit can contain several logical commits, but should remain independently reviewable and tested. A unit is complete only when its exit evidence is recorded against the actual source commit. Internal modules can merge before SQL enablement; normal catalog creation and provider dispatch must continue rejecting unavailable DiskANN support until the full runtime path is ready.

| Unit | Prerequisites | Primary owners | Status and evidence |
| --- | --- | --- | --- |
| Configuration and independent fixtures | Existing design | Storage, SQL | Merged as `95f278f0` in PR #157 after review fixes through `535183ac`: 1,536 Storage/SQL library tests, strict Clippy, fixture and dependency/ownership/harness checks pass. Automatic formatting and CodeRabbit checks passed; the manually dispatched pre-merge CI was not run. |
| Navigation metric and PQ | Configuration and independent fixtures | Storage | Merged as `4b14fb74` in PR #158: 539 Storage library tests including 15 numeric/PQ cases, strict Clippy, Python 3.8 rational fixture, dependency/ownership/harness and hygiene checks pass. Automatic formatting and full CodeRabbit review passed; manually dispatched pre-merge CI was not run. |
| Page format and controlled readers | Configuration and independent fixtures | Storage | In progress: node/page codecs merged in PR #159 (`d9d78aad`) after 549 Storage tests, strict Clippy, Python 3.8 fixtures and full automatic review. Generation metadata codecs have passed 559 Storage tests and Python 3.8 byte fixtures; review/merge pending. Controlled readers/cache and complete-unit acceptance remain pending. |
| Provider records and generation leases | Page format and controlled readers | Storage Key/Value, SQLite, redb | Pending; none |
| Vamana construction | Navigation metric and PQ | Storage | Pending; none |
| Bounded build and sealing | Vamana construction; provider records and generation leases | Storage | Pending; none |
| Search and retained views | Bounded build and sealing | Storage, Operators | Pending; none |
| Versioned changes and observations | Search and retained views | Storage, Execution, provider adapters | Pending; none |
| Publication, rebuild, and recovery | Versioned changes and observations | Storage, Execution, provider adapters | Pending; none |
| SQL lifecycle and planning | Publication, rebuild, and recovery | SQL, Execution, Planner, Engine adapters | Pending; none |
| Bindings and public documentation | SQL lifecycle and planning | Rust/API, Python, Node.js, WASM | Pending; none |
| Integrated recall and resource acceptance | All preceding units | Affected owners and existing CI/workload harnesses | Pending; none |

The twelve units are completion accounting, not twelve mandatory PRs. The PR map below subdivides the wider page-format, provider, recovery, SQL, and binding units into eighteen bounded changes. Fixture preparation, reference review, and documentation review can continue while a relevant CI run is active. Use `feature/` branches, logical commits without label prefixes, and push completed commits during implementation.

## PR boundaries and merge order

Keep at most one DiskANN implementation PR open. Each PR targets current main, contains its own focused tests and relevant documentation/plan update, and is verified and merged before the next dependent PR is opened. Remove its merged remote/local feature branch before starting the next branch. A ready local commit is pushed promptly; do not hold several completed units in one long-lived branch. While CI runs, complete review, fixture specification, and the next unit's interface analysis without building an unmerged branch stack.

The rows below specify review boundaries and suggested titles, not pre-created PRs or promises that a wide change must fit one PR. The prerequisite column uses the same titles, with shorter wording where unambiguous. Each row requires the subset of its unit's exit evidence that covers that PR's delivered behavior; the whole unit becomes complete only after all constituent PRs pass. A verified Key/Value reader can therefore merge before the native SQLite reader, while the provider unit remains in progress. If a row grows into independent behaviors, split it before implementation proceeds and update this table; do not defer its own tests to a later integration PR.

| Suggested PR title | Includes | Deferred to later PRs | Prerequisites and merge evidence |
| --- | --- | --- | --- |
| Define DiskANN configuration and reference fixtures | Storage parameters, raw SQL option descriptors, fixture provenance, consumer inventory | SQL enablement, algorithms, provider records | Existing design; configuration/fixture owner checks pass and unsupported public routing remains intact |
| Implement DiskANN navigation metrics and product quantization | Canonical-score boundary, numeric side classification, Euclidean PQ training/encoding/lookup | Graph construction, persistence, SQL | Configuration; independent numeric/PQ fixtures, quota/cancellation, and unchanged IVF checks |
| Define DiskANN node and page encodings | Checked node slots, page addressing, generation-bound envelopes, checksums and multi-fragment shapes | Manifest/PQ codecs, readers/cache, providers, graph/search | Configuration and numeric boundaries; independent byte fixtures, malformed frames, numeric validation and quota/cancellation checks |
| Encode DiskANN manifests and resident quantization state | Manifest/configuration/coverage encodings, PQ codebook/code and side-stream validation, generation/layout compatibility | Reader/cache policy, physical drivers, graph/search | Node/page encodings and PQ; recognized revisions, exact round trips, corrupt metadata and bounded allocation checks |
| Read and retain DiskANN pages within resource limits | Generation-bound reader and streaming seal contracts, controlled memory reader, resident leases, bounded cache and in-flight buffers | Persistent drivers, graph/search algorithms | Both codec PRs; multi-page reconstruction, reordered/missing/duplicate reads, lazy open and owner lifetime checks |
| Store DiskANN generations in Key/Value providers | Shared record namespaces, staging owners, retained readers, SQLite Key/Value and redb adapters | Native SQLite layout, public manifest switching, SQL | Controlled readers; actual cold/reopen/affinity checks through both Key/Value providers |
| Store DiskANN generations in native SQLite | Native page records and generation readers in supported plain/encrypted/compressed modes | MVCC change semantics, publication scheduling, SQL | Key/Value provider contract established; native physical/encryption/failure checks |
| Implement deterministic Vamana graph construction | Visited candidates, RobustPrune, two passes, reverse candidates, reserved connectivity edge | Out-of-core orchestration, search consumers | Navigation metrics/PQ; independent graph fixtures and degree/reachability checks |
| Build and seal DiskANN indexes within resource limits | Streaming partitions, encrypted temporary runs, external merge/prune, PQ/page sealing | Public generation publication and background maintenance | Vamana and both provider PRs; larger-than-build-workspace and failure-cleanup checks |
| Search paged DiskANN indexes through retained views | Beam search, tensor rerank, side streams, exact thresholds, adaptive completeness, controlled snapshots | SQL recognition and mutable-generation switching | Bounded build; query/retention tests through memory and real providers |
| Preserve DiskANN writes in common MVCC | Evaluated replacements/tombstones, visibility overlay, coverage tokens, independent writers, SSI observations | Rebuild scheduler, generation switch, DDL | Search; deterministic multi-writer/savepoint/isolation and reopen checks |
| Publish and rebuild DiskANN generations atomically | Build admission/coalescing, generation fencing, atomic switch, receipt resolution, cache publication | Full backup/upgrade matrix and final reclamation policy | Common MVCC; lost-reply, competing-rebuild, and late-writer fault schedules; unreclaimed retired state stays bounded and safe |
| Recover and reclaim DiskANN generations safely | Fail-closed restore, orphan/retired cleanup, lease/receipt retention, backup/restore, old-writer rejection | SQL access-method enablement | Publication; process-loss, final-reader, backup, and capability-negotiation checks for each provider |
| Integrate DiskANN index lifecycle with catalog owners | Internal create/restore/drop/rename/truncate scheduling, target/dimension validation, catalog identity, narrow Engine adapters | Public access-method recognition, planner costing, bindings | Recovery; owner-level lifecycle/rollback checks and PostgreSQL shared-DDL reference fixtures |
| Enable DiskANN SQL with physical planning and diagnostics | Public method/dispatch, Planner properties/costs, EXPLAIN/counters, calibration dependencies, public SQL integration tests, SQL/manual contract updates | Language artifact claims and unrelated planner rewrites | Catalog lifecycle; complete memory/persistent SQL create/query/write/reopen/drop and regression gates |
| Verify native language access to DiskANN | Rust, Python, Node.js fixtures/examples, real native artifact tests, native binding documentation | Browser acceptance and native SSD performance claims | SQL enablement; native artifacts execute the same persistence and score assertions |
| Verify browser DiskANN and binding feature parity | Real-browser persistence/worker behavior, actual WASM artifact, shared example parity, dictionary-feature independence | Performance measurement and new provider capabilities | Native language contract; browser execution and binding/feature matrix pass |
| Finalize DiskANN workload and acceptance evidence | Existing workload/reporter integration, deterministic recall/resource gates, compact final CI evidence, final support/upgrade/history updates | New production algorithms, transaction features, unrelated fixes | All runtime/artifact PRs; acceptance checks pass on final product source with no hidden incomplete unit |

A PR must have one principal behavior that its tests can assess. Do not combine an algorithm implementation with provider formats, SQL enablement, and bindings merely because all serve DiskANN. Keep necessary trait changes with their first tested consumer, rather than landing speculative abstraction-only refactors. Use logical commits for configuration/implementation/tests or provider adaptations where those are independently meaningful; a commit split does not make an oversized PR acceptable.

Intermediate merges retain existing behavior and reject unsupported public DiskANN creation. Owner-level fixtures exercise new internal paths without temporary SQL commands, permanently disabled tests, placeholder success responses, or blanket lint exceptions. Published persistent DiskANN creation becomes available only after format negotiation, mutation, publication, and recovery are complete; unsupported existing providers return an explicit capability error.

The final acceptance PR consolidates evidence and the existing workload. It is not a container for unfinished runtime work. Resolve runtime defects found during acceptance in focused corrective PRs before opening the final evidence PR. If one is already open when a runtime defect is reproduced, withdraw it while preserving its unmerged work, complete the owner correction, and then resume the evidence PR so only one implementation PR remains open. Update the PR map with that concrete addition rather than silently expanding a current PR or erasing prior completed evidence.

## Implementation details and exit evidence

### Configuration and independent fixtures

- Define resolved Storage parameters for degree, construction/search list sizes, alpha, beam width, PQ bytes, seed, and format/algorithm revisions using the design's defaults and validation rules. Store alpha in a validated canonical representation compatible with configuration equality; check arithmetic and conversion bounds before allocation.
- Add raw SQL option descriptors and parser tests without yet enabling the public access method. Dimension-dependent defaults remain unresolved until the target field is known; persist every effective setting so reopen cannot adopt changed defaults.
- Record the paper sections, pinned reference revision, fixture generator revision, numeric conventions, seed/order rules, and deliberate UQA adaptations in compact provenance. Separate reference expectations for unaugmented Vamana from the final graph's UQA connectivity edge.
- Establish small hand-checkable pruning, PQ lookup, score, tensor, tie, and graph fixtures before implementing those algorithms. Keep fixed query/corpus splits and tie handling explicit. Fix the broader recall fixture inputs and establish reviewed acceptance limits independently of the candidate's passing output.
- Inventory `VectorIndexSpec`, create/restore dispatch, retained snapshots, copied-table/view/cursor consumers, catalog serialization, and backup consumers; attach each necessary change to its owning unit.

Exit evidence: owner tests reject invalid/duplicate/cross-algorithm options, alpha and size overflow, and invalid dimension/PQ combinations; fixtures have independently reviewable expected values and provenance. Existing IVF/HNSW option behavior and public DiskANN rejection remain intact. The plan records concrete fixture and consumer paths once added.

The resolved parameters and validated `DiskANNAlpha` live under `crates/uqa-storage/src/vector_index/config/diskann.rs`; catalog decoding requires every effective value and recognized algorithm/format revisions. `crates/uqa-sql/src/schema/indexes/options/diskann.rs` preserves absent options, including dimension-dependent PQ width, and parses explicit values without adding a Storage dependency. `VectorIndexSpec` and `index_access_method` remain unchanged. Owner tests compile a real DiskANN index statement, verify its raw descriptors, and confirm that public method routing still rejects it.

Independent fixtures and their generator are in [`crates/uqa-storage/tests/fixtures/diskann`](../../crates/uqa-storage/tests/fixtures/diskann/README.md). They fix rational pruning geometry, PQ chunk/lookup values, exact raw cosine bits, tensor maxima, DocId ties, explicit initial/visit graph order, unaugmented pass outputs, separate UQA cycle augmentation, and a held-out clustered recall workload with byte fingerprints and a preselected recall floor. The canonical-score fixture is executed by Storage owner tests. Numeric/PQ acceptance is recorded below; graph construction, page access, and runtime recall have not yet been implemented or accepted.

The inspected consumer inventory assigns follow-up changes to their owning units:

| Consumer paths | Required owner/unit change |
| --- | --- |
| `uqa-storage/src/vector_index/config/types.rs`, `backend.rs`, `key_value/storage_backend.rs`; `uqa-storage-sqlite/src/backend.rs` | Add physical selection only with tested create/restore readers; generation providers and SQL lifecycle. |
| `uqa-sql/src/schema/indexes/options.rs`, `vectors.rs`; `uqa-execution/src/schema/indexes/creation.rs`, `registry.rs`, `restoration.rs` | Finalize dimension-dependent settings, preserve validation/identity order, and route lifecycle through the ready Storage implementation; catalog/SQL integration. |
| `uqa-engine/src/open/registries.rs`, `migration/schema.rs`, `tables.rs`, `capabilities/index_creation.rs` | Lend existing catalog/provider/session handles to owner-level restoration/construction; catalog lifecycle and bindings. No graph or page algorithm belongs in these adapters. |
| `uqa-engine/src/table_storage/dependencies.rs`, `columns.rs`, `persistent.rs`; `open/session_seed.rs`, `open/table_restore.rs` | Preserve registered physical selection during column changes, copied tables and reopening; catalog lifecycle. |
| `uqa-storage/src/vector_index/collection.rs`, `memory_snapshot.rs`, `read_only_snapshot/vectors.rs`; `uqa-execution/src/query/table_snapshot.rs`, `table_snapshot/vector_metadata.rs` | Retain paged generations through captures/materialization and nested views/cursors instead of rebuilding exact full-corpus substitutes; search/retained views. |
| `uqa-storage/src/mvcc/vector/layout.rs`; `uqa-storage-sqlite/src/mvcc/native/layout.rs`, `format.rs`; `uqa-execution/src/serializable/vector.rs` | Add versioned change/coverage records and logical observations; provider records, common MVCC, publication and recovery. |
| `uqa-core/src/catalog_index.rs`; `uqa-engine/src/migration/schema.rs`; `uqa-storage-sqlite/src/connection/restore.rs`, `native_restore.rs`, `mvcc/restore.rs` | Preserve resolved catalog strings and complete database-owned generation data through export/restore; format negotiation and recovery. |
| `uqa-scoring/src/vector_calibration.rs`; `uqa-operators/src/vector.rs`; Planner/Execution KNN consumers | Preserve canonical scores, requested candidate pools and calibration identity; search and SQL planning. |

All paths in the inventory are relative to `crates/`. Reinspect the relevant implementation and feature configuration when its unit begins; this inventory is not a claim that the downstream adaptations are complete.

Validation of `d096c325`: Linux Docker with Rust 1.90 ran `cargo test -p uqa-storage -p uqa-sql --lib --locked` (524 Storage and 1,012 SQL tests), and `cargo clippy -p uqa-storage -p uqa-sql --lib --tests --locked -- -D warnings`. After the final fixture adjustment, the canonical score fixture passed again. `generate.py`, scoped `cargo fmt --check`, repository hygiene, workspace dependency policy, Engine capability policy, integration-harness policy, and `git diff --check` passed. These are functional checks; no timing or performance acceptance is claimed. No CI workflow was manually dispatched, rerun or cancelled for this change.

### Navigation metric and PQ

- Implement controlled normalization and squared Euclidean navigation without changing the canonical raw `f32` cosine scorer. Keep zero-norm and nonfinite-derived-norm entries in an explicit exact side stream; define exceptional-query routing from existing scorer behavior.
- Implement deterministic byte-code PQ with nonempty coordinate chunks, all remainder coordinates, up to 256 actual centroids per chunk, bounded sampling, Euclidean centroid updates, deterministic empty-cluster handling, encoding, and query lookup. Do not use IVF's spherical centroid update for this objective.
- Account for training, lookup, codebook, and encoding buffers before allocation; bound iteration and sample work and check cancellation. Preserve raw vector bits and record codec/training revisions.
- Reuse only helpers whose semantics match. If a common training helper is extracted, retain existing IVF tests and compare its prior deterministic output before and after the extraction.

Exit evidence: exact small PQ tables and labels, non-divisible dimensions, one/tiny training sets, repeated vectors, empty clusters, malformed codes, deterministic runs, cancellation, and quota release pass. Numeric fixtures distinguish squared-distance pruning with $\alpha^2$ from an incorrect $\alpha$ factor and cover signed zero, underflow/overflow, negative cosine, and canonical score/error outcomes. PQ estimates never enter a public score type.

The implementation lives under `crates/uqa-storage/src/diskann_index`: `metric.rs` owns controlled normalization, `NavigationInput` and `SquaredNavigationDistance`; `pq.rs` owns codebooks, encoding, query tables and the distinct `PQDistance`; `pq/training.rs` owns bounded reservoir admission and seeded initialization; `pq/lloyd.rs` owns Euclidean updates. Codebook and lookup ownership retain their original memory allowance without retaining a past query's cancellation. Failed sample admission preserves both accepted vectors and RNG state. The design's [typed carrier boundaries](../design/diskann-vector-index.md#typed-carrier-boundaries) map these physical types to the manuscript without adding score or probability conversions.

On Linux Docker with Rust 1.90, `cargo test -p uqa-storage --lib --locked` passed all 539 tests, including existing IVF/HNSW cases and 15 new numeric/PQ cases; `cargo clippy -p uqa-storage --lib --tests --locked -- -D warnings` passed. `generate_training.py` reproduced its independently authored rational sample/centroid/label/distance expectations on Python 3.8. Quota sweeps exercise training admission and failure cleanup; separate checks cover failed reservoir replacement, cancellation, retained query buffers, tiny/repeated inputs, label 255 at the 256-centroid boundary, non-divisible chunks and deterministic codebook bits. Formatting, whitespace, repository hygiene, dependency, Engine ownership and single-harness checks passed. No timing or performance acceptance was performed. Generation identity, persistent codec validation, query-result integration and platform artifact acceptance belong to their later units.

### Page format and controlled readers

- Specify versioned little-endian manifest, node, fragment, PQ, and side-stream encodings with checked dimensions, lengths, counts, neighbor ranges, checksums, logical identities, and origin-version tokens. Freeze layout constants and compatibility rules before any durable public index can be created.
- Implement fixed slots and multi-fragment nodes, including dimensions that exceed the logical 4 KiB page target. Decode only after bounds and complete-fragment validation; do not allocate from unchecked stored counts.
- Implement the Storage-owned generation-bound reader contract, a memory page reader, and fault-injecting readers for missing, duplicate, short, reordered, or corrupt batches. Batch capabilities and actual concurrent I/O capabilities remain distinct.
- Implement resident-code ownership, generation leases, cache keys containing database/index incarnation and generation/format, byte-based eviction, query reservations, and bounded in-flight page buffers. Sharing data must not share a prior query's cancellation or grant an unbounded allowance.
- Add a writer/sealing interface that can stream unpublished pages and validate them without a whole-graph allocation. Define retention and ownership transfer when a build becomes a published or retained generation.

Exit evidence: format round trips and malformed-input cases pass; multi-page nodes reconstruct correctly; eviction and reordered completion preserve results. Opening an immutable fixture admits codes/metadata without enumerating all graph pages. All retained owners and query buffers release their charges at the correct final lifetime, including failed construction and cross-allowance capture.

The first bounded change implements `diskann_index::format::{DiskANNGeneration, DiskANNVectorVersion, DiskANNNodeLayout, DiskANNNode, DiskANNPage}` with node/page encode/decode functions. The design's [binary format](../design/diskann-vector-index.md#node-and-page-encoding) distinguishes persistent data identity from transaction-history identity and defines origin tokens without substituting them for snapshot coverage. Validated nodes keep raw vector bits, document/ordinal identity and origin separate from navigation or ranked output. Page envelopes are borrowed and allocation-free; complete-node validation remains a separate boundary. Persistent origin allocation, readers, fragment gathering policy, manifest/PQ/side-stream codecs, caches and providers are not implemented by this change.

Linux Docker with Rust 1.90 passed `cargo test -p uqa-storage --lib --locked` (549 tests) and `cargo clippy -p uqa-storage --lib --tests --locked -- -D warnings`. The ten new codec cases include independently fixed bytes/digests, ordered reconstruction of a two-fragment node, layout address properties and overflow, malformed lengths/identities/norms/neighbors, valid-checksum corruption, memory-limit failure cleanup and cancellation. `generate_pages.py` reproduced the compact expectations on Python 3.8. Existing numeric/PQ and IVF/HNSW tests remain green after sharing the allocation-free norm calculation. These are codec and regression results, not evidence of a complete reader, snapshot integration or runtime DiskANN search.

The second bounded change adds [generation metadata codecs](../design/diskann-generation-format.md): a fixed manifest, bounded source-coverage fingerprinting, exact `f64` PQ codebooks, code batches bound to the encoded codebook identity, and borrowed numeric-side batches. The source fingerprint includes ordered logical identities, original version tokens and raw bits; it deliberately supplies no commit-order or visibility predicate. Empty/all-side generations have no graph entry or fabricated codebook. PQ restore shares the numerical owner's option/chunk validation and retains its allocation allowance independently of old query cancellation. No provider, cache, reader, publication or SQL path is enabled here.

Linux Docker with Rust 1.90 passed all 559 Storage library tests, including ten new metadata cases, and `cargo clippy -p uqa-storage --lib --tests --locked -- -D warnings`. Python 3.8 reproduced `generate_metadata.py` expectations from the independent rational training fixture and explicitly encoded record/page layouts. Exact-byte, digest and restored-lookup checks accompany rechecksummed malformed metadata, wrong generations/codebooks, invalid counts/revisions/labels/origins, ordered-prefix failure atomicity, stream batch boundaries, memory admission/failure release, cancellation and retained-codebook lifetime tests. Formatting, whitespace, document links/fences, repository hygiene, dependency, Engine ownership and single-harness checks passed. Reader completeness across reordered/missing/duplicate batches, full artifact sealing, catalog-context validation and MVCC source verification remain required by later owners.

### Provider records and generation leases

- Implement native SQLite page BLOBs and shared Key/Value generation namespaces; use the common layout for SQLite Key/Value and redb. Keep staging, codes, pages, canonical vectors, and metadata in the database's encryption/backup domain.
- Connect retained page readers to existing database/session affinity, generation retention, and provider read controls. Do not hold a provider transaction guard or coordinator mutex during graph computation or while waiting for another worker.
- Implement bounded batch reads on every provider. Start with sequential physical reads where appropriate; only advertise overlap when actual provider workers preserve database/encryption identity and the generation lease. Canonical/change reads still require the selected logical snapshot.
- Add unreachable staged-generation records and recoverable build ownership with bounded cleanup. Published-manifest routing remains unavailable until publication and recovery are implemented; private provider fixtures exercise these records in the meantime.
- Exercise native plain SQLite, SQLCipher, compressed/encrypted SQLite, SQLite Key/Value, and redb in the provider configurations they already support. Preserve serialized-provider behavior when the existing constructor selects it; do not claim concurrent logical writers for a serialized adapter.

Exit evidence: real cold provider reads, close/reopen, generation isolation, source-handle closure, page corruption, missing records, cross-database rejection, and encryption-mode routing pass. Neither restore nor query reconstructs a complete graph. redb uses its supported shared owner, and providers report effective read concurrency truthfully.

### Vamana construction

- Implement full visited construction candidates, deterministic greedy search, RobustPrune, reverse-edge candidates, and repruning. Use full navigation vectors for construction and the two passes with alpha one followed by configured alpha.
- Specify initial graph generation, PRNG, sorted logical-key-to-node assignment, permutation, medoid sampling, and tie-breaking. A seed alone is insufficient if worker completion or hash iteration changes mutation order.
- Apply the design's UQA connectivity rule: for more than one navigable node, reserve one outgoing degree slot for a directed cycle over stable node IDs; fill the remaining slots with Vamana edges and deduplicate. Keep unaugmented reference comparisons separate.
- Handle empty, all-side-stream, singleton, tiny, duplicate, clustered, and degenerate populations without inventing HNSW levels or reciprocal-edge requirements.

Exit evidence: independently derived graph/pruning fixtures pass; final degree, no-self-edge, no-duplicate-edge, node-range, directed reachability, and deterministic-build checks pass. A focused geometry fixture detects the wrong alpha unit even if a recall workload happens to pass. Existing HNSW behavior remains unchanged.

### Bounded build and sealing

- Stream the selected canonical snapshot into bounded sampling and overlapping partitions. Account for vector bytes, edges, membership maps, sorting, validation, and temporary storage, not only the local graph payload.
- Split oversized/skewed partitions deterministically, including identical-centroid populations, with bounded recursion and encrypted temporary data. External-sort logical identities and adjacency runs instead of keeping a full global node-to-document map in RAM.
- Build one admitted partition at a time, merge candidate edge runs deterministically, perform the final global degree pruning, add the reserved connectivity edge, and stream PQ codes/pages into the unpublished generation.
- Validate complete identities, coverage, degree/reachability, codecs, fragments, and checksums using bounded passes; seal a compact publication candidate. Before publication, cancellation, corruption, or insufficient allowance must leave the old index unchanged and staging recoverable.

Exit evidence: a fixed corpus whose raw-vector size exceeds the declared build workspace completes without a full-corpus materializer; a skewed/duplicate corpus exercises capacity splitting and final merged degree limits. Record actual peak owned bytes and temporary bytes. Too-small allowances fail predictably before publication, release memory, and leave only bounded recoverable staging. Memory-provider fixtures alone do not establish this gate.

### Search and retained views

- Implement PQ-guided beam search, page-request deduplication, deterministic expansion despite reordered reads, exact raw-vector candidate scores, and byte-accounted frontier/visited/rerank workspaces. Count cached-node expansions as actual expansions.
- Reduce vector identities to documents only at the correct boundary; fetch all visible ordinals of each selected document for its canonical tensor maximum. Merge the exact numeric side stream, including zero-score vectors that outrank negative scores.
- Widen search when deleted/superseded nodes or tensor collapse leave fewer than the requested number of eligible documents. Retain sufficient deferred navigation work or a deterministic completeness traversal so widening can reach previously pruned vertices. Return a quota/cancellation error when completion cannot fit; do not silently truncate results.
- Implement exact threshold scanning over the selected canonical corpus and explicit numeric-edge query routing. Keep ordinary relational filtering and security-barrier behavior under the existing Execution contract.
- Implement `VectorIndex` controlled search, count/membership, retained read snapshots, and memory copy-on-write behavior. Audit read-only wrappers, table copies, views, cursors, and nested captures so they retain the page/code leases instead of converting DiskANN to a full exact snapshot.
- Return the final raw-cosine document pool and selected generation/configuration metadata needed by calibration consumers. PQ estimates, visited-node counts, and internal widening must not become probabilities or replace the requested document-level `candidate_k`.

Exit evidence: exact public scores and tensor maxima, stable ties, complete result counts, empty/singleton/all-deleted states, adaptive widening, side streams, thresholds, filters, and selected hybrid consumers pass. Retained queries survive source mutation/closure; cold and warm cache states produce identical results for fixed inputs. Injected read failures propagate and release resources rather than appearing as graph termination.

### Versioned changes and observations

- Extend common vector MVCC with already-evaluated per-document replacements/tombstones and base coverage from Storage-issued visibility tokens. Commit canonical row/vector state and DiskANN changes atomically; a tensor replacement masks every old ordinal, including replacement with an empty tensor.
- Stream exact visible changes under the same snapshot as base candidate validation. Include private writes, savepoint overlays, and late commits excluded from the base snapshot; do not infer coverage from transaction start time or a maximum transaction ID.
- Preserve independent logical records for disjoint writers. Use evaluated counter/maintenance merges rather than a per-write whole-index compare-and-swap or transaction-lifetime index permit.
- Extend Execution's serializable vector boundary to register conservative object-level ANN read coverage before exposing results, with document-key write observations. Cache hits and unvisited graph nodes must not omit a dependency; planning/EXPLAIN without execution does not read vector data.
- Preserve READ COMMITTED rechecks, REPEATABLE READ, SERIALIZABLE, statement rollback, and savepoint semantics through provider and retained-snapshot adapters. Commit retry reuses evaluated mutations without rerunning SQL, analyzers, callbacks, or graph construction.

Exit evidence: deterministic schedules pass for two disjoint writers in both commit orders, conflicting same-document writers, private tensor replacement, rollback, update/delete against a retained query, and serializable cycles where an unvisited changed vector affects top-k. Exact change scans remain bounded when the delta exceeds RAM. Provider checks include row/vector/index atomicity after reopen.

### Publication, rebuild, and recovery

- Add explicit generation states and owner transitions for building, sealed, published, retired, and reclaimable data. A short publication boundary validates index incarnation and expected generation, installs the manifest atomically, and preserves every change outside its actual coverage token.
- Connect bounded maintenance scheduling, request coalescing, change thresholds, progress, and cancellation. Coexisting old/new codes, readers, builders, and temporary data share accounted limits; delayed maintenance uses the streamed exact change path.
- Resolve commit receipts before retry or cleanup, including durable commit with a lost reply and cache failure after commit. Never delete possibly published pages, replay user mutations, or report rollback after confirmed commit.
- Make recovery, abandoned-build cleanup, retired-generation reclamation, and covered-change pruning respect snapshot/definition leases, cross-process retention, and unresolved receipts. Cleanup is bounded and resumable.
- Implement fail-closed restore, backup/restore consistency, explicit format negotiation, and rejection of incapable older writers, including sessions opened before DiskANN was created. Unsupported format or corrupt state must not trigger an implicit rebuild or index substitution.

Exit evidence: fault schedules cover failure during staging, after sealing, before/after durable publication, before cache publication, and during cleanup. Verify a writer starting before the build snapshot but committing after it, commits around the manifest switch, two competing rebuilds, retained readers across drop/truncate, final-reader reclamation, and process restart. Each persistent provider preserves rows, raw vectors, selected generation, and changes after recovery; incapable old writers are rejected without mutation.

### SQL lifecycle and planning

- Wire the completed Storage implementation through SQL recognition/catalog identity and Execution creation, restoration, rebuild, drop, rename, truncate, and dependency handling. Resolve target dimensions before finalizing configuration while retaining the required validation and error order.
- Add `VectorIndexSpec` and provider dispatch consistently, with one physical approximate index per field. Initial transactional CREATE includes private rows; failed CREATE leaves no reachable incomplete index; DROP preserves canonical vectors and returns the field to the existing exact path.
- Keep graph/PQ/build algorithms out of Engine. Add only narrow state/session/transaction/provider adapters required by existing owner interfaces; do not add catch-all service traits or relax capability policy.
- Extend Planner physical properties/costs and EXPLAIN for real page work, resident PQ, effective I/O overlap, changes/side streams, tensor reranking, and residual filters. Planning must not read live graph pages or train codebooks. Publish counters without labeling logical page reads as measured physical SSD operations.
- Preserve raw cosine calibration, cache/generation provenance, Bayesian evidence conversion, and existing hybrid support/prior rules. Add regression cases for unchanged IVF/HNSW selection and exact threshold planning.
- Preserve the distinct ordinary-cosine, low-level linear, query-pool, and fixed-model conversion paths in the design. Keep fixed-model fitting offline and prevent automatic use of a saved model by pool-based SQL. Extend owner tests for the existing Gaussian likelihood-ratio transform, uninformative pools, numerical bounds, and one-prior fusion.
- Validate fixed models against runtime corpus/change visibility and DiskANN generation/configuration identity through Storage metadata and Execution, rather than trusting matching caller-provided version strings alone. Define the retrieval fingerprint in the existing model target fields; reject incompatible private writes, rebuilds, index kinds, candidate K, and search settings without implicit refitting or fallback. Keep these checks and their tests in the existing lifecycle/SQL PR boundaries; no new probability algorithm is required in DiskANN.

Exit evidence: public Engine SQL create/query/write/reopen/drop succeeds through each required provider; invalid options, catalog identity, dependencies, privileges, failure/savepoint rollback, retained readers, EXPLAIN, and hybrid behavior pass. Fixed-model scores match independent expected probabilities for the same raw input; stale models fail before results are exposed; unchanged selected pools retain the existing query-pool output. PostgreSQL 18 differential fixtures cover affected shared DDL/transaction semantics with exact SQLSTATE/diagnostic expectations. Enable the access method only after the preceding runtime gates pass.

### Bindings and public documentation

- Extend the existing Rust vector example and equivalent Python, Node.js, and browser scenarios using the same fixture intent, SQL parameters, result assertions, mutation, and close/reopen outcome. Expose configuration through the same SQL; add host resource-control APIs only if required by the owner contract.
- Run actual built wheel/addon/WASM artifacts. A Rust compile, native Node run, or Node-hosted WASM run cannot substitute for the corresponding Python or real-browser execution. Browser persistence/worker assertions must use the supported browser path.
- Verify Engine without dictionary features and bindings with their normal defaults and dictionaries disabled; retain the existing independent Nori/Kuromoji feature checks. DiskANN must not become coupled to a tokenizer build feature.
- Update the [DDL manual](../manual/sql/02-ddl.md), [retrieval manual](../manual/sql/06-retrieval.md), [storage manual](../manual/internals/03-storage.md), [vector design](../design/vector-indexes.md), [binding reference](../manual/reference/08-bindings-and-extensions.md), [example matrix](../../examples/README.md), affected platform READMEs, `llms.txt`, upgrade guidance, and `HISTORY.md` when behavior is implemented. Keep proposed and supported behavior distinguishable until then.

Exit evidence: Rust, Python, Node.js, and real browser scenarios pass through their actual artifacts with persistent reopen and unchanged scores; manual SQL compile/execute and the affected example matrix pass. Record platform, features, artifact identity, and source SHA. Follow the existing full binding-parity gate when declaring the complete example matrix accepted; do not count compilation as execution.

### Integrated recall and resource acceptance

- Extend `retrieval_workloads`, the vector-search manifest, reporter, and reporter unit tests together. Add a correctness-only selection for fixed recall/output/resource checks without Criterion timing; the current timing runner is not that selection. Keep one executable and use small synthetic report fixtures for verifier tests.
- Evaluate fixed synthetic and independently sourced embedding fixtures against canonical exact SQL ground truth. Report document recall, top-1 accuracy, result completeness, shared-result score error, tensor maxima, and variation across the declared seeds. Keep canonical tie ordering distinct from any separately reported tie-aware recall metric.
- Verify probability conversion independently from ANN recall. Measure candidate-K/search-setting sensitivity and use held-out labels for Brier score, log loss, expected calibration error, and reliability bins before claiming empirical calibration. Record the model target and data split; do not treat matching cosine scores or a query-pool sigmoid as proof that one calibrator transfers across physical indexes.
- Exercise actual cold reopened providers with graph/raw data larger than the cache, and builds with raw data larger than the build workspace. Account for resident codes, decoder buffers, cache, actual visited sets, concurrent queries, coexisting generations, staged/temporary bytes, and retained changes. A mock page reader is supporting evidence only.
- Run the affected crate surfaces, cross-crate SQL histories, persistent modes, native platform CI, analyzer feature checks, and binding workflows on the final product source. Review newly reproduced failures in scope and correct their root causes before closure.

Exit evidence: reviewed recall/output floors, bounded-memory/build gates, provider fault schedules, and required platform/artifact checks pass with immutable source references. No limit is loosened merely to make a candidate pass. Performance claims remain a separate gate requiring a controlled host and an independently established noise bound; the functional implementation can be complete without claiming a speedup or billion-vector result.

## Verification strategy and evidence ownership

| Boundary | Primary oracle | Test location and minimum evidence |
| --- | --- | --- |
| Pruning, Vamana, PQ, codecs, budgets | Hand-checkable fixtures and pinned paper/reference interpretation | Storage unit submodules; malformed inputs, deterministic outputs, and cancellation/resource release |
| Pages, encryption, leases, recovery | Actual provider records and injected failure points | SQLite provider tests, Storage Key/Value conformance with test backends, and redb library tests; cold reopen and process-loss cases |
| Scores, tensors, threshold, candidate completeness | Canonical exact scorer and independent expected fixture values | Storage/Operators tests plus public Engine integration submodules; membership quality and returned-score correctness reported separately |
| Visibility, independent writers, SSI, receipts | Explicit transaction histories and existing MVCC contracts | Common Storage/Execution owner tests plus provider and Engine histories; named cases with deterministic barriers |
| SQL and optimizer behavior | PostgreSQL 18 for shared SQL behavior; UQA contracts for retrieval | SQL/Execution/Planner tests and existing Engine harness; no algorithm tests hosted in Engine for convenience |
| Probability conversion and fusion | Independent fixed-transform outputs, existing pool/prior contracts, held-out labels for empirical claims | Scoring/Operators/Fusion owner tests plus Execution target validation and Engine persistence cases; candidate-selection drift and stale-model rejection |
| Public language support | Executed real artifacts | Existing Rust/Python/Node.js/browser suites and CI workflows; persistent lifecycle and parameter parity |
| Recall and bounded operation | Versioned workload manifests and actual cold provider reads | Existing retrieval workload, reporter tests, and compact acceptance records; no historical machine-report dependencies |

During iteration, compile and run the smallest owner selection that establishes the changed behavior, then the affected integration paths. Register new test modules before selecting them and verify nonzero executed test counts; a passing command that matches zero tests is not evidence. Reuse build artifacts and normal available Cargo parallelism rather than forcing an arbitrary low job count or repeatedly cleaning target directories. Scale/recall data does not belong inside every small unit test.

Before an implementation commit, run the applicable repository checks below, plus focused owner tests and formatting/strict affected-crate Clippy. Broaden to affected crate surfaces and required CI at each merge boundary; after checks pass, rerun only when new changes or a concrete unresolved concern invalidate their evidence. Public manual SQL changes require the existing `queries::manual_sql_examples::manual_sql_examples_compile_or_execute` harness.

```sh
git diff --check
python3 scripts/check-workspace-dependencies.py
python3 scripts/check-engine-capabilities.py
python3 scripts/check-integration-test-harnesses.py
python3 scripts/check-benchmark-coverage.py
bash scripts/check-rust-file-headers.sh
bash scripts/check-rust-file-lines.sh
bash scripts/check-public-repository-hygiene.sh
```

Use the existing [native CI](../../.github/workflows/ci.yml), [Python workflow](../../.github/workflows/python-wheels.yml), and [JavaScript/WASM workflow](../../.github/workflows/javascript-bindings.yml) as the command source for platform acceptance. While CI runs, finish review, documentation, and compact evidence updates for the same unit. Verify the actual checked source before merge, distinguish later documentation-only edits, synchronize the PR body, and clean up the branch after successful merge.

Timing comparisons require the same corpus, queries, recall target, provider/encryption mode, concurrency, and declared cold/warm cache state across exact, IVF, HNSW, and DiskANN. Until host control and the noise bound are independently established, do not retry noisy timings, infer control from metadata, or delay independent correctness work. Commit only fixtures, expected outputs, limits, and compact provenance; raw JSON, traces, temporary databases, and reference binaries stay in ignored output or CI artifacts.

## Completion accounting

Keep the twelve ledger entries stable. Update an entry to in progress when its implementation starts and to complete only after its exit evidence passes. If a discovered requirement expands scope, record the concrete addition and affected gate without erasing completed evidence or repeatedly redefining the remaining count. A green local algorithm test does not close provider, SQL, or artifact acceptance.

For each completed unit, record the commit, changed owners, exact test selection and result, provider/platform/feature coverage, CI/artifact references where applicable, and any still-open gate. Keep this as a compact table or short entry, not a chronological dump of every build attempt. All units are currently pending; there are no DiskANN implementation or performance results to report.

The implementation is complete when all twelve units pass, the public SQL/artifact matrix is verified, every supported provider preserves the mutation/recovery contract, cold searches and builds satisfy the stated resource bounds, and documentation accurately describes the delivered behavior. Record controlled performance results only when available under the separate measurement gate; do not publish unsupported throughput or scale claims.

For this plan-only change, check local links, Markdown structure, one-line prose paragraphs, code/math fences, and whitespace. No Rust rebuild, database operation, reference compilation, timing run, release, or implementation PR is needed to save this document.
