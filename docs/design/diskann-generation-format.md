# DiskANN generation metadata format

This document specifies metadata codecs in `uqa-storage::diskann_index::format`: manifest envelopes 1 and 2, with codebook, code and side envelopes remaining at revision 1. The [DiskANN design](diskann-vector-index.md#node-and-page-encoding) defines node/page bytes, canonical scores and ownership; the [implementation plan](../plans/0014-diskann-vector-index.md) tracks runtime delivery. Codecs validate physical records; [generation construction](diskann-generation-build.md) and provider sealing consume them without granting SQL publication or MVCC visibility. All fields below use little-endian integers and IEEE-754 bit patterns. Offset ranges exclude their end.

## Record envelope

Manifests, codebooks, code batches and numeric side batches use the same 96-byte envelope with distinct magic values. Graph pages retain their separate 4 KiB format. No native Rust structure layout is persisted.

| Byte offsets | Contents |
| --- | --- |
| 0–8 | Magic: `UQADNMF\0`, `UQADNPQ\0`, `UQADNCD\0` or `UQADNSD\0` |
| 8–12, 12–16 | Record revision and header size 96 (`u32` each); manifests accept 1 or 2, other metadata accepts 1 |
| 16–32 | Persistent data incarnation |
| 32–40, 40–48, 48–56 | Table incarnation, index incarnation, generation (`u64` each) |
| 56–64 | Exact body length (`u64`) |
| 64–96 | SHA-256 of bytes 0–64 followed by the complete body |
| 96–end | Record-specific body |

Decode requires the caller's expected generation and rejects unknown magic/revisions, wrong header sizes, mismatched identities/lengths and checksums. Hashing checks cancellation in bounded chunks. Encoding admits the complete requested record buffer before allocation; borrowed manifest/batch decoding allocates no variable-size buffer. Checksums detect damaged records and bind their metadata, but are not authentication. Providers must retain their existing encryption and affinity rules and bound record reads before materializing bytes.

## Canonical input fingerprint

`DiskANNCoverageBuilder` consumes all selected canonical vectors in increasing `(DocId, ordinal)` order, with each document starting at ordinal zero and continuing without gaps. It includes graph and numeric-side vectors; empty tensors contribute no vector. Each accepted entry contains its original raw `f32` bits and `DiskANNVectorVersion`. Input validation rejects nonfinite raw values; cancellation or invalid input preserves the previously accepted hash prefix. The builder uses constant hash state and no corpus-sized buffer.

The hash input is the byte string `UQA DiskANN canonical coverage\0\x01`, the 40-byte generation identity, the dimension count (`u32`), followed by each entry's `DocId` (`u64`), ordinal (`u32`), 16-byte writer history, writer allocation (`u64`), mutation revision (`u64`), and raw coordinate bits in order. The final total vector count (`u64`) terminates the stream. Signed zero and different source origins produce different fingerprints even when their cosine geometry is identical.

`DiskANNBuildCoverage` is an identity for a selected input stream, not a commit watermark, a membership oracle or proof that the caller supplied a complete snapshot. The build owner must retain one canonical snapshot and verify the selected stream and version mappings during sealing/publication. Candidate visibility and covered-change reclamation require those mappings and existing MVCC boundaries; they cannot be inferred from this digest, an allocation maximum or a process-local private revision. This distinction remains necessary for private writes, savepoint branches, late commits and restored transaction histories.

## Manifest

Manifest revision 1 has a 288-byte body and retains its original 384-byte encoding. Revision 2 appends 256 bytes of build provenance, making its body 544 bytes and complete record 640 bytes. The envelope revision and exact length must agree. Both use the same initial body below; node/page and index-format revisions remain 1. Offsets in this and the following tables are relative to the body, after the common envelope.

| Body offsets | Contents |
| --- | --- |
| 0–32 | Eight `u32` values: dimensions, algorithm revision, navigation revision, score revision, graph-page revision, page bytes, node-header bytes, page-header bytes |
| 32–112 | Ten `u64` values: maximum degree, build list, search list, alpha `f64` bits, beam width, PQ bytes, seed, graph node count, numeric-side count, entry node |
| 112–144 | Canonical input fingerprint |
| 144–152, 152–160 | Total selected vector count and graph page count (`u64` each) |
| 160–192, 192–224, 224–256, 256–288 | Codebook, code stream, numeric-side stream and graph digests |

Navigation revision 1 means the existing normalized `f64` squared-Euclidean navigation; score revision 1 means canonical raw-vector `f32` cosine and existing tensor reduction. These are explicit discriminators, not restore-time defaults. The effective parameters pass the same `DiskANNIndexParams` validator as catalog configuration. Page constants and computed page count must match the node layout. Coverage's generation, dimensions and count must match the manifest, including checked addition of graph and side counts. A nonempty graph has an in-range entry; an empty graph stores `u64::MAX` and has no entry or codebook. Empty artifacts require SHA-256 of the empty byte string.

The codebook digest covers its entire encoded record. The code-stream digest covers all raw code bytes in ascending node order. The side-stream digest covers all 48-byte entries in their canonical order. The graph digest covers the ordered 32-byte checksums of all graph pages. Thus code and side batch boundaries can change without changing their logical stream digests. The provider/seal owner must verify complete ordered streams, cross-batch boundaries and all artifact digests before publication; a manifest codec alone does not inspect those records. Open must also compare the stored parameters and dimensions with their owning catalog contract.

### Build provenance

Revision 2 requires `DiskANNBuildProvenance`. The following offsets are relative to its 256-byte suffix, starting at manifest body offset 288. The first 192 bytes contain 24 `u64` words; decoders reject values outside the owning options' narrower integer ranges.

| Suffix offsets | Contents |
| --- | --- |
| 0–16 | Build work-order revision and partition work-order revision, both 1 |
| 16–32 | Maximum partition points and maximum recursion depth |
| 32–64 | Coarse PQ maximum samples, maximum iterations, requested centroids and seed |
| 64–104 | Partition count, membership count, candidate-edge count, actual maximum depth and actual maximum leaf size |
| 104–136 | Merge work-order revision 1, sort-buffer records, merge-pass count and final edge count |
| 136–168 | Global PQ maximum samples, maximum iterations, requested centroids and seed |
| 168–192 | Code-batch node capacity, side-batch entry capacity and entry sample count |
| 192–224 | Partition assignment SHA-256 |
| 224–256 | Global adjacency SHA-256 |

Validation uses the original partition/PQ option validators, rejects unknown work revisions and impossible empty/singleton/count/depth/degree relationships, and derives the exact two-way pass count from candidate edges and sort capacity. Global PQ seed matches the index seed. Entry sample count is exactly `min(nodes, 256)`; its seeded Floyd rule, ascending summation and smallest-ID tie policy are shared with admitted Vamana construction. The restored codebook must report these same global PQ training options. Code/side capacities are positive and sealing checks actual batch boundaries against them, including the final short batch.

The adjacency hash is SHA-256 over `UQA DiskANN adjacency\0\x01`, the canonical coverage digest, node count (`u64`), effective degree `min(max_degree, nodes - 1)` with saturating empty subtraction (`u64`), and every dense node's row. Each row contains neighbor count (`u64`) followed by effective-degree neighbor slots (`u64` each), with sorted unique neighbors and zero unused slots. Physical sealing recomputes this hash and the edge count from decoded pages. Every non-singleton node must include `(node + 1) mod nodes`; the reserved cycle establishes graph reachability without a resident visited bitmap.

Assignment and adjacency fingerprints bind construction records; they do not certify snapshot completeness, rerun centroid selection or turn the physical seal into an MVCC publication permit. Revision-1 manifests have no provenance and retain their original physical validation. Writers select revision 2 only when provenance is present; readers continue accepting both exact encodings.

## Product quantization codebook

| Body offsets | Contents |
| --- | --- |
| 0–4, 4–8, 8–10, 10–12 | Dimensions, chunk count (`u32`), actual centroid count (`u16`), reserved zero |
| 12–24 | Scalar representation, PQ codec and training revisions (`u32`, all initially 1) |
| 24–28, 28–32, 32–34, 34–36 | Maximum samples, maximum iterations (`u32`), requested centroids (`u16`), reserved zero |
| 36–40, 40–48, 48–56, 56–64 | Sampled count (`u32`), seed, observed count, scalar count (`u64`) |
| 64 onward | Exactly $m+1$ coordinate offsets (`u32`), then $CD$ scalar `f64` bit patterns |

Offsets use the PQ owner's nonempty contiguous chunk partition, including all remainder coordinates. Scalars are chunk-major, centroid-major, then coordinate order within a chunk. The actual centroid count $C$ equals the lesser of requested centroids and admitted samples; samples equal the lesser of observations and the sample limit. Observations must equal the manifest's graph node count. Dimensions, PQ width and seed match the manifest. Unknown scalar/codec/training revisions, inconsistent counts, offsets, reserved bytes or exact lengths fail before centroid allocation. An empty graph cannot restore a fake codebook.

Centroids retain their exact `f64` bits and allocation allowance after decoding. A conservative finite coordinate envelope of $[-2,2]$ rejects corrupt magnitudes that could overflow distance arithmetic: unit-vector coordinates and their means, even with sequential binary64 rounding across the supported `u32` dimension/sample counts, remain inside it. This is a numeric safety check, not a claim that the decoder has rerun training or certified every centroid's provenance. The stored record digest binds the actual codebook, while independent training fixtures verify the trainer separately. Restored owners retain no past query's cancellation signal.

## Code batches

Codebook encoding/decoding returns a `DiskANNQuantizationIdentity` bound to the generation, dimensions, PQ width, actual centroid count, observed node count and complete codebook digest. Code batch APIs require that identity rather than a caller-invented collection of scalar layout values.

| Body offsets | Contents |
| --- | --- |
| 0–16 | Dimensions, PQ bytes, centroid count, PQ codec revision (`u32` each) |
| 16–24, 24–32, 32–40 | Total nodes, first node, batch node count (`u64` each) |
| 40–72 | Codebook digest |
| 72 onward | Exactly batch-count times PQ-width bytes, in dense node order |

A batch is nonempty and fits entirely within the selected generation. Every byte label is below the actual centroid count. The borrowed decoder checks the expected first node, complete metadata, codebook digest and exact payload shape before exposing per-node slices. It rejects overflow, partial codes, extra records and labels for nonexistent centroids. It does not reinterpret code distances as payload scores or probabilities. Missing, duplicate or reordered batches remain a reader-level completeness obligation.

## Numeric side batches

The 32-byte body prefix stores dimensions (`u32`), classification revision 1 (`u32`), total side records, first record and batch count (`u64` each). Each following 48-byte entry stores `DocId` (`u64`), ordinal (`u32`), classification (`u32`), writer history (16 bytes), writer allocation (`u64`) and mutation revision (`u64`). Classification 1 is a zero canonical norm, including squared-sum underflow; 2 is a nonfinite norm derived from finite raw coordinates. Other tags, invalid origins, duplicates and unordered document/ordinal keys fail. Side ordinals need not be contiguous because navigable ordinals live in graph pages.

`DiskANNSideEntry::from_raw` checks the same canonical norm classification as navigation and rejects ordinary navigable vectors and nonfinite raw input. Batches retain logical references, not a second copy of every raw vector. The exact candidate owner must fetch the selected canonical raw value, validate origin and ordinal under its snapshot, and preserve canonical scoring. A stored tag does not establish current visibility or replace that validation. Encoded and decoded batches are independently bounded; the reader must check order and coverage across batch boundaries.

## Independent evidence

The [compact metadata fixture](../../crates/uqa-storage/tests/fixtures/diskann/README.md#generation-metadata-bytes) derives byte layouts from Python `struct` and SHA-256, rational PQ expectations, a simple fixed graph, and explicit raw-vector bits. Storage tests compare exact digests and header bytes, reconstruct codebooks without changing codes or lookup values, reject rechecksummed invalid inner metadata, preserve empty/all-side states, and exercise failure cleanup and cancellation. These codec results are separate from provider, MVCC, reader and runtime DiskANN acceptance.
