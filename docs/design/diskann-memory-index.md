# DiskANN writable memory indexes

Storage's `DiskANNMemoryIndex` implements writable `VectorIndex` behavior over the existing sealed memory pages and [retained document search](diskann-document-search.md). It supplies the memory copy-on-write boundary required by Engine rollback adapters; it does not register DiskANN with SQL or implement persistent-provider transaction scheduling. The [implementation plan](../plans/0014-diskann-vector-index.md) retains those separate delivery gates.

## Owned state

The live state is $(B,D,J)$: one immutable prepared physical generation $B$, a canonical document map $D$, and a current-change map $J$. Each canonical entry owns its complete tensor, including empty replacements, and its original mutation identity. The maps use Core's `BudgetedSharedMap`; shared entries and unchanged subtrees keep their original reservations. A read-only snapshot retains the prepared generation and both immutable roots. A writable snapshot shares those roots initially and may subsequently replace its own state independently.

Each memory index creates a private nonzero storage incarnation and one mutation lineage. Every writable branch shares its monotonic identity allocator; ordinary mutation revisions and physical generation IDs are never reused after rollback, discarded candidates or failed builds. These identities establish equality of actual owned memory values, not SQL-visible transaction IDs, commit ordering or persistent-provider receipts. The allocator fails on exhaustion rather than wrapping. A new independent index has another incarnation.

`DiskANNMemoryOptions` explicitly supplies effective DiskANN parameters, reader/cache limits, partition settings, merge capacity and generation/PQ settings. The owner also retains the invoking `StorageReadControl` and a shared `DiskANNTemporaryBudget`. These settings never increase the caller's allowance. The memory provider retains its encoded records in RAM; it makes no SSD, out-of-core storage or process-RSS claim.

## Mutations and snapshots

Replacing document $d$ first validates every coordinate and tensor cardinality, admits the caller-owned buffers' complete capacities, and prepares a new canonical entry and current-change entry. The candidate is $D'=D[d\mapsto(v,X)]$ and $J'=J[d\mapsto v]$, where $v$ is a fresh owner-issued version and $X$ is the complete tensor. Both new roots and their retained-query wrapper must be admitted before the live state changes. Failure leaves $(B,D,J)$ unchanged; skipped identity numbers are intentional.

Single-vector and tensor replacement use this same operation. Deleting a nonempty document writes an empty replacement that masks every old ordinal. Deleting an absent or already empty document is a no-op after checking original controls. Mutations never update or rebuild $B$ and do not reopen its PQ or complete-origin readers. Query-local candidate state remains separate from the shared immutable preparation.

Read-only snapshots keep $(B,D,J)$ even after a live mutation, clear, rebuild or source drop. Writable snapshots add one admitted handle and share their initial immutable roots, prepared readers, allocator and original resource limits. A later replacement copies only the changed ordered-map paths and adopts the new tensor; it does not clone unchanged vector buffers. Restoring an older writable root rolls back its data and generation while retaining the shared allocator's non-reuse guarantee.

## Construction and clear

Creation constructs and seals a real empty physical generation. There is no missing-head exact fallback. Explicit `initialize` captures the current canonical source and reuses the existing encrypted temporary input, partition construction, global merge, page/PQ writing and complete artifact sealing. The replacement reader validates that sealed generation before publication. Only then does the live index select the new generation and an empty change map; the captured complete origins establish coverage of its canonical source.

`clear` follows the same atomic publication boundary with empty canonical and change roots and a fresh empty generation. Earlier readers keep their old pages, raw vectors and scores. All retained old generations, new physical records, canonical tensors, map paths, resident codes and build/query workspace share the original memory allowance. Temporary encrypted files share the original temporary allowance and are removed when preparation returns or fails. A failed rebuild or clear preserves the old live root and changes.

Automatic rebuild scheduling and persistent-provider catalog publication remain separate lifecycle work. This explicit memory operation neither performs SQL effects nor replays caller expressions.

## Preservation argument

Assume $B$ completely covers a previous canonical root and $J$ names every subsequent current replacement. A successful replacement changes exactly $d$ in both $D$ and $J$. The [document search](diskann-document-search.md#candidate-support-and-scores) compares base origins against $D$: the old version of $d$ is masked, and the exact change stream contributes the complete score of $X$ if nonempty. Every other canonical entry and change remains unchanged. Thus mutation preserves full tensor scoring and result completeness under the same ANN candidate contract; PQ values still do not become result scores.

A sealed rebuild establishes complete origin coverage for the exact captured $D$, so clearing $J$ removes only redundant change records. Rebuilding can change approximate candidate membership, as declared by the existing algorithm; it does not change canonical scores, exact threshold results or document-level cardinality. Clear establishes the invariant for the empty corpus directly.

Immutable roots ensure a retained reader continues to evaluate exactly its captured $(B,D,J)$. New root publication cannot modify that reader's entries or page leases. Fresh versions distinguish equal coordinate values written on different branches, including after rollback. Existing filter, threshold, tensor maximum and probability/fusion operations therefore receive the same raw-score carrier and retain their established meanings. No new algebraic operator or probability conversion is introduced.

## Verification boundary

Owning Storage tests exercise empty construction, complete tensors, zero-vector side entries, late replacements/deletions, explicit rebuild and clear, original readers, independent writable branches and restored savepoint roots. Literal axis-vector results establish raw scores; exact threshold results are invariant across rebuild. A 128 KiB canonical fixture checks unchanged allocation addresses and bounded additional retained bytes for a single replacement, without timing or RSS measurement.

Failure checks exhaust the original memory allowance, deny temporary storage, reject malformed coordinates and exhaust the identity allocator. They verify unchanged live results/manifests, retained changes after failed rebuild, candidate cleanup and final-reader release. Nested snapshots reject a fresh larger-budget bypass and preserve original cancellation. Source-scoped results and automatic checks are recorded in the plan and PR; public Engine rollback, persistent live adapters, SQL and binding artifacts remain unverified here.
