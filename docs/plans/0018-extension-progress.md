# Extension task progress

This is the current status of the original [PR #638 task records](https://github.com/cognica-io/uqa-engine/pull/638), with implementation and evidence kept in the [extension plan](0018-extension-packages-and-types.md). Successor PRs retain the original 80 checkbox records verbatim as historical input. Their 50 checked and 30 unchecked entries are not a measurement of subsequent progress. The record numbers below are their one-based positions, including the five historical verification records.

Status checkpoint: the maintainer resumed #823, #119 and #309 on 2026-10-15. The active delivery is PR #855 on `fix/retained-dropped-type-diagnostic`. The retained-name correction, fixture corrections, CASE modifiers, SQL-owned CHECK/catalog nodes and Storage-owned retained boundaries are committed in logical units; Execution publication and Engine retention integration are committed as `9cf628e56` and pass the focused public tests locally. Automatic Linux/macOS acceptance of this expanded delivery remains required. Release preparation stays paused. A local pass is not a delivered result; the original records and unfinished extension units below retain their scope.

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
| #844 retained dropped-type diagnostics | Published in PR #855 | 21 Execution owner tests and four-provider reference | Qualify the complete delivered source and merge. |
| #845 retained projection names | Committed as `627dd8c68` | Five Execution owner tests and four-provider PostgreSQL reference | Automatic platform acceptance and delivery. |
| #847/#848/#849 fixture corrections | Committed as `70368a49d` | Sequence identities, native plans and wire IntervalStyle checks | Automatic platform acceptance and delivery. |
| #850 CASE result modifiers | Committed as `373015b45` | Independent PostgreSQL wire references and bounded type retention | Automatic platform acceptance and delivery. |
| #829 CHECK row analysis and #119 typed node semantics | SQL owner committed as `230984bca`; Execution adapters committed as `9cf628e56` | 79 SQL node owners, four-provider CHECK/runtime references | Complete publication integration and platform acceptance. |
| Borrowed metadata and retained committed/private boundaries | Committed as `85a4525a5` | Three-provider boundary histories, bounded metadata reads, four SQLite predecessor modes and redb upgrade | Automatic platform acceptance and delivery. |
| #119/#309 typed catalog runtime | Implemented and locally verified | Typed table/domain conbin, pg_get_expr, default/index/routine/partition nodes, role-sensitive information views and constraint rendering; FK delete subsets pass all four providers and persistent reopen | Current-main integration, actual platform reports and merge. |
| #823 physical constraint publication | Implemented and locally verified | 15 private histories, 15 SERIALIZABLE histories, three original peer histories, 15 derived-view/domain/foreign/trigger/portal histories and three predecessor initialization cases | Qualify delivered source on actual Linux/macOS reports and merge. |
| Partition FK catalog identities | Corrected and locally verified | Three persistent providers and the formerly failing SQLite creation-order reference | Include the regression in this delivery and platform acceptance. |

The previously failing #823 histories now pass: direct catalog scans retain the fixed committed physical rows plus current private row keys, including multiple visible versions with the same OID. Current binding uses the current definition head, while derived scans and held cursors keep the appropriate retained source. Domain revalidation forces a physical replacement even for equal fields. Ordinary tables, foreign tables, domains and constraint triggers publish in the existing definition transaction; predecessor initialization is atomic and does not rewrite user rows. SQLite record format 62 and redb format 58 exclude older writers; native mapping remains 17. Raw PostgreSQL and test output remains outside Git. None of this closes the complete original composite-consumer audit or the extension-package units.

## Completion path

Push the logical commits, reconcile the current main branch, and verify actual Linux/macOS automatic regression reports before merging PR #855. Close #119 only after typed catalog publication, reconstruction and retained-reader behavior pass their independent owner/provider and automatic platform checks. Then finish and record the combined record-64 comparison/consumer audit before proceeding to the next extension unit. Other original rows retain the scope and completion conditions above; discovered fixes must name the row they advance and cannot silently replace it.
