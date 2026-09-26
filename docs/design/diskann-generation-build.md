# DiskANN physical generation construction

Storage's `DiskANNBuildInput::write_generation` turns a captured canonical input and its [merged global adjacency](diskann-build-merge.md) into immutable graph pages, PQ metadata/code batches, numeric-side batches and a versioned manifest. It completes the physical build pipeline; public SQL execution, snapshot publication, mutations and query search remain separate owners in the [implementation plan](../plans/0014-diskann-vector-index.md).

## Input and artifact ownership

The input and graph must have identical canonical coverage, node count, original memory/cancellation context and shared encrypted temporary allowance. The sink's generation must match that capture. `DiskANNGenerationOptions` fixes global PQ training, positive code/side batch capacities and the maximum encoded metadata record size. Invalid provenance options, foreign owners and an undersized manifest allowance fail before the first write.

`DiskANNBuildSink` exposes generation identity and bounded writes only. `DiskANNMemoryBuilder` retains artifact bytes under its independent physical-storage allowance. `KeyValueDiskANNStage` writes through the existing native SQLite, SQLite Key/Value or redb generation owner. The algorithm does not acquire provider dependencies, publish a catalog, commit the caller's SQL transaction or discard the caller's stage implicitly. A storage failure preserves its original typed error and leaves the stage with its existing recovery/receipt ownership.

Successful construction returns `DiskANNManifest`, still requiring `DiskANNMemoryBuilder::finish` or `KeyValueDiskANNStage::seal`. A failed construction returns no manifest or readable partial generation; the caller retains staging for recovery and bounded cleanup. Cancellation and memory errors remain errors rather than exact-search substitutions. Canonical snapshot completeness and eventual visibility must still be established by the publication owner.

## Work order and bounded buffers

The fixed work order is global PQ training/encoding, global entry selection, numeric-side encoding and graph-page encoding. Training replays captured navigation records through the existing bounded reservoir; encoding uses the same normalized navigation values and writes code batches in dense node order. The codebook and code workspace are released before entry selection and graph construction. Neither PQ values nor navigation distances become public scores.

Global entry selection shares Vamana's seeded capped-sample centroid implementation. At most 256 node IDs and one centroid are retained. Sampling and full candidate scans read one captured vector at a time, preserve ascending-ID coordinate summation and use the smallest node ID on equal distance. Empty/all-side corpora have no entry or fabricated codebook; singleton graphs retain one node with no neighbors.

Numeric-side batches retain the existing classified `(DocId, ordinal, version)` references. Exact scoring will retrieve their canonical raw values at the selected snapshot. Graph construction visits one adjacency row and raw vector at a time, encodes original coordinate bits and versions, then packs slots into 4 KiB pages or emits the codec's fixed fragments. A packed page retains at most one page payload; a fragmented node retains only its admitted slot and current output page. No whole-corpus vector, edge or identity directory is allocated.

Algorithm-owned dynamic vectors, buffers and decoded records charge the original memory allowance. Existing encrypted captures and adjacency files continue charging the same temporary allowance until dropped. Fixed stack state, opaque allocator/file bookkeeping and provider-owned caches remain outside those explicit buffer bounds; the memory sink's retained artifacts have their own physical-storage allowance. This is a byte-ownership contract, not a process RSS or timing claim.

## Provenance and sealing

The [revision-2 manifest](diskann-generation-format.md#build-provenance) persists work revisions, effective partition/merge/PQ/batch options, counts, entry sample size and assignment/adjacency fingerprints. Revision-1 manifests retain their exact original encoding and validation. The capture-owned `DiskANNBuildCapture::write_generation` additionally writes complete document-origin batches and a revision-3 manifest; the lower-level input-only path retains its revision-2 output. Origin capture, exact-source binding and reopened lookup are specified in [build coverage](diskann-build-coverage.md#durable-origin-artifacts). Graph and existing metadata formats remain unchanged.

Physical sealing checks ordered complete streams, exact batch boundaries, raw node identities and versions, checksums and artifact digests. For revisions 2 and 3 it additionally checks global PQ training options, decoded adjacency count/digest and the reserved successor for every non-singleton node. It does not infer SQL publication authority from a coverage fingerprint or successful physical verification.

## Carrier boundary and evidence

Physical vector identities and adjacency remain separate from the paper's document support, decorated postings and ranked views. Duplicated construction memberships do not invoke `Payload` collision merges; preserving coordinate bytes does not claim exact ANN support. Query integration retains the [explicit visibility, document projection and canonical tensor scoring boundary](diskann-vector-index.md#typed-carrier-boundaries).

Owner tests cover packed and fragmented complete generations, multi-ordinal documents, signed zero, source versions, batch boundaries, empty/all-side/singleton corpora, original errors, ownership mismatch, corruption and resource failure. The independent Python provenance fixture fixes exact manifest bytes without reading Rust results. Shared provider acceptance builds 1,024 32-dimensional vectors, whose raw bytes total 131,072, under a 65,536-byte controlled workspace, seals persistent artifacts, releases encrypted temporary files and all provider owners, then reopens every node, PQ code, side entry and complete document-origin stream. Sparse origin lookup crosses batch boundaries, preserves explicit empty tensors and handles the maximum document ID. Actual provider/platform results belong in the implementation plan; these fixtures make no recall or performance claim.
