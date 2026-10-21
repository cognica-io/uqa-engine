# Bounded exact vector selection

Storage owns exact vector scoring and candidate selection. A retained exact reader previously allocated one score for every document before selecting a finite K. The common selection owner in `uqa-storage/src/vector_index/query/selection.rs` now supplies the existing bounded heap to exact readers as well as the DiskANN adapter. It does not change a physical index's candidate generation, score transform or retention allowance.

## Selection contract and preservation

For each document $d$, compute the same canonical tensor score $s(d)$ by evaluating its vectors in their stored order and taking the maximum under binary32 `total_cmp`. Scores use the existing cosine arithmetic, including zero vectors, finite-coordinate overflow, signed zero and NaN total ordering. Define $d_1\prec d_2$ when $s(d_1)>s(d_2)$ under that total order, or equal score bits and $d_1<d_2$. The result consists of the first $\min(K,N)$ distinct documents under $\prec$, serialized in document-ID order as before.

After observing $j$ distinct documents, the heap contains precisely the best $\min(K,j)$ documents under $\prec$. The claim is true for the empty prefix. If the heap has fewer than K entries, adding the next document preserves it; otherwise the next document either replaces the unique worst entry or cannot belong to the best K. Induction therefore gives the same final candidates as complete sorting. K equal to zero returns the existing empty owner result after argument validation. Heap comparisons copy already computed score bits and introduce no arithmetic reassociation. The DiskANN adapter's optional membership directory contains only heap entries; its existing fixed-canonical-view precondition ensures repeated candidates have identical complete tensor scores.

Selection retains $O(\min(K,N))$ scores. The heap, optional membership directory and final posting conversion remain under the original read allowance; arbitrary large K is not a constant-memory promise. Admission failure and cancellation return their original typed errors and release the temporary leases. Threshold retrieval remains a complete-population result with its existing storage contract. No candidate refill, approximate route, persistent format or probability calibration is introduced.

## Verification

The owning retained-reader fixture scans 16,384 documents with K values 0, 1 and 8 while allowing only 32KiB of additional selection space, asserts exact IDs and score bits, and requires a typed memory error for an unbounded requested result. Related memory, tensor, DiskANN, canonical-reader, COPY and four-provider regressions pass in the 345-case local composition; the final 38 style-verification cases overlap. Strict six-crate library/test Clippy also passes. Automatic source-matched Linux/macOS qualification remains required before delivery. These are work, memory and output checks, not elapsed-time acceptance.
