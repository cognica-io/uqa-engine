# Controlled DiskANN page readers

`uqa-storage::diskann_index::pages` owns generation-bound physical reads, immutable resident state, page leases, a bounded cache, memory staging and streaming artifact seals. It uses the [node/page layout](diskann-vector-index.md#node-and-page-encoding) and [generation metadata](diskann-generation-format.md). Persistent provider adapters, graph construction, search and MVCC publication are separate units in the [implementation plan](../plans/0014-diskann-vector-index.md).

## Source and reader contracts

`DiskANNPageSource` binds one immutable `DiskANNGeneration` and its retained physical owner. Metadata keys distinguish the manifest, codebook, code batches addressed by their first dense node ID, and numeric-side batches addressed by their first stream position. A record read must visit exactly one encoded record and reject its size before provider materialization when it exceeds `max_record_bytes`. A graph request contains unique IDs and must complete every requested ID exactly once; completion order may differ from request order. Sources report maximum batch size separately from effective read concurrency.

Visitors are internal copy operations. A provider may borrow its existing data during that copy but must release its guards before returning to the reader. It must not invoke application callbacks, graph computation or worker waits while holding those guards. Provider-owned temporary buffers charge the invoking `StorageReadControl`; reader-owned copies charge the same parent allowance. A record's encoded-size cap applies individually to both copies, while the shared parent must admit their simultaneous live bytes.

`DiskANNReader::open` checks the selected catalog's dimensions and every effective parameter against the manifest, admits the decoded codebook and complete resident code array, and verifies codebook identity and the ordered code-stream digest. Checked node-count times PQ-width arithmetic precedes allocation. Empty and all-side generations allocate no codebook or code array. Normal open reads no graph pages and does not enumerate the side stream. Open validates accessed records; it does not perform a full graph integrity scan.

`read_pages` accepts strictly increasing IDs, admits a complete fixed-size destination buffer for each cache miss before calling the source, and partitions requests by source batch capacity and the in-flight byte limit. It rejects missing, duplicate, extra, short, corrupt and foreign-generation completions, including errors a faulty source suppresses after invoking a visitor. Only a fully successful request returns page leases, in request order. Provider errors remain errors rather than partial graph termination.

`read_node` validates the page envelope before decoding a packed node. For a fragmented node it reserves the complete slot, gathers one validated fragment at a time, and decodes only after every fragment arrives. Its transient page requirement is therefore one logical page regardless of the node's dimension count. Complete node validation checks raw coordinates, canonical norm, logical identity, origin and neighbor representation; it supplies no snapshot-visibility decision.

`visit_side` streams bounded numeric-side records, checks ordering across batch boundaries, and verifies the complete stream digest. An internal consumer may have received a prefix when a later error occurs; it must discard its partial candidate state and must not expose rows or publish effects before successful completion. Canonical value lookup, origin/ordinal visibility and scoring remain the candidate owner's responsibility.

## Allocation and lifetime

Core's `MemoryBudget::child` adds a component limit while preserving its parent's allowance. Every reservation charges the child and each ancestor; a failure rolls back provisional charges without changing the existing lease. Splitting or sharing a reservation retains those same owners, and final release returns every charge. Independent children compete for their shared parent's remaining capacity. The implementation introduces no dependency from Core to Storage and requires no new feature.

| Limit or owner | Charged state and lifetime |
| --- | --- |
| Reader resident child | Decoded codebook, resident codes and shared reader metadata; released at the final reader clone |
| Cache child | Page buffers, shared payload headers, ordered-map nodes and FIFO queue capacity, including evicted pages still held by leases |
| In-flight page cap | Reader-owned destination page bytes in the current source batch; source workspace and completed result leases also require the invoking parent allowance |
| Metadata record cap | Maximum encoded bytes for one record; provider workspace and copied output coexist under the invoking parent |
| Query allowance | Request/slot/result headers, uncached page buffers, fragment assembly and decoded node buffers until their actual owners drop |
| Memory source allowance | Staged record/page bytes, map nodes and shared immutable physical owner until the final source clone drops |

The cache belongs to one immutable manifest, so its page IDs are already scoped by database, table, index, generation and format. Admission uses FIFO eviction by successful insertion order, with exact owned-byte accounting rather than an entry-count estimate. Hits clone the existing lease. A pinned page remains charged after eviction; exhausting the cache or its retained parent leaves a valid query-owned page uncached. Cancellation and structural errors propagate instead of becoming cache misses. Cache admission does not hold a provider guard.

Reader clones share immutable codes and cache ownership without storing an earlier query's cancellation signal. Each operation checks its current invocation. Sharing a reader with another query does not transfer retained bytes to that query or enlarge the original parent. A returned page owns either an independent query copy or a shared cache allocation; it can outlive the reader without keeping every physical memory-source record alive. Reference-count/allocator bookkeeping retains Core's existing exclusion from payload accounting.

## Unpublished writes and physical sealing

`DiskANNMemoryBuilder` owns charged staging records and pages. Writes cannot replace an existing key, overwrite a page, or supply a manifest before sealing. `finish` consumes the builder, verifies the complete physical streams, writes the manifest and transfers the original allocations to an immutable `DiskANNMemorySource`. Any failure drops unpublished state. This source reports a maximum batch of 32 pages and actual concurrency of one; it demonstrates controlled memory storage, not disk or larger-than-RAM operation.

`DiskANNArtifactSealer` accepts codebook, contiguous code batches, contiguous side batches and ascending graph pages. It validates every node, cross-page graph identity order, cross-batch side order, complete fragment assembly, artifact counts and all manifest digests. Its graph workspace retains at most one assembled slot and one decoded node, rather than a whole graph. Once a stream operation fails, the sealer cannot later produce a successful seal.

`DiskANNArtifactSeal` certifies physical stream completeness and representation only. It does not establish graph reachability, source-snapshot completeness, graph/side coverage correspondence, canonical origin validity or publication eligibility. The later bounded builder must verify those graph/source obligations, and the MVCC owner must validate coverage and publication under its existing transaction rules. No seal changes a public catalog or makes SQL DiskANN available.

## Typed carrier boundary and verification

These readers expose physical vector identities and bytes. They do not create decorated postings, ranked rows, evidence or probabilities. The [typed carrier boundary](diskann-vector-index.md#typed-carrier-boundaries) still requires snapshot/version validation, distinct document candidates, complete visible tensor scoring and the existing posting/ranking contracts. Cache residency, batching and completion order preserve decoded values for the same immutable bytes; they cannot authorize score merges, early top-k truncation or exact-search equivalence.

Owner tests cover lazy open, fragmented nodes, reordered and malformed source completions, catalog and aggregate-digest mismatches, zero/tiny cache behavior, pinned eviction, source closure, cancellation, allocation failure cleanup, component/parent limits, complete/failed sealing and empty/all-side generations. A materializing source checks that both simultaneous record buffers charge the original parent without halving the encoded-record cap. Core separately tests sibling concurrency, failed growth rollback, shared payload lifetime and deep child ownership. These checks establish the physical reader contract; real provider, MVCC and retrieval acceptance remains separately tracked.
