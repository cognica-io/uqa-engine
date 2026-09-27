# DiskANN physical EXPLAIN

Status: static diagnostics implemented; source-scoped verification is recorded in the implementation plan. This document defines static diagnostics within the existing [SQL integration unit](../plans/0014-diskann-vector-index.md#sql-lifecycle-and-planning). Invocation counters and authoritative selected-view current/change populations remain required; static rendering does not complete those obligations.

## Ownership and invocation

Storage projects query-independent `DiskANNIndexStats` from the retained manifest and declared reader configuration. A valid concrete query adds `VectorQueryRoute` in `DiskANNQueryStats`; obtaining index settings never requires an invented query vector. Core carries these facts without a provider or planning dependency. Planner binds diagnostic leaves to relations, applies the existing [work estimator](diskann-physical-planning.md), and renders the result. Engine supplies retained catalog inputs, scalar evaluation and the existing optimizer callback. Execution captures diagnostics before optional execution and passes them to the renderer.

Execution acquires the explained query's existing relation locks before collecting physical metadata: read inputs retain AccessShare and mutation targets retain RowExclusive, including their selected descendants. Identity binding revalidates after a lock wait. This prevents concurrent index replacement between diagnostic selection and the actual invocation. Independent PostgreSQL 18.4 execution confirms those lock modes for static SELECT and UPDATE; the Engine regression requires a peer DROP INDEX to wait until transaction end.

`EXPLAIN ANALYZE` keeps its one nested execution. Static diagnostics are captured before that execution, including for statements that mutate data. They are estimates, not a report of completed work. Actual DiskANN counters must be attached to the original invocation by their execution owners; another search or subtraction of shared counters is prohibited. The existing logical-only renderer remains available to portal declaration, which needs the result descriptor without opening physical index readers.

## Diagnostic contract

TEXT output includes the physical descriptions alongside the existing logical plan. JSON retains the existing result envelope and adds a `Physical Plans` array when a DiskANN leaf is present. Each description identifies its plan path, relation, qualifier, physical field and complete database/table/index/generation identity. Configuration, page layout, stored populations and declared resource limits are separate from `Estimated Work`. Unknown facts are JSON null, never a measured zero.

The work estimate separates PQ lookup and distance work, logical page requests and bytes, provider dispatch rounds, side/change streams, complete-tensor reranking, exact work and resident PQ payload. Logical beam width remains distinct from provider batching and concurrency. These uncalibrated costs do not measure elapsed time, SSD operations or cache occupancy. A zero-norm or overflowing binary32 norm follows Storage's existing exact-route classifier. Residual filtering does not refill the bounded candidate pool.

Vector arguments reuse SQL's existing KNN and calibrated-vector parsers. Planning may evaluate available, context-independent arguments; dynamic or unavailable arguments retain a deferred diagnostic. Such a description can show the selected field's stored settings while leaving route and costs unknown. Known invalid inputs omit physical metadata projection so the original reached execution diagnostic retains precedence. Planning must not invoke volatile routines, substitute a builtin for a shadowing callback, or validate an unreachable search by opening its artifacts.

## Relational placement

Descriptions follow actual relation inputs and the existing qualifier-filter placement rules. A relation alias or positional column alias must resolve to its physical field. Inheritance and partition inputs identify each selected physical member. Operator-join operands retain separate left and right relation bindings. Query-valued children are visited under their lexical scopes; a CTE name must never accidentally resolve to a shadowed physical table.

CTE reachability and deferred/materialized classification are shared SQL semantics used by both Execution and Planner. Static traversal does not materialize CTE rows. View or recursive-output filter specialization must follow the same guarded rewrite as execution. A vector expression in an ordinary scalar projection or a residual that cannot reach a physical retrieval operator is not evidence that an index was selected.

## Preservation argument

Fix a statement, its parameter bindings and retained input view $S$. Let $P$ be its existing executable plan and let $D(P,S)$ be the ordered diagnostic output. The extension returns an annotation $(P,D(P,S))$; erasing the second component returns exactly $P$. No diagnostic is used as an input to retrieval, scoring, fusion, candidate admission, residual filtering or publication. Thus every defined posting, graph, tuple and ranked-result operation continues to consume the same operands in the same order. Their carrier, identities and applicable composition laws are unchanged, including floating-point score order and payload collision precedence.

For a known field and concrete admissible query, Storage supplies the same norm classifier used by search and Planner applies the existing cost formula. For an unavailable argument, $D$ contains no invented coordinate, candidate count or numeric route. Therefore replacing an unknown diagnostic with a known one changes only observations about the plan; it cannot choose a different execution branch. Evaluating diagnostic arguments is restricted to context-independent evaluation without runtime callback execution. Metadata reads retain the invoking controls and do not publish changes or create logical vector read observations.

For ANALYZE, let $E(P,S)$ denote the existing single execution, including its result, error and state effects. Holding the statement's relation identities, capturing $D(P,S)$ before the existing nested call and rendering afterward preserves that one call to $E$. It neither executes a second copy nor obtains work counts from another invocation. Metadata and argument failures still require regression verification for diagnostic ordering; this argument does not waive those tests or establish unimplemented counters.

## Acceptance obligations

- Execute TEXT and JSON through Engine with literal independent expectations for settings, generations, numeric routes and residual results.
- Verify parameters, invalid vector widths and nonfinite inputs, volatile and shadowing callbacks, and one actual ANALYZE execution.
- Exercise aliases, separate join operands, CTE shadowing and reachability, views, scalar subqueries, set operations, commands, inheritance and partition members.
- Verify memory and all persistent providers, retained/private views, cold reopen, original cancellation/resource failures and absence of graph/PQ/origin preparation caused by planning.
- Preserve the serializable no-observation control and ordinary KNN/hybrid results. Run the owning SQL, Planner and Storage tests and affected Engine integration cases.

The selected-provider capability test installs its read guards after capturing the provider view. It verifies the actual SQL diagnostic path on that view; it does not claim that opening an Engine or constructing a retained view performs no executable preparation. Storage's bounded metadata tests and direct review separately cover live manifest capture without graph/PQ/origin/corpus reads.

Executed source-scoped results belong in the implementation plan and PR body. The recorded results establish the tested boundaries; compilation, a metadata-only mock or the presence of a diagnostic field is insufficient evidence of complete SQL acceptance.
