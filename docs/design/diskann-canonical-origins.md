# DiskANN canonical origins

Status: Common MVCC and Key/Value origins merged in PR #170; native SQLite canonical sources merged in PR #171; ordered corpus reads and retained-source build capture merged in PR #172. [Canonical document scoring](diskann-canonical-scoring.md) merged in PR #173 and the [Key/Value change journal](diskann-versioned-changes.md) in PR #174, with native journaling in PR #176. [Retained build membership](diskann-build-coverage.md) is implemented for review. Provider-bound publication, paged query integration and public DiskANN routing remain pending in the [implementation plan](../plans/0014-diskann-vector-index.md).

## Ownership and identity

Storage's common MVCC owns `StorageMutationOrigin`, `KeyValueStore::with_versioned_mutation` and `VersionedKeyValueStore::with_versioned_record_mutation`. The record-level scope supplies the same fixed native record view as its batch; the Key/Value scope adapts that view without a second capture. Native SQLite borrows this scope, so autocommit evaluation, failed-evaluation cleanup and receipt retries remain in common Storage. The mutation scope issues the actual durable transaction allocation that will publish its records, together with a monotonically increasing transaction-local revision. The revision advances before evaluating the callback and is excluded from statement/savepoint undo. A new transaction receives a non-reused durable allocation, so process restart cannot repeat an earlier pair. This identity is provenance; neither its numeric order nor its existence establishes commit order, publication, or snapshot visibility. Private origins can be retained after their source transaction rolls back, but cannot thereby become published base coverage.

`KeyValueDiskANNCanonical` is the first consumer. It replaces a document's existing canonical vector keys and its origin/count record in one evaluated batch. Coordinates are not duplicated into a separate DiskANN corpus. An empty tensor persists a zero-count replacement, so concurrent insert-versus-delete writers still conflict on the same origin record even when there are no shared vector ordinals. Independent documents have independent keys and do not acquire an index-wide logical write permit. The existing brute-force, IVF and HNSW Key/Value replacement paths invalidate the document's origin; their clear path removes the field's origins. Subsequent DiskANN reads reject populated unstamped documents instead of accepting a stale version. Initial adoption of existing data therefore requires an explicit canonical replacement, not a fabricated origin inferred from commit sequence or build identity.

Existing Key/Value table/column cleanup removes matching origins in the same batch as canonical data, including zero-count records. Rename and legacy-name migration rekey origins with their vectors, preserving the original version; origin-only destinations count as occupied. Retained readers keep the old boundary across those operations and their rollback. The private native binary-record wrapper forwards versioned mutation scopes with the original origin and batch mapping; that generic capability does not substitute for the native canonical vector adapter.

## Receipt lifecycle

Origins reserve managed writer allocations before commit preparation. Active transactions with such allocations remain writable and do not appear as sealed pending commits. Commit seals the already evaluated records and publishes under that same allocation, including when all changes were undone. Retry preserves both the allocation and evaluated origin bytes without calling the application again. A read-only transaction rejects the origin scope before invoking its callback.

Failed autocommit evaluation, cancellation and unwinding abort and acknowledge the early allocation with the original retention allowance and an independent cleanup cancellation signal. An abort/acknowledgement failure retains the exact attempt for rollback retry and prohibits both further writes and commit of the rejected evaluation. SSI rollback resolves an early physical allocation even before a publication binding exists. Origin metadata remains ordinary canonical data after receipt reclamation; readers do not need a historical receipt to compare origins.

## Key/Value format

Origin keys use the binary prefix `\0uqa-diskann-canonical-v1\0`, followed by the existing canonical vector field prefix and the document's big-endian 64-bit identity. Canonical vectors retain their existing keys and little-endian `f32` coordinates. Both providers use `DiskANNCanonicalOrigin` in Storage's DiskANN format module. The fixed 56-byte origin value has this layout:

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 8 | ASCII `UQAVORG1` |
| 8 | 16 | Writer database incarnation |
| 24 | 8 | Nonzero writer allocation, little-endian |
| 32 | 8 | Nonzero mutation revision, little-endian |
| 40 | 4 | Vector dimensions, little-endian |
| 44 | 4 | Reserved zero bytes |
| 48 | 8 | Tensor ordinal count, little-endian; at most $2^{32}$ |

The original database incarnation is retained as provenance across data restoration; it is not compared to the current history incarnation. These internal records do not enable published DiskANN indexes. The Key/Value layout does not change provider format negotiation; native SQLite fences its new family as described below.

## Native SQLite format and lifecycle

`SQLiteDiskANNCanonical` retains raw little-endian coordinates in the existing `_vectors` family (42). Native mapping format 11 adds object-owned family 58, `_uqa_mvcc_native_vector_origins(table_name, field, doc_id, origin)`. Its identity is the existing stable table object/generation followed by field and document; the BLOB stores the same 56-byte origin envelope. Zero-count replacements have a row even without canonical ordinals. Generation page family 57 stores no duplicate canonical values.

Mapping 12 preserves that origin layout and adds the [native change journal](diskann-versioned-changes.md#native-representation) in family 59. Canonical replacement writes coordinates, origin and immutable change identity in one native batch. Upgrading an origin-only predecessor preserves those origins without fabricating changes or coverage.

Initialization and predecessor upgrades create the origin table, its write/capture guards and the new format marker in one physical transaction. Earlier record families, histories, receipts, counters and the independent data namespace remain unchanged. The marker fences older native writers; malformed current layouts or missing guards reject reopening. Ordinary native exact/IVF/HNSW replacements and deletion invalidate origins in the same evaluated batch; clearing vectors removes empty origins too. Generic table/column lifecycle processing includes the declared origin family, preserving versions through rename and removing them on drop/purge. A retained source still sees its original owner and names after mutation or closure.

Native reads capture one `NativeSnapshot`, its original control, table incarnation and field. They inspect only document-scoped key metadata to validate complete contiguous ordinal coverage, then fetch each vector through the common bounded point-read API. The physical cap includes the exact native row envelope, both name fields and the expected canonical BLOB size. Decoding validates the complete key/row identity and table scope before exposing the BLOB, then validates vector width and finite coordinates in one reused charged buffer. An oversized private or committed row fails before decoding it as a vector; native reads do not use the existing full-field vector materializer.

## Retained canonical reads

`RetainedDiskANNCanonical` pins one committed/private view of both canonical and origin prefixes without reading the full field. Document reads validate the origin envelope, dimension and complete contiguous ordinal key set. Missing origins for populated documents, missing/extra ordinals, malformed widths, nonfinite coordinates and unsupported provider capabilities fail closed. A document with neither values nor an origin is absent; a zero-count origin represents an explicit empty replacement.

Streaming reads fetch one size-bounded value at a time and reuse one charged decoded-vector buffer. All keys, decoded values and provider buffers use the query allowance, while the fixed source retains its original cancellation and ownership boundary. An error invalidates the caller's partial output. The source can survive mutation, savepoint rollback and closure of its creating handle. `RetainedSQLiteDiskANNCanonical` supplies the same document-scoped observation contract directly from native vector/origin families.

`DiskANNCanonicalRead` exposes both retained implementations through the same dimensions, origin, document-read and ordered document-cursor contract. The cursor selects the least live document from the union of canonical keys and origin keys using bounded key-only seeks. It includes empty replacements and populated unstamped documents, checks the original and invoking controls even at exhaustion, and skips the previous document's ordinals without collecting an ID directory. Key/Value supports the full unsigned document range; native SQLite preserves its signed nonnegative document range. Native tombstones do not become live documents.

Whole-corpus visits perform each bounded cursor read before opening the next document, avoiding provider reentry from borrowed callbacks. They require strictly increasing document identities and validate every complete tensor; an empty origin emits no coordinates, while an unstamped document rejects the visit. Errors discard partial results. `DiskANNBuildInput::capture_source` passes this fixed source directly into the existing encrypted capture, preserving actual mutation origins and coordinate bits in document/ordinal order. An absent source stays absent, and a captured private undo branch remains fixed after rollback and source closure.

This corpus stream supplies build input and the future exact-threshold/numeric-edge query path. Its build fingerprint remains an integrity fingerprint, not a visibility token, membership oracle or publication authorization. Base-versus-change coverage must still come from Storage's actual retained visibility boundary; corpus enumeration is not a per-query substitute for the required exact changed-vector stream.

`check_control` validates the source's original controls and the invoking control without opening a value read. The canonical scorer uses it during coordinate chunks and before zero-k/empty work, so avoiding I/O cannot bypass retained cancellation. Complete document scoring and exact query selection belong to the separate [scoring contract](diskann-canonical-scoring.md).

## Carrier boundary and validation

This change supplies canonical `(document, ordinal, origin, raw coordinates)` observations. It does not collapse ordinals, construct postings, assign scores, convert probabilities or rank results. Thus the [typed carrier boundary](diskann-vector-index.md) remains unchanged: snapshot-valid vector identities must still become distinct document candidates before every visible ordinal contributes to that document's canonical cosine maximum. Origin equality alone establishes neither document support equality nor payload/rank equality. Exact changed-vector merging, base coverage validation, conservative SSI query observations and catalog row/vector atomicity remain integration obligations.

Provider conformance exercises actual issued origins on SQLite Key/Value and redb, including private retained tensors, statement/savepoint undo, independent writers in both commit orders, empty-tensor conflicts, cold reopen, failed evaluation, cancellation, unwinding, failed abort and lost commit replies. A tensor with 16,384 raw coordinate bytes is streamed under an 8,192-byte query allowance; this is an owned-byte check, not timing or process-RSS evidence. Final source-scoped results are recorded in the implementation plan after verification.

Native provider verification exercises actual `_vectors` and origin rows with an empty generation-record family, four plain/encrypted/compressed file modes, physical write guards, cold reopen, retained private undo branches, both disjoint commit orders, same-document conflicts, complete catalog lifecycle, ordinary vector invalidation, malformed envelopes/ordinal sets/native row identities, bounded tensor streaming and independent query/original cancellation. Native evaluation errors, unwinding and cancellation use common receipt cleanup. Local pass counts and scope are recorded in the implementation plan; automatic review and check results belong to the PR body.
