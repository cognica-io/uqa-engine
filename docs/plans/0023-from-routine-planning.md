# Functions in FROM: planning and diagnostics

Status: Active

Issue [#1068](https://github.com/cognica-io/uqa-engine/issues/1068) retains two requirements after PR #1050: PostgreSQL 18 planning of eligible SQL set functions beyond simple constant-input projections, and SQL/PL/pgSQL diagnostic CONTEXT. The original issue report and acceptance criteria remain authoritative. This work starts from `e38848f8a` and preserves the fixes already delivered for unknown inputs, record definitions, polymorphic results, result width, scalar aliases and lazy volatile calls.

## Work and acceptance

- [x] Capture independent Docker PostgreSQL 18 evidence for the remaining query shapes, row-dependent inputs and diagnostic contexts, including unfetched calls, nested routines and repeated execution.
- [ ] Extend SQL-owned typed routine expansion and Planner integration without capturing caller columns in an inner scope, duplicating volatile evaluation or crossing routine security/settings boundaries. Preserve result coercion, row multiplicity and ordering, and use the existing execution owners for expanded queries.
- [ ] Carry structured diagnostic context through SQL errors, routine analysis, planner inlining and SQL/PL/pgSQL execution, preserving SQLSTATE, primary message, DETAIL, HINT and diagnostic order through the public adapters.
- [ ] Add focused owner tests and PostgreSQL transcript replay through all four providers, including persistent reopening. Record the preservation argument and final evidence here, and update the manual and HISTORY for the delivered behavior.
- [ ] Review the final change, run formatting and ownership/dependency checks, and verify actual automatic Linux/macOS bounded-work, memory and output regression results for the delivered product source. Qualify FDW once before merge if the final source fingerprint requires it.
- [ ] Update the PR and issue records, merge, confirm #1068 closed, return to main and remove the completed branch.

## Ownership and dependencies

The reviewed Cargo manifests and `scripts/workspace-dependency-policy.json` place routine declarations, typed body analysis, result assignment and SQL diagnostics in `uqa-sql`; caller-plan simplification in `uqa-planner`; and invocation, procedural statement execution and diagnostic-frame lifetimes in `uqa-execution`. Engine supplies the existing catalog, session and evaluation adapters. These owners have no optional feature gate for this behavior; Engine's Arrow, DuckDB, Nori and Kuromoji features remain independent. No reverse dependency or new test executable is needed.

## Progress

Docker PostgreSQL 18.4 supplied 130 statements covering query shapes, explicit and implicit lateral calls, colliding inner/outer qualifiers and caller CTE names, unused and conditional arguments, collation, nested diagnostics, exception handling and rethrows. All 123 statements matched on all four providers, with 30 retained cases passing after each persistent provider reopened, and the same transcript passed through the PostgreSQL TCP protocol. The predecessor 376-statement transcript and its 27 reopen cases also passed on all four providers. Related execution checks, including the manual SQL harness, now pass 246 of 247 cases after correcting an overly broad simple-expression plan rule. The remaining overload test identified FROM binding before scalar-subquery argument analysis; selection now consumes the analyzed argument types, with its final replay pending. A separate review correction preserves the lateral flag of unchanged function sources. An independent PostgreSQL counter probe confirmed that simple assignments plan the immutable constant once under all three cache modes, while ordinary SELECT INTO retains counts 1/6/32 for force-generic/auto/force-custom. Focused SQL, Planner and Execution owner suites passed 89, 99 and 20 tests; CLI and wire diagnostic unit checks passed three tests. The regression-inventory verifier passed all 30 tests. Review also removed an overly broad exclusion of scalar-domain results; seven additional oracle statements cover planning errors, valid rows and deferred domain checks. The completed 130-case/32-reopen replay and final owner tests remain pending. Strict Clippy passed for the six affected crates before the final SQL review corrections; its final SQL rerun is pending. Formatting, dependency and Engine-capability checks have passed. Automatic CI acceptance remains pending, and the draft PR is being pushed while the final focused replay completes.

The post-merge controlled timing run [39093923834](https://github.com/cognica-io/uqa-engine/actions/runs/39093923834) for `369831adf` failed its acceptance gate: claims were accepted, but analytical materialized-result scanning was inconclusive. Its point ratio was 1.127609, noise factor 1.222614, and retained interval [0.922294, 1.378631] around a 1.15 limit. This is not passing timing evidence. The retained controller artifacts show no measured CPU steal or major-fault growth in the anomalous calibration processes; their materialized phase has lower instructions per cycle. These observations do not establish a root cause or justify deleting outliers, widening the limit or rerunning until a pass.

## Preservation argument

Let $R$ be the caller's bag of rows, $Q$ the analyzed SQL body, and $a_i(r)$ the selected, coerced argument expressions. The supported rewrite substitutes each parameter at its original use site. For every caller attribute referenced by a used argument, the rewriter allocates a fresh opaque identity $\kappa_j$ and captures that attribute from $r$ before entering the body's scope. For the resulting capture environment $C_r$, freshness gives $C_r(\kappa_j)=r_j$ and prevents any inner table, alias or CTE from changing that lookup. Substitution therefore preserves every used argument observation:

$$
\operatorname{eval}(a_i,r)=\operatorname{eval}(a_i[r_j\mapsto\kappa_j],C_r).
$$

The captures contain attributes, not argument computations. An unused parameter introduces no capture or evaluation, a conditional expression keeps its conditional argument demand, and volatile or subquery arguments fail the eligibility check. The body is analyzed without caller CTE bindings, and its selected relation references are qualified before substitution, so a same-named caller CTE cannot supply or replace a body relation. Selected argument/result types and collations are retained, including assignment casts around set-operation outputs.

For eligible definitions, the function has the caller's snapshot and no strict, security-definer or configuration boundary to reproduce. Applying the argument identity above to each leaf of $Q$ preserves the rows seen by its existing filters, projections, grouping, windows, CTEs and set operators. Those operators remain in their current owners; the rewrite introduces no alternative equality, multiplicity, ordering or scoring rule. Consequently the lateral bag result remains

$$
\biguplus_{r\in R} \{r\}\times Q[a_i\mapsto a_i(r)].
$$

Metadata binding does not execute the body. The common deferred operator initializes its physical input once at the first pull, preserving the existing blocking/spill boundaries while allowing a parent that requests no rows to close it without effects. Planner simplification still sees the substituted body and reports PostgreSQL's immutable constant errors before execution. Physical scope overlays retain outer values at nested query boundaries, and positional output shaping preserves declared columns and lock origins. No document support, payload merge, probability or ranking operation is added or changed.

Diagnostic decoration maps a primary error $e$ and its ordered frame sequence $c$ to $(e,c)$. Appending an enclosing frame is sequence concatenation: its identity is the empty sequence and its associativity preserves inner-to-outer call order. SQLSTATE, primary text, DETAIL, HINT, position and the typed cause project unchanged from $e$. Each procedural activation contributes its innermost failing statement once; a handler starts a fresh frame lifetime, while bare RAISE returns the saved pair. Cancellation matching uses the primary typed cause. This makes diagnostics observable without changing exception selection, rollback or cancellation handling.

## Independent reference

The fixture was captured with `postgresql_version` recorded in the expected JSON from the disposable Docker PostgreSQL 18.4 oracle, using `capture_command_completion_oracle.py --rows --details --context`. Stable case IDs were copied from the input fixture in statement order after verifying every SQL string matched. Input SHA-256: `c1cf43669a2a33f3ad36b5a63b0043ce8dfc9e74327e6873561b5a8e7c69804d`; expected-output SHA-256: `2c5903d9a5a43fa10b03ded1c4f14e870f8cb19c63a05bb4b22aa03032244eb6`. Generated diagnostic and build logs remain outside the tracked tree.
