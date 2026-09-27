# DiskANN canonical population statistics

The shared count contract and immutable memory-root maintenance provide exact populations without reconstructing them during planning. Persistent-provider maintenance and counts for composed canonical sources remain required by the existing [SQL integration unit](../plans/0014-diskann-vector-index.md#sql-lifecycle-and-planning). Their missing statistics remain unknown; this change does not complete that unit.

## Definitions and ownership

Fix a retained canonical view $S$ and physical generation $g$. For each document $d$, let $n_S(d)$ be its current tensor cardinality, $o_S(d)$ its current canonical origin, and $b_g(d)$ the complete origin captured by the build, or $\bot$ when absent. Define:

$$N(S)=\sum_d n_S(d),\qquad C(S,g)=\sum_d n_S(d)\,\mathbf{1}[o_S(d)\ne b_g(d)].$$

$N$ counts all current ordinals, including zero-norm side vectors. $C$ counts the current ordinals whose document origins are outside this exact build coverage. It does not count mutation operations, journal entries, changed documents or deleted historical ordinals. Empty replacements preserve their origins while contributing zero. Rewriting identical coordinates with a fresh origin contributes the current tensor's complete cardinality to $C$.

Storage owns `DiskANNCanonicalCounts`, mutation arithmetic and the canonical source's `population_counts(g, control)` capability. Its invariants are $0\le C\le N\le 2^{64}-1$. The capability returns exact counts only for the requested canonical/generation association; it may not enumerate the corpus, journal or origin pages to reconstruct them. Unsupported associations return `None`. Metadata capture and prepared readers project the same counts into Core's existing optional population fields. Planner consumes those fields through its existing [cost contract](diskann-physical-planning.md); Engine adds no counting algorithm or provider dependency.

## Memory maintenance and proof

Each immutable memory canonical root retains $(N,C,g)$ with its document and latest-change maps. Before initial construction the empty root has $(N,C)=(0,0)$ and no covered generation. A completed build installs its actual generation and clears the candidate root's change map. Every fork shares the allocator of non-reused origin identities while keeping its own canonical root.

For a replacement of document $d$, let $a=n_S(d)$, let $h$ indicate that its previous origin is already outside $g$'s coverage, and let $z$ be the replacement tensor cardinality. A fresh origin cannot equal $b_g(d)$, so the owner computes:

$$N'=N-a+z,\qquad C'=C-ha+z.$$

All other documents retain their cardinality and origin. Removing $d$'s old summand and inserting its new summand therefore derives exactly $N(S')$ and $C(S',g)$ from the definitions. The memory change map supplies $h$: construction clears it only after capturing every current origin, and each later replacement inserts its fresh version together with the complete tensor. This invariant holds initially and is preserved by every replacement, including repeated writes and zero-cardinality replacements. Checked subtraction and addition reject underflow, overflow or an invalid subset before publication; neither saturation nor wrapping can fabricate a count.

The candidate contains both new maps and new totals. Live replacement occurs only after all validation, memory admission, path copies and control checks succeed. A failed candidate therefore preserves the old vectors, origins and totals together. Cloning or retaining a root preserves its complete tuple; changing a fork cannot change another snapshot's statistics. Restoring a saved root restores the same counts without evaluating SQL again.

A successful rebuild captures this exact immutable source and validates complete build coverage before preparing the replacement reader. For the new generation $g'$, every current origin equals its captured build origin, hence $C(S,g')=0$ while $N(S)$ is unchanged. The source's new counts and covered generation are installed together with that reader. Earlier readers retain their earlier $(S,g)$ association. A failed build never installs this reset, and a request against a different generation cannot borrow the new counts.

## Preservation and resource boundaries

The statistics extend each retained state with a deterministic observation of existing cardinalities and origins. Erasing that observation yields the same documents, coordinate bits, origin identities, physical pages and mutation effects as before. The update equations introduce no posting, payload, score, graph or ranked-result operation. They use exact nonnegative integer cardinalities rather than approximate floating-point counts.

Replacing unknown estimates with these exact inputs changes only the existing cost annotations and diagnostic fields. The established ordered decorated-intersection rule continues to preserve fold order, rounded scores and payload collision precedence; no new optimizer rewrite or candidate-admission rule is introduced. The preservation argument for [physical planning](diskann-physical-planning.md#preservation-argument) therefore applies with $\widehat V=N$ and the known changed population $C$. Approximate traversal, exact threshold behavior, complete-tensor scoring and probability conversion keep their existing definitions.

Memory reads copy fixed-size totals and perform the original owner and invoking control checks. They need no projection allocation or corpus walk. The additional fixed root fields are charged by the existing retained-root allocation. Transparent retained wrappers forward this capability through their existing guards; sources composed from different canonical selections do not inherit an unrelated root's counts. Their required aggregate maintenance remains a separate implementation boundary.

Persistent providers must update totals with actual canonical replacements, preserve evaluated effects when merging disjoint writers, reclassify against the correct generation across publication and late commits, retain private/savepoint/history views, and recover exact counts after reopen. The current field-reference marker and allocation watermarks are not counts. New persisted records require old-writer fencing and format acceptance; this document does not claim that those obligations are implemented.

## Verification obligations

Owner regressions use literal cardinalities for full tensors, identical-coordinate replacements, zero-norm and empty tensors, deletion, repeated writes, failed validation, independent forks, old readers, rebuild and clear. Checked-arithmetic cases reject invalid subsets and overflow. A metadata-only source rejects all document/origin/coordinate enumeration and graph/PQ/origin artifact reads while projecting independently supplied totals. Actual Engine planning and JSON EXPLAIN verify current roots, private writes and savepoint/transaction rollback. Source-scoped executed results are recorded in the implementation plan.
