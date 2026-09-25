# DiskANN canonical document scoring

Status: Implemented for review on top of the [retained canonical sources](diskann-canonical-origins.md). Physical graph/side/change integration, coverage authority, publication and public DiskANN SQL remain incomplete in the [implementation plan](../plans/0014-diskann-vector-index.md).

## Ownership and observations

`uqa-storage::diskann_index::DiskANNCanonicalScorer` borrows one `DiskANNCanonicalRead`, a validated query and the invoking control. Storage owns scoring, selection and posting construction. Providers retain canonical visibility, coordinate buffers and their original controls. Engine gains no numerical or retrieval algorithm, and no dependency or Cargo feature changes.

For a vector-bearing document $d$ with visible tensor ordinals $0,\ldots,n_d-1$, its raw score is

$$
s_q(d)=\underset{0\le i<n_d}{\operatorname{max}_{\mathrm{total\_cmp}}}\;\operatorname{cos}_{f32}(q,v_{d,i}).
$$

The reduction uses the existing exact index's `f32::total_cmp` order, including its IEEE edge values. An absent document or an explicit empty tensor has no score. A `DiskANNDocumentScore` carries the document, original mutation version, complete ordinal count and raw cosine. It is not a navigation distance, probability, payload collision merge or claim about approximate membership. Every visible ordinal contributes before the document is selected; encountering a non-best tensor element cannot lower its final score.

Canonical cosine retains the established sequential `f32` dot and squared-norm reductions, square roots and denominator multiplication. Ordinary and controlled calls share those operations. Controlled scoring checks both source and invoking cancellation between coordinate chunks without resetting or regrouping the sums. Zero norms yield positive zero; underflow and derived overflow retain the established result, including NaN. Inputs still require the declared width and finite coordinates.

## Candidate validation

`score_candidate` compares the physical candidate's origin with the current retained document origin. A stale or absent origin is masked. A matching origin must contain the requested ordinal; an impossible ordinal, including an ordinal in an empty replacement, is corruption and fails. An admitted candidate then receives the complete canonical tensor score. Provider changes to the returned origin, ordinal order or dimensions fail; the first callback error survives even if a provider suppresses it. A successful candidate score does not validate a graph's generation affinity, base coverage or the completeness of an ANN result.

`visit_scores` emits each vector-bearing document once in ascending document order. It validates cursor progress and requires every enumerated document to have an origin. Failed reads, decoding, cancellation or consumer callbacks invalidate all partial output. `check_control` on the retained source also covers zero-k and empty queries that perform no physical reads.

## Exact selection and memory

`search_exact_knn` scans the selected canonical corpus and keeps at most $k$ unique document scores in Core's charged binary heap. The worst selected score is at the root; a better score or smaller document ID on a score tie replaces it. The heap grows with actual selected documents, so an empty corpus with an enormous requested $k$ does not reserve that request. This path needs no resident map of every scanned document. Exact threshold queries apply the existing finite-threshold check and ordinary `score >= threshold` comparison after complete tensor reduction; NaN does not pass that comparison.

Result construction reuses Storage's controlled posting helper. Scores widen from `f32` to `f64` without probability conversion, and postings are stored in ascending document order. Heap capacity, score buffers and posting construction share the invoking allowance. Threshold output is proportional to matches and fails when that output cannot fit; it does not silently truncate. The returned `PostingList` keeps the existing caller-owned result boundary after controlled construction.

These exact methods supply explicit exact-threshold and numeric-edge routing. Ordinary ANN queries still require validated generation selection, side/change merging, distinct document candidates and adaptive completeness. Missing coverage or a failed graph read must not silently select this exact path.

## Verification

The existing independently generated [score fixture](../../crates/uqa-storage/tests/fixtures/diskann/README.md#independent-expectations) supplies exact raw bits, a tensor whose second ordinal is best, empty tensors, ties and expected document ordering. The new scorer executes those expectations for all requested k values and finite thresholds. Numeric-edge cases also compare the established exact index's observations without asserting portable NaN payload bits.

Hand-checkable padded `(3,4)` vectors preserve cosine `3/5` across coordinate chunks. A chunk-boundary dot sequence $2^{24},1,-2^{24}$ must reduce to zero under sequential `f32` rounding; regrouping chunks would change that result. Deterministic injected checks cancel inside a long vector. Other cases cover stale/impossible candidates, suppressed callback errors, changing origins, broken cursors, original/query cancellation and released failure workspace.

Actual retained Key/Value and native SQLite sources exercise complete tensor scores, exact top-k and thresholds, including cold reopen in all SQLite file modes and redb. A separate 4,096-document source verifies top-three selection under a 2,048-byte query allowance; this bounds selection workspace, not source storage or process RSS. Provider tensors containing 16,384 coordinate bytes retain their 8,192-byte invoking allowance. Source-scoped pass counts and review evidence are recorded in the plan and PR; no performance or public ANN recall result is claimed.
