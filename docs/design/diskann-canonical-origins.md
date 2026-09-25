# DiskANN canonical origins

Status: Key/Value implementation under verification. Native SQLite canonical layout, changed-vector coverage, query scoring and public DiskANN routing remain pending in the [implementation plan](../plans/0014-diskann-vector-index.md).

## Ownership and identity

Storage's common MVCC owns `StorageMutationOrigin` and `KeyValueStore::with_versioned_mutation`. The mutation scope issues the actual durable transaction allocation that will publish its records, together with a monotonically increasing transaction-local revision. The revision advances before evaluating the callback and is excluded from statement/savepoint undo. A new transaction receives a non-reused durable allocation, so process restart cannot repeat an earlier pair. This identity is provenance; neither its numeric order nor its existence establishes commit order, publication, or snapshot visibility. Private origins can be retained after their source transaction rolls back, but cannot thereby become published base coverage.

`KeyValueDiskANNCanonical` is the first consumer. It replaces a document's existing canonical vector keys and its origin/count record in one evaluated batch. Coordinates are not duplicated into a separate DiskANN corpus. An empty tensor persists a zero-count replacement, so concurrent insert-versus-delete writers still conflict on the same origin record even when there are no shared vector ordinals. Independent documents have independent keys and do not acquire an index-wide logical write permit. The existing brute-force, IVF and HNSW Key/Value replacement paths invalidate the document's origin; their clear path removes the field's origins. Subsequent DiskANN reads reject populated unstamped documents instead of accepting a stale version. Initial adoption of existing data therefore requires an explicit canonical replacement, not a fabricated origin inferred from commit sequence or build identity.

Existing Key/Value table/column cleanup removes matching origins in the same batch as canonical data, including zero-count records. Rename and legacy-name migration rekey origins with their vectors, preserving the original version; origin-only destinations count as occupied. Retained readers keep the old boundary across those operations and their rollback. The private native binary-record wrapper forwards versioned mutation scopes with the original origin and batch mapping; that generic capability does not substitute for the native canonical vector adapter.

## Receipt lifecycle

Origins reserve managed writer allocations before commit preparation. Active transactions with such allocations remain writable and do not appear as sealed pending commits. Commit seals the already evaluated records and publishes under that same allocation, including when all changes were undone. Retry preserves both the allocation and evaluated origin bytes without calling the application again. A read-only transaction rejects the origin scope before invoking its callback.

Failed autocommit evaluation, cancellation and unwinding abort and acknowledge the early allocation with the original retention allowance and an independent cleanup cancellation signal. An abort/acknowledgement failure retains the exact attempt for rollback retry and prohibits both further writes and commit of the rejected evaluation. SSI rollback resolves an early physical allocation even before a publication binding exists. Origin metadata remains ordinary canonical data after receipt reclamation; readers do not need a historical receipt to compare origins.

## Key/Value format

Origin keys use the binary prefix `\0uqa-diskann-canonical-v1\0`, followed by the existing canonical vector field prefix and the document's big-endian 64-bit identity. Canonical vectors retain their existing keys and little-endian `f32` coordinates. The fixed 56-byte origin value has this layout:

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 8 | ASCII `UQAVORG1` |
| 8 | 16 | Writer database incarnation |
| 24 | 8 | Nonzero writer allocation, little-endian |
| 32 | 8 | Nonzero mutation revision, little-endian |
| 40 | 4 | Vector dimensions, little-endian |
| 44 | 4 | Reserved zero bytes |
| 48 | 8 | Tensor ordinal count, little-endian; at most $2^{32}$ |

The original database incarnation is retained as provenance across data restoration; it is not compared to the current history incarnation. This format is internal and does not enable published DiskANN indexes or change existing provider format negotiation.

## Retained canonical reads

`RetainedDiskANNCanonical` pins one committed/private view of both canonical and origin prefixes without reading the full field. Document reads validate the origin envelope, dimension and complete contiguous ordinal key set. Missing origins for populated documents, missing/extra ordinals, malformed widths, nonfinite coordinates and unsupported provider capabilities fail closed. A document with neither values nor an origin is absent; a zero-count origin represents an explicit empty replacement.

Streaming reads fetch one size-bounded value at a time and reuse one charged decoded-vector buffer. All keys, decoded values and provider buffers use the query allowance, while the fixed source retains its original cancellation and ownership boundary. An error invalidates the caller's partial output. The source can survive mutation, savepoint rollback and closure of its creating handle. Native SQLite's canonical vector families require their own adapter; mapping byte keys into the native DiskANN page family would create a second corpus and is not that adapter.

## Carrier boundary and validation

This change supplies canonical `(document, ordinal, origin, raw coordinates)` observations. It does not collapse ordinals, construct postings, assign scores, convert probabilities or rank results. Thus the [typed carrier boundary](diskann-vector-index.md) remains unchanged: snapshot-valid vector identities must still become distinct document candidates before every visible ordinal contributes to that document's canonical cosine maximum. Origin equality alone establishes neither document support equality nor payload/rank equality. Exact changed-vector merging, base coverage validation, conservative SSI query observations and catalog row/vector atomicity remain integration obligations.

Provider conformance exercises actual issued origins on SQLite Key/Value and redb, including private retained tensors, statement/savepoint undo, independent writers in both commit orders, empty-tensor conflicts, cold reopen, failed evaluation, cancellation, unwinding, failed abort and lost commit replies. A tensor with 16,384 raw coordinate bytes is streamed under an 8,192-byte query allowance; this is an owned-byte check, not timing or process-RSS evidence. Final source-scoped results are recorded in the implementation plan after verification.
