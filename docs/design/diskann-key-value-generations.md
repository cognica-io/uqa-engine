# DiskANN physical generations

The Storage-owned `KeyValueDiskANNStore` persists physical DiskANN generations through existing versioned Key/Value sessions. SQLite Key/Value, redb and native SQLite share staging, physical sealing and retained page sources. Bound canonical sources use the [generation publication contract](diskann-generation-publication.md) to select a sealed generation and its complete coverage atomically; the [bounded journal pruner](diskann-journal-pruning.md) consumes that coverage under the same provider transaction. [Query generation selection](diskann-query-generations.md) retains committed and private physical sources with the canonical view. Candidate merging, reclamation and SQL creation remain unfinished.

## Ownership and identities

`connect` requires a versioned provider, an independent session with the same transaction-history and persistent-provider identity, durable identifier allocation, and retained reads. It opens a dedicated staging session and probes retained-reader affinity before initialization writes. No caller SQL transaction is committed or rolled back by the repository. Clones share a short staging-operation mutex; graph stream validation releases that mutex and all provider guards between reads.

An initialized 16-byte data identity is distinct from the provider's transaction-history identity. Autonomous identifier allocation reserves generation numbers within a data/table/index namespace. Allocation survives rollback and cleanup, so a consumed generation number is never reused. The low-level physical API accepts nonzero 64-bit table/index handles. `allocate_bound_stage` instead obtains those handles from durable mappings of a retained canonical source's full 128-bit catalog incarnations; it never truncates or hashes an object identity.

`index_scope` resolves the stored index identity from the canonical source's fixed catalog view. Execution's `DiskANNIndexIdentityResolver` invokes the existing SQL definition decoder and validates the index against the captured table object. Storage retains the actual table object, storage generation, resolved index object, exact record history and original controls. Native SQLite supplies its captured native row; SQLite Key/Value and redb supply their retained catalog record. Missing definitions, zero identities, foreign history and cancellation fail explicitly. A scope is preparation evidence: the source's current-definition guard is still required at publication.

The table mapping keys the complete table object and storage generation; the index mapping adds the complete index object. Both mappings are inside the existing physical provider and use its durable, non-reused allocator. Names do not participate, so renaming preserves a handle and recreating an index with another object identity does not. Mapping creation uses one evaluated batch; competing creators must adopt the already committed selection or resolve their failed original attempt. Allocation opens no caller transaction and never completes one. A rolled-back SQL definition may leave unused staging mappings; their lifecycle reclamation belongs with orphan generation cleanup before public enablement.

## Persistent namespace

The binary root is `\0uqa-diskann-v1\0`. The database marker uses root plus tag 0, with value revision 1 followed by the nonzero 16-byte data identity. A generation prefix is root, tag 1, data identity, table incarnation, index incarnation and generation number; its three integer components use big-endian 64-bit encoding.

Root tag 2 followed by the data identity, 16-byte table object and 16-byte storage generation addresses a table-handle mapping. Root tag 3 adds the 16-byte index object and addresses an index-handle mapping. Each value is exactly nine bytes: revision 1 and a nonzero big-endian `u64` handle. Root tag 4 plus the data identity is the allocator namespace for these handles. Generation allocation continues to use its existing per-table/index namespace; mapping and generation identifiers therefore have independent watermarks.

| Suffix after generation prefix | Value |
| --- | --- |
| Tag 0 | Fixed 18-byte state: revision, status, nonzero 16-byte staging owner |
| Tag 1 | Encoded manifest |
| Tag 2 | Encoded PQ codebook |
| Tag 3 + big-endian 64-bit first node | Independently addressed encoded code batch |
| Tag 4 + big-endian 64-bit first position | Independently addressed encoded numeric-side batch |
| Tag 5 + big-endian 64-bit page ID | Exactly 4,096 encoded graph-page bytes |
| Tag 6 + big-endian 64-bit first document position | Complete encoded document-origin batch for manifest revision 3 |

Unknown tags, extra key bytes, unsupported state revisions and absent required records fail closed. The existing [generation](diskann-generation-format.md) and [page](diskann-vector-index.md#node-and-page-encoding) codecs own payload validation. Metadata, staging records and pages remain inside the original provider's encryption and backup domain; no sidecar payload files or unrelated SQLite connections are opened.

## Staging and physical sealing

`allocate_stage` returns the reserved generation and its owner before `start` attempts any state write. The caller retains that identity even when creation loses its commit reply. `resume_stage` requires existing state and adopts its persisted owner; it cannot create an absent generation. A handle that attempted creation or cleanup cannot recreate its namespace after removal.

| State | Permitted action and next state |
| --- | --- |
| Writing | Append absent record/page keys; atomically write the manifest and move to Frozen; discard |
| Frozen | Read and verify complete physical streams; move to Sealed only after successful verification; discard |
| Sealed | Open retained sources and verify repeated sealing requests against the same manifest |
| Discarding | Continue bounded deletion until the namespace and state are gone |
| Published | Remain selected by the logical head; atomically become Retired when a verified replacement publishes |
| Retired | Remain readable under retained ownership; await separate lifecycle reclamation |

Each append requires the unchanged database marker and staging-state revision at commit. Freezing changes the state revision, fencing even writes evaluated before freezing. Records cannot be replaced through the staging API. Verification scans at most 64 fixed keys per page, releases the key visitor, copies one bounded record or graph page, then invokes the common artifact sealer. It validates every ordered code/side/page stream and its final digest. Revision-3 generations additionally require complete ordered origin batches, including empty tensors, and matching total tensor cardinality and digest. All logical origin records use the same native binary mapping and encryption domain; no native family or schema change is needed. An incomplete or corrupt generation remains Frozen and unavailable to ordinary readers.

The final Sealed transition conditionally requires the original owner, Frozen state and unchanged manifest. Sealed means complete physical artifacts; bound publication additionally validates exact captured coverage, catalog parameters/incarnations, expected head and physical mappings. Public query routing and SQL lifecycle integration remain separate obligations.

`discard_step` accepts a positive record count capped at 64. It fences later writers and deletes at most that many payload records in one atomic mutation, retaining Discarding until the final empty/partial page removes the state. Cleanup is explicit and resumable, never a fallible destructor operation. Sealed-generation reclamation is rejected because only the future catalog/retention owner can authorize it.

## Commit outcomes and retained reads

The dedicated writer uses the existing MVCC evaluated-batch and receipt protocol. An unresolved attempt blocks subsequent staging operations. `commit_pending` and `rollback_pending` resolve that original session attempt, preserving typed committed/indeterminate outcomes; they do not replay staging callbacks or create a second recovery protocol. After process loss, existing provider receipt recovery and `resume_stage` supply durable state. Autonomous allocation may leave harmless unused generation numbers when creation fails.

`open_source` retains one immutable provider read boundary and reads only the fixed database marker and state. It does not enumerate or load graph/PQ records. The retained source holds its MVCC lease after the live repository closes; historical values remain available through later replacement, deletion and version reclamation. Page batches contain at most 32 unique IDs and report actual I/O concurrency 1.

Point reads enforce each encoded-size cap before provider materialization. Source metadata, copied key pages and reader buffers retain the supplied memory allowance; provider private batches and retained sessions keep their existing separate session allowance. Every read uses the current operation's cancellation and workspace control, so cancelling the opening query does not cancel independent readers. Canonical query selection additionally retains the original query cancellation while preserving that physical opener contract. Bounded physical construction follows the [generation build contract](diskann-generation-build.md); public index integration and complete runtime resource acceptance remain later obligations.

## Native SQLite mapping

`ManagedConnection::diskann_generations` requires an already bound native record session and returns the same Storage-owned generation repository. It obtains an independent staging session from the existing pool, retaining its encryption credential, native data namespace, transaction-history identity, retention allowance and receipt protocol. It does not construct a SQLite Key/Value database over a native file. Opening it during a caller transaction does not publish or undo the caller's changes.

Native mapping format 10 registers database-owned family 57, `_uqa_mvcc_native_diskann_records`, with BLOB key/value columns. The logical bytes above become the physical table's binary key/value; native MVCC keys and row envelopes wrap those bytes without JSON or text conversion. A graph-page value remains exactly 4,096 bytes in the physical table. Native materialization and history publication share the existing guarded atomic commit. Upgrade preserves earlier family identities and format 9's independently persisted data namespace; failed upgrades leave the previous schema/history intact. Current malformed tables or missing guards reject reopening.

The private translation adapter implements the existing ordered Key/Value interface. Native key encoding owns both complete BLOB components and unterminated byte-prefix encoding, preserving empty keys, embedded zero bytes, binary order and strict continuation. Key-only scans never fetch payloads. Point-read limits include the checked native row/key envelope before the provider loads a value; decoding checks the complete native identity and logical key before exposing borrowed payload bytes. Compound reads, retained prefix scopes, original revisions, conditional writes, savepoints and typed commit outcomes retain their existing common MVCC owners. No generation state machine is duplicated in SQLite.

## Verification

The reusable conformance suite writes an actual graph page, codebook, code batch, side batch and manifest; checks raw signed-zero bits and original vector versions after reopening; verifies caller-transaction isolation; rejects late writes, missing/oversized pages and corrupt seals; and exercises bounded cleanup plus retained reads across replacement, deletion and collection. Both native SQLite and SQLite Key/Value run it through plain, encrypted, compressed and encrypted-compressed connections. redb closes all prior shared-owner handles before its cold reopen.

Common MVCC fault tests cover lost creation replies, rejected publication, lost replies before/after durability, original commit fingerprints and a previously evaluated writer racing freeze or discard. Fixed metadata tests preserve typed quota diagnostics and reject missing, repeated or suppressed invalid completions. These are deterministic correctness/resource checks, not timing or SSD performance evidence.
