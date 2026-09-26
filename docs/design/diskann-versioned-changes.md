# DiskANN versioned changes

Status: The Key/Value change journal and retained document cursor merged in PR #174. Native SQLite changes and their retained cursor are implemented for review. Storage-issued build coverage, covered-version retirement, paged query merging and public DiskANN routing remain pending in the [implementation plan](../plans/0014-diskann-vector-index.md).

## Ownership and atomicity

Storage's `KeyValueDiskANNCanonical::replace` writes existing canonical coordinates, the complete tensor's origin and one immutable change record in the same evaluated MVCC batch. The change contains only origin/shape metadata; it does not duplicate coordinates. Empty replacements also create a change. Common MVCC supplies the actual publishing writer and non-reused mutation revision, preserves statement/savepoint rollback and seals evaluated bytes for receipt-safe retry. Engine, dependency direction and Cargo features are unchanged.

`SQLiteDiskANNCanonical::replace` uses the same common mutation scope and codecs with native object-owned rows. Coordinates remain in `_vectors`, origins remain in their existing family, and the journal accompanies both in one evaluated native publication. An acknowledgement failure after durable commit retains that outcome and completing the original attempt keeps the same origin and single change record.

Independent documents retain distinct canonical guards and change keys. A same-document writer still validates its original canonical precondition. Change keys include the complete mutation identity, so a subsequent replacement receives a different key; eventual deletion of an older covered key cannot overwrite a later writer's change. This avoids making ordinary writes depend on a shared index header or a mutable per-document maintenance slot. Writer allocation order is never interpreted as commit order.

## Representation

The Key/Value namespace is `\0uqa-diskann-changes-v1\0` followed by the existing canonical vector field prefix. Each fixed 40-byte suffix is a `DiskANNChangeIdentity`:

| Offset | Bytes | Meaning |
| --- | --- | --- |
| 0 | 8 | Document ID, big endian |
| 8 | 16 | Original writer history identity |
| 24 | 8 | Original writer allocation, big endian |
| 32 | 8 | Mutation revision, big endian |

The value is the existing fixed 56-byte [canonical origin envelope](diskann-canonical-origins.md#keyvalue-format), including dimensions and complete ordinal count. Unknown widths, zero allocation/revision and inconsistent current origin/count fail. History identity is preserved as data; this key format does not depend on restored record commit sequences.

### Native representation

Native mapping 12 adds object-owned family 59, `_uqa_mvcc_native_vector_changes(table_name, field, identity, origin)`. The `identity` BLOB is the same 40-byte change identity and `origin` is the same 56-byte envelope. Native record keys contain the stable table object/generation, field and ordered identity BLOB. The document remains inside that shared identity; native document IDs retain their existing nonnegative signed-64-bit range. Native row reads cap all four variable-width fields and their exact envelope before materialization.

Upgrade from mapping 11 preserves raw values, origins, the data namespace and existing record/receipt history while creating an empty journal. Earlier supported mappings also advance atomically. Failed upgrades restore the previous schema and guards. The current mapping rejects a predecessor writer's version check; missing tables/guards and changed layouts fail on reopen. Preserved origin-only data still needs explicit adoption before public query routing, and an empty journal is not a coverage claim.

## Retained enumeration

`RetainedDiskANNCanonical` retains vector, origin and change prefixes through one fixed provider read. `next_change_after` seeks the next document's journal range, validates that document's complete canonical ordinal set, and probes the exact key for its current origin. It returns that identity once, including an explicit empty replacement. Superseded mutations and deleted documents do not contribute. A cursor seeks past the entire preceding document range, so repeated historical mutations do not require a resident deduplication map or a scan of their values. The terminal document ID remains valid.

`RetainedSQLiteDiskANNCanonical` applies the same selection to one captured native object/field view. Its key cursor skips physical tombstones, decodes the shared identity and seeks past all historical identities for the preceding document. Current journal payloads must match the complete canonical origin/count on that same view; malformed keys, mismatched scope and oversized current rows fail. Original-source and invoking controls also apply to absent owners and terminal cursors.

Current change metadata must equal the retained canonical origin/count. Encoded point reads are capped before materialization. Original-source and invoking controls remain active on empty cursors and between seeks. Duplicate/out-of-order journal callbacks, missing change-value callbacks and suppressed consumer errors fail; temporary cursor buffers are released on failure. Callers obtain raw values and complete tensor scores through this same canonical source and the [canonical scorer](diskann-canonical-scoring.md).

Table and field cleanup removes the journal with canonical data. Rename and legacy-name migration move its namespace with the original identities. Retained readers keep their original journal and canonical boundary across those operations and undo. Ordinary non-DiskANN replacement invalidates the canonical origin, so populated unstamped data still fails instead of accepting a stale journal entry.

## Coverage and retention boundaries

This journal enumerates currently recorded replacements; it is not a build-coverage token. Publication must still prove which actual committed/private origins the selected immutable generation covers, and must preserve late commits excluded from its capture. A missing journal entry cannot itself establish that a generation covers a document. Existing origin-only data requires explicit adoption, and incapable older writers require negotiation before public enablement.

One physical change record is retained per successful mutation until authorized covered/obsolete-version retirement is implemented. Bounded reads do not establish bounded persistent retention. Retirement must use the actual selected generation and coverage authority, respect retained snapshots and unresolved publication receipts, and never remove a possibly uncovered current version. Journal merge, byte/count maintenance and that retirement protocol remain required gates before normal DiskANN queries are enabled; this implementation makes no whole-runtime acceptance claim.

## Verification

Independent fixed bytes verify the key representation and document-range cursor. Actual SQLite Key/Value and redb conformance exercises ordered current versions, empty tensors, terminal IDs, a retained rolled-back branch, later commit visibility and cold reopen. A 259-record journal contains more than 8,192 bytes of identity/value data while its current-document cursor uses an 8,192-byte query allowance; this is an owned-workspace assertion, not a timing or RSS measurement. Other cases cover malformed keys/current values, callback faults, cancellation/quota release, table/column lifecycle and lost commit replies without a second journal entry. Source-scoped results and review evidence are recorded in the plan and PR.

Native tests exercise the same 259-record/8,192-byte boundary, actual savepoint origins, late commits, disjoint and same-document writers, canonical invalidation and field cleanup. Plain, encrypted, compressed and combined files preserve journal identities after reopen. Separate cases cover malformed keys/rows, cancellation, table/column lifecycle, durable acknowledgement retry and upgrades preserving populated predecessor origins without inventing journal entries. A literal 132-byte row fixture independently checks the encoded native envelope for two five-byte names and the shared identity/origin sizes.
