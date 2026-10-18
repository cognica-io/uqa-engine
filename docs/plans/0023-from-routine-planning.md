# Functions in FROM: planning and diagnostics

Status: Active

Issue [#1068](https://github.com/cognica-io/uqa-engine/issues/1068) retains two requirements after PR #1050: PostgreSQL 18 planning of eligible SQL set functions beyond simple constant-input projections, and SQL/PL/pgSQL diagnostic CONTEXT. The original issue report and acceptance criteria remain authoritative. This work starts from `e38848f8a` and preserves the fixes already delivered for unknown inputs, record definitions, polymorphic results, result width, scalar aliases and lazy volatile calls.

## Work and acceptance

- [ ] Capture independent Docker PostgreSQL 18 evidence for the remaining query shapes, row-dependent inputs and diagnostic contexts, including unfetched calls, nested routines and repeated execution.
- [ ] Extend SQL-owned typed routine expansion and Planner integration without capturing caller columns in an inner scope, duplicating volatile evaluation or crossing routine security/settings boundaries. Preserve result coercion, row multiplicity and ordering, and use the existing execution owners for expanded queries.
- [ ] Carry structured diagnostic context through SQL errors, routine analysis, planner inlining and SQL/PL/pgSQL execution, preserving SQLSTATE, primary message, DETAIL, HINT and diagnostic order through the public adapters.
- [ ] Add focused owner tests and PostgreSQL transcript replay through all four providers, including persistent reopening. Record the preservation argument and final evidence here, and update the manual and HISTORY for the delivered behavior.
- [ ] Review the final change, run formatting and ownership/dependency checks, and verify actual automatic Linux/macOS bounded-work, memory and output regression results for the delivered product source. Qualify FDW once before merge if the final source fingerprint requires it.
- [ ] Update the PR and issue records, merge, confirm #1068 closed, return to main and remove the completed branch.

## Ownership and dependencies

The reviewed Cargo manifests and `scripts/workspace-dependency-policy.json` place routine declarations, typed body analysis, result assignment and SQL diagnostics in `uqa-sql`; caller-plan simplification in `uqa-planner`; and invocation, procedural statement execution and diagnostic-frame lifetimes in `uqa-execution`. Engine supplies the existing catalog, session and evaluation adapters. These owners have no optional feature gate for this behavior; Engine's Arrow, DuckDB, Nori and Kuromoji features remain independent. No reverse dependency or new test executable is needed.

## Progress

The current set-function expander rejects CTEs, set operations, aggregates, DISTINCT, subqueries, relation sources and row-dependent arguments. Its post-order parameter substitution is safe only for closed inputs. The current `SQLError` preserves SQLSTATE, DETAIL, HINT and positions but does not yet carry routine CONTEXT. Implementation and verification remain pending.
