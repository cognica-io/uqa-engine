# DiskANN build coverage

Status: Retained build capture and exact source membership are implemented for review. Provider/index binding, atomic generation publication and covered/obsolete journal retirement remain required in the [implementation plan](../plans/0014-diskann-vector-index.md). This contract does not enable public DiskANN queries.

## Capture and membership

Storage's `DiskANNBuildCapture<S>` owns both the retained `DiskANNCanonicalRead` source and the encrypted `DiskANNBuildInput` produced from it. The concrete source keeps the original committed/private snapshot, canonical scope, leases and read controls. Construction consumes the existing ordered canonical stream; it does not open a fresh source or collect a corpus-sized origin directory.

After construction, `finish` checks the manifest's complete capture fingerprint and separate navigation/side counts against that input. A mismatch fails and releases the capture. Successful completion releases encrypted temporary input and returns `DiskANNCanonicalCoverage<S>`, keeping the selected source and original build control. Physical artifact sealing is a separate prerequisite: matching metadata alone does not certify stored graph pages or publish a generation.

For a document $d$ and original vector version $v$, membership is $C_S(d,v) = [\operatorname{origin}_S(d)=v]$ on the retained source $S$. The source validates the complete canonical tensor before returning its origin. An explicit empty replacement has an origin and can be covered; absence cannot. An earlier writer allocation, a later live read or a missing journal entry never establishes membership. The token has no public constructor from a manifest or digest.

This distinction matters even without hash collisions: a source with only empty tensors and an absent source emit the same zero-vector capture fingerprint, while selecting different origins. The fingerprint binds emitted build input; the retained source answers membership. A writer allocated before capture but committed afterward remains outside the earlier source. A privately captured replacement remains the same selected version after savepoint rollback. Source, build and invoking cancellation remain active during membership checks, including absent documents.

## Provider and publication ownership

The generic capture preserves its concrete source type. A subsequent provider publication/retirement operation must validate that source's actual table/field and stable owner against the destination index, require a physically sealed matching generation, and validate the expected current definition/generation. `source()` exists for those owner checks; caller-provided generation numbers or equal vector contents do not establish provider binding. SQLite and Key/Value adapters retain their existing source layouts and dependency direction; Engine does not compute coverage.

Covered immutable change keys may be retired only through the same authorized publication transaction that installs the generation. Every change outside the selected source, including a late commit or later private revision, must remain available. Obsolete-version reclamation also needs its own proof against the publishing view, retained snapshots and unresolved receipts. These operations are not implemented by the membership token and are not implied by successful capture.

The token is process-owned evidence. Losing its retained source before publication invalidates the unfinished authority; a manifest hash cannot reconstruct it. Existing staged-generation recovery must preserve the previously published index and discard or otherwise resolve the unpublished candidate under its ownership rules. Normal open must eventually validate the published generation and outstanding journal together; it must not rebuild an index as a substitute for missing publication evidence.

## Verification

Storage tests distinguish equal zero-vector fingerprints with different empty-origin membership, reject foreign capture metadata and changed navigation/side classification, and exercise source/build/query cancellation and temporary-file release. Actual SQLite Key/Value and redb conformance constructs and physically seals generations from real canonical origins, then checks late commits, replacements, private undo and empty tensors. Native SQLite tests keep both committed and undone sources after closing their original plain/encrypted/compressed connections. Verification uses bounded owned workspace, with no timing or RSS acceptance claim.
