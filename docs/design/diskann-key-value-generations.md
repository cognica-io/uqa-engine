# DiskANN physical generations

The Storage-owned `KeyValueDiskANNStore` persists physical DiskANN generations through existing versioned Key/Value sessions. SQLite Key/Value, redb and native SQLite share this implementation. It supplies staging, physical sealing and retained page sources; canonical MVCC coverage, catalog publication, search and SQL creation remain separate implementation units.

## Ownership and identities

`connect` requires a versioned provider, an independent session with the same transaction-history and persistent-provider identity, durable identifier allocation, and retained reads. It opens a dedicated staging session and probes retained-reader affinity before initialization writes. No caller SQL transaction is committed or rolled back by the repository. Clones share a short staging-operation mutex; graph stream validation releases that mutex and all provider guards between reads.

An initialized 16-byte data identity is distinct from the provider's transaction-history identity. Autonomous identifier allocation reserves generation numbers within a data/table/index namespace. Allocation survives rollback and cleanup, so a consumed generation number is never reused. The current physical API accepts nonzero 64-bit table/index incarnations; mapping existing 128-bit catalog identities to durable aliases is not implemented here and must not truncate them.

## Persistent namespace

The binary root is `\0uqa-diskann-v1\0`. The database marker uses root plus tag 0, with value revision 1 followed by the nonzero 16-byte data identity. A generation prefix is root, tag 1, data identity, table incarnation, index incarnation and generation number; its three integer components use big-endian 64-bit encoding.

| Suffix after generation prefix | Value |
| --- | --- |
| Tag 0 | Fixed 18-byte state: revision, status, nonzero 16-byte staging owner |
| Tag 1 | Encoded manifest |
| Tag 2 | Encoded PQ codebook |
| Tag 3 + big-endian 64-bit first node | Independently addressed encoded code batch |
| Tag 4 + big-endian 64-bit first position | Independently addressed encoded numeric-side batch |
| Tag 5 + big-endian 64-bit page ID | Exactly 4,096 encoded graph-page bytes |

Unknown tags, extra key bytes, unsupported state revisions and absent required records fail closed. The existing [generation](diskann-generation-format.md) and [page](diskann-vector-index.md#node-and-page-encoding) codecs own payload validation. Metadata, staging records and pages remain inside the original provider's encryption and backup domain; no sidecar payload files or unrelated SQLite connections are opened.

## Staging and physical sealing

`allocate_stage` returns the reserved generation and its owner before `start` attempts any state write. The caller retains that identity even when creation loses its commit reply. `resume_stage` requires existing state and adopts its persisted owner; it cannot create an absent generation. A handle that attempted creation or cleanup cannot recreate its namespace after removal.

| State | Permitted action and next state |
| --- | --- |
| Writing | Append absent record/page keys; atomically write the manifest and move to Frozen; discard |
| Frozen | Read and verify complete physical streams; move to Sealed only after successful verification; discard |
| Sealed | Open retained sources and verify repeated sealing requests against the same manifest |
| Discarding | Continue bounded deletion until the namespace and state are gone |

Each append requires the unchanged database marker and staging-state revision at commit. Freezing changes the state revision, fencing even writes evaluated before freezing. Records cannot be replaced through the staging API. Verification scans at most 64 fixed keys per page, releases the key visitor, copies one bounded record or graph page, then invokes the common artifact sealer. It validates every ordered code/side/page stream and its final digest. An incomplete or corrupt generation remains Frozen and unavailable to ordinary readers.

The final Sealed transition conditionally requires the original owner, Frozen state and unchanged manifest. Sealed means complete physical artifacts; it is not a graph-connectivity proof, a canonical snapshot-coverage token or permission to route a public index to that generation. The later publication owner must establish those properties.

`discard_step` accepts a positive record count capped at 64. It fences later writers and deletes at most that many payload records in one atomic mutation, retaining Discarding until the final empty/partial page removes the state. Cleanup is explicit and resumable, never a fallible destructor operation. Sealed-generation reclamation is rejected because only the future catalog/retention owner can authorize it.

## Commit outcomes and retained reads

The dedicated writer uses the existing MVCC evaluated-batch and receipt protocol. An unresolved attempt blocks subsequent staging operations. `commit_pending` and `rollback_pending` resolve that original session attempt, preserving typed committed/indeterminate outcomes; they do not replay staging callbacks or create a second recovery protocol. After process loss, existing provider receipt recovery and `resume_stage` supply durable state. Autonomous allocation may leave harmless unused generation numbers when creation fails.

`open_source` retains one immutable provider read boundary and reads only the fixed database marker and state. It does not enumerate or load graph/PQ records. The retained source holds its MVCC lease after the live repository closes; historical values remain available through later replacement, deletion and version reclamation. Page batches contain at most 32 unique IDs and report actual I/O concurrency 1.

Point reads enforce each encoded-size cap before provider materialization. Source metadata, copied key pages and reader buffers retain the supplied memory allowance; provider private batches and retained sessions keep their existing separate session allowance. Every read uses the current operation's cancellation and workspace control, so cancelling the opening query does not cancel independent readers. Total build memory, public index retention and larger-than-memory build acceptance remain later obligations.

## Native SQLite mapping

`ManagedConnection::diskann_generations` requires an already bound native record session and returns the same Storage-owned generation repository. It obtains an independent staging session from the existing pool, retaining its encryption credential, native data namespace, transaction-history identity, retention allowance and receipt protocol. It does not construct a SQLite Key/Value database over a native file. Opening it during a caller transaction does not publish or undo the caller's changes.

Native mapping format 10 registers database-owned family 57, `_uqa_mvcc_native_diskann_records`, with BLOB key/value columns. The logical bytes above become the physical table's binary key/value; native MVCC keys and row envelopes wrap those bytes without JSON or text conversion. A graph-page value remains exactly 4,096 bytes in the physical table. Native materialization and history publication share the existing guarded atomic commit. Upgrade preserves earlier family identities and format 9's independently persisted data namespace; failed upgrades leave the previous schema/history intact. Current malformed tables or missing guards reject reopening.

The private translation adapter implements the existing ordered Key/Value interface. Native key encoding owns both complete BLOB components and unterminated byte-prefix encoding, preserving empty keys, embedded zero bytes, binary order and strict continuation. Key-only scans never fetch payloads. Point-read limits include the checked native row/key envelope before the provider loads a value; decoding checks the complete native identity and logical key before exposing borrowed payload bytes. Compound reads, retained prefix scopes, original revisions, conditional writes, savepoints and typed commit outcomes retain their existing common MVCC owners. No generation state machine is duplicated in SQLite.

## Verification

The reusable conformance suite writes an actual graph page, codebook, code batch, side batch and manifest; checks raw signed-zero bits and original vector versions after reopening; verifies caller-transaction isolation; rejects late writes, missing/oversized pages and corrupt seals; and exercises bounded cleanup plus retained reads across replacement, deletion and collection. Both native SQLite and SQLite Key/Value run it through plain, encrypted, compressed and encrypted-compressed connections. redb closes all prior shared-owner handles before its cold reopen.

Common MVCC fault tests cover lost creation replies, rejected publication, lost replies before/after durability, original commit fingerprints and a previously evaluated writer racing freeze or discard. Fixed metadata tests preserve typed quota diagnostics and reject missing, repeated or suppressed invalid completions. These are deterministic correctness/resource checks, not timing or SSD performance evidence.
