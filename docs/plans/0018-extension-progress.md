# Extension task progress

This is the current status of the original [PR #638 task records](https://github.com/cognica-io/uqa-engine/pull/638), with implementation and evidence kept in the [extension plan](0018-extension-packages-and-types.md). Successor PRs retain the original 80 checkbox records verbatim as historical input. Their 50 checked and 30 unchecked entries are not a measurement of subsequent progress. The record numbers below are their one-based positions, including the five historical verification records.

Status checkpoint: main `bef6f8068e7a311f040fdb46b2af187de9c441d2`, after PR #854. Its exact delivered tree passes both actual automatic platform regression suites. The open delivery is the retained dropped-type diagnostic correction on `fix/retained-dropped-type-diagnostic`; its own automatic platform results remain pending. Work pauses at the maintainer's request after this PR is published, with the remaining local corrections preserved. A locally passing change is not merged or automatically qualified. Update this record, the delivering PR and the affected issue when implementation, verification or delivery changes; move a completed delivery into its original item's evidence rather than leaving it in the active queue.

## Original items and current status

Every originally unchecked record is represented below. An open row without new evidence remains required work; it is not a compatibility exception. A merged prerequisite does not complete a broader row.

| Record | Original requirement | Current status and evidence | Remaining completion condition |
| --- | --- | --- | --- |
| 9 | RI-trigger, TOAST and cloned unique-index OID allocation | Open; no complete allocation-order evidence recorded | Match creation-order OIDs for all named object classes. |
| 18 | Rename system, temporary and graph namespaces | Open | Implement the PostgreSQL behavior and verify authority, dependencies and rollback. |
| 24 | Deferrable UNIQUE and PRIMARY KEY | Open | Implement deferred enforcement, diagnostics and transaction behavior. |
| 25 | Column COLLATE, inheritance conflicts and regcollation | Partial: regcollation merged in #638 | Complete column and inherited-collation behavior; the alias correction alone does not close this row. |
| 26 | Typed tables and column options | Open | Implement CREATE TABLE OF, dependency/lifecycle behavior and PostgreSQL verification. |
| 29 | Builtin and relation-row type ACLs | Open | Match GRANT/REVOKE, typacl and USAGE checks. |
| 33 | Infinite temporal values and consumers | Implementation merged in #770; original-item closure audit remains | Consolidate the recorded scalar, aggregate, range, arithmetic and provider evidence for this complete row. Finite bounds remain record 34. |
| 34 | Full finite temporal ranges and input formats | Partial: ISO interval input is included in #770 | Complete finite bounds, calendar arithmetic and every remaining named input form; interval support alone does not close the row. |
| 36 | Internal FK trigger rows and dependencies | Open | Publish and retain all RI trigger rows and their dependencies. |
| 37 | Stored implicit coercions, dependencies and rule bodies | Partial: typed coercion/node corrections are merged, including #792; runtime catalog work remains in #119 | Verify domain-to-base coercions and assignments, dependency recording, deparse and distinct rule qualification/action sets. |
| 38 | Names in retained view, rule and routine definitions | Partial; retained projection-name correction #845 is locally verified | Complete rename/alias/USING behavior across the original definition forms; #845 alone is not that matrix. |
| 39 | Dependencies for unpinned/system and external objects | Partial: foreign-server ownership/drop completed in #415/#416 | Complete the remaining system, TOAST, wrapper, mapping and collation object classes. |
| 40 | COLLATE, routine language, typed defaults and DROP notices | Partial: routine prolang is merged in #450; default-node integration is local under #119 | Deliver typed adbin and verify the remaining COLLATE and notice behavior. |
| 43 | Remaining builtin routine identities | Partial: additional concrete call signatures merged in #813 | Complete remaining builtin families, regproc/regprocedure lookup and stored-call behavior. |
| 44 | Functions, types and domains in pg_temp | Open | Match creation, lookup, lifetime, visibility and dependencies. |
| 46 | Shared temporary namespaces and backend-slot numbering | Open | Preserve namespaces after session exit and expose other sessions' temporary catalog rows. |
| 48 | WINDOW routines and remaining routine/settings lifecycle | Open | Complete every original statement form, privilege rule and missing setting. |
| 50 | TABLESAMPLE, system columns and whole-row view columns | Open | Complete the original execution and metadata surfaces with PostgreSQL evidence. |
| 64 | Composite type lifecycle and record comparison | **Active original unit.** ADD/DROP/RENAME/ALTER ATTRIBUTE and retained tuple behavior are merged through #702; comparison corrections are merged in #746/#755/#760/#763/#766; retained grouping identity is merged in #854 (#841 closed) | Finish the combined comparison/consumer exit audit and remaining discovered corrections. The #119/#823 follow-up below does not itself complete this row. |
| 65 | User-defined ranges and multiranges | Not started | Complete the range unit's PostgreSQL and reopen evidence after composite acceptance. |
| 66 | Package registry and pre-open options | Not started | Deliver canonical manifests, sealed registry and constructor/reopen validation. |
| 67 | SQL extension lifecycle | Not started | Deliver CREATE/ALTER/DROP EXTENSION, scripts, members, catalogs and rollback. |
| 68 | Extension value envelope and host base types | Not started | Deliver the carrier, registered I/O and every observing consumer with the required proof. |
| 69 | Operators, casts, operator classes and families | Not started | Deliver declarations, resolution, catalogs and independent diagnostic evidence. |
| 70 | Comparator indexes and semantic uniqueness | Not started | Deliver ordered indexes, uniqueness and concurrent-provider schedules. |
| 71 | Generic data models | Not started | Deliver descriptors, resumable execution and the reference model. |
| 72 | Optimizer extensibility | Not started | Deliver pure hooks, checked rewrites, access paths and optimizer acceptance. |
| 73 | Language SDKs and host bridges | Not started | Verify actual Python, Node.js and WASM artifacts through creation, persistence and reopen. |
| 74 | Migration, wire and restoration | Not started | Deliver extension format changes, wire I/O, backup/restore and cross-language reopen. |
| 75 | Public acceptance and documentation | Not started | Complete public contracts, examples and final acceptance for all preceding units. |

## Current follow-up and its original scope

[#119](https://github.com/cognica-io/uqa-engine/issues/119) was taken up while verifying record 64 and also advances the catalog/deparse requirements in records 37 and 40. It covers typed catalog expressions and constraint introspection, not package registration or SQL extension installation. [#823](https://github.com/cognica-io/uqa-engine/issues/823) is a reproduced transactional visibility defect in that runtime integration. Keep both issues open until their public behavior and provider acceptance are delivered.

The recent delivered corrections are #851 (original RETURNING inputs), #852 (contextual parameter types) and #853 (nested JOIN USING keys). Their actual Linux and macOS automatic reports pass respectively 444/614, 450/620 and 461/631 checks/cases, with zero JUnit failures/errors/skips and eight independent-process correctness histories on each platform. Their tested-source and tree identities and closed issues are recorded in the [implementation evidence](0018-extension-packages-and-types.md#original-returning-inputs-and-declared-wildcard-scopes). These are prerequisite deliveries, not three completed original extension items.

| Work | State at this checkpoint | Verified evidence | Still required |
| --- | --- | --- | --- |
| #844, retained dropped-type diagnostics | Published correction; automatic platform evidence pending | 21 Execution owners and four provider cases, including 145 initial and eight reopen observations per provider | Actual Linux/macOS results for 468 checks / 641 cases, review and merge; then close #844. |
| #845, retained projection names | Local verification only | Five Execution owners and four provider cases with 85 PostgreSQL observations | Isolated delivery and automatic platform acceptance. |
| #847, sequence identity assertions | Local test correction only | Focused sequence and restoration checks | Deliver the fixture correction; no product behavior change is claimed. |
| #848, native plan fixtures | Local test correction only | Focused Execution, Planner and Engine checks | Deliver the fixture correction; no product behavior change is claimed. |
| #849, reported IntervalStyle coverage | Local test correction only | Two wire checks and strict pg-server Clippy | Deliver the fixture correction; no product behavior change is claimed. |
| #850, CASE result modifiers | Local verification only | Independent PostgreSQL wire references, SQL/Planner/wire checks and bounded type-retention checks | Isolated delivery and automatic platform acceptance. |
| Borrowed metadata reads | Local verification only | Owner/provider memory, callback-error and transaction-boundary checks | Isolated delivery and automatic platform acceptance; this does not fix #823 by itself. |
| #829, CHECK row analysis | Local verification only | SQL and Execution owners plus four-provider PostgreSQL references | Deliver borrowed row binding, dependencies and runtime behavior with automatic platform acceptance. |
| Constraint rendering and default assignment | Local verification only under #119/#309 | Typed-node, selected-argument and session-output references | Deliver the qualified corrections and complete public catalog integration. |
| #119 catalog-node runtime | Local integration verified in part; not delivered | Focused typed conbin/default/index/routine/partition and usage-view references | Finish transactional publication, retained readers and restoration; deliver and verify the complete runtime contract. |
| #823 ALTER publication decisions | Local verification only | 17 independent PostgreSQL tuple observations; 22 SQL owners; one Engine state test across three persistent providers; strict SQL/Execution/Engine Clippy and rustfmt | Deliver and verify automatic platform results; equal-payload domain validation still needs explicit physical replacement in the journal. |
| #823 physical catalog tuples | Implementation in progress; public failure remains | Local fixed/current-private reads, typed tuple codec, transaction guards, indexed borrowed metadata seeks and selected-domain construction pass focused checks | Connect SQL DDL publication, format initialization, direct/derived catalog reads and Engine retention adapters. The 15 private-history and three peer failures remain open; 15 additional SERIALIZABLE histories have not run against UQA. |

The latest selected-domain constructor passes 97 Execution catalog-projection tests, including two new identity/encoding and early-exit/error cases, strict Execution library/test Clippy and workspace rustfmt. Independent PostgreSQL evidence confirms unchanged domain constraint rows after a domain rename. The earlier active-transaction lookup correction passes six Execution tuple tests. Earlier indexed-read qualification passes twenty focused Execution/Storage/SQLite/redb checks, including bounded visits and borrowed payload allocation. These are local source results; they do not establish a completed SQL visibility fix or automatic platform acceptance. Raw logs and diagnostic reports remain outside Git.

## Completion path

Finish the current published unit and deliver the already qualified corrections in logical units while completing #823's actual DDL and read consumers. Close #119 only after typed catalog publication, reconstruction and retained-reader behavior pass their independent owner/provider and automatic platform checks. Then finish and record the combined record-64 comparison/consumer audit before proceeding to the next extension unit. Other original rows retain the scope and completion conditions above; discovered fixes must name the row they advance and cannot silently replace it.
