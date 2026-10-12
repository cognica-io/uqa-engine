# Catalog expression delivery units

Status: Active. This ledger splits the 42 commits retained in [PR #774](https://github.com/cognica-io/uqa-engine/pull/774) at `f63cb99b9eb15e46c6ceaf6069721301501a2ab8`. The source branch remains a recovery reference until its changes have been delivered. The [extension plan](0018-extension-packages-and-types.md) and all 80 inherited PR #638 task records remain authoritative; decomposition does not mark any additional task complete.

Open one implementation PR at a time. Extract each unit onto the then-current main, bring its owning-crate tests, PostgreSQL reference provenance, proof and public documentation, and verify that tree before merging. The table gives extraction boundaries, not 25 pre-created branches or permission to carry unrelated fixes into one PR. Recheck dependencies during extraction and split a row again if it contains independently reviewable behavior. Do not copy later units' completion claims or whole accumulated manual/HISTORY changes into an earlier unit.

“Preserved” means source and prior evidence exist on the archived branch; it does not mean the unit is independently qualified or merged. Shared metadata must precede its consumers: catalog selection precedes namespace/runtime consumers, declared and row-schema metadata precede query/DML outputs, expression binding precedes field/constant rewrites, and those identities precede rule/trigger/routine consumers. The catalog-aware predicate and ordered-set corrections can be extracted independently after checking their main baseline. Each extracted unit receives its own tested commit and Linux/macOS automatic performance results; the old combined result cannot qualify a different source tree.

| Unit | Source commits | Review boundary | Status |
| --- | --- | --- | --- |
| Anonymous ROW catalog nodes | `b01f2598f`, `d416e7e72` | SQL operator identities, ROW nodes, DISTINCT/NULL expansion and SQL reconstruction | Extracted; 1,673 SQL tests and local checks pass; automatic CI pending |
| Named ROW catalog nodes | `eb6717e50` | Named constructors and live descriptor SQL reconstruction | Preserved |
| Stored ROW descriptors and diagnostics | `a2c81c707`, `639057bda` | Definition-time positions, coercions, retained copies and analysis errors | Preserved |
| Catalog collation selection and explicit COLLATE | `50c9e31cd`, `5f2bbf2da` | Post-typing selection, explicit syntax, coercion and the original collated ROW reference | Preserved |
| Retained and prepared collations | `dceefbedb`, `0e6f424c7` | Stored identities, input normalization and prepared ROW coercions | Preserved |
| Collation namespaces and retained budgets | `8f70b1265`, `af740ada9` | Namespace/privilege lookup and admitted name-selection scratch | Preserved |
| Runtime COLLATE and stored SQL | `4e2beb31f` | Value forwarding, exact reconstruction and four-provider behavior | Preserved |
| Declared relation and domain collations | `2d6c06e4e` | Declaration metadata, provider restoration and rollback | Preserved |
| Catalog-aware record predicates | `681551a3d` | Projected-predicate boundary and canonical value evaluation | Preserved; independent extraction candidate |
| Ordered-set result identities | `b349407d8` | Polymorphic result substitution before parent binding | Preserved; independent extraction candidate |
| Catalog variables and typed row schemas | `8226dd04d`, `95d9437b7` | Attribute identity, selected column metadata, schema composition and spill | Preserved |
| Query source and direct result schemas | `fb3ae77a7`, `cfada5e54` | Table scans, aliases, direct SELECT/CTE/subquery outputs | Preserved |
| DML and RETURNING schemas | `231d77bc0` | Target and OLD/NEW metadata; closes #775 when delivered | Preserved |
| Computed output analysis | `d5cb47162`, `a8ee23aa5`, `991166bff` | Shared ordinary/prepared expression analysis and window diagnostics | Preserved |
| Domain and array common types | `59d07e7c9`, `118a8f97d` | Coercion metadata and exact common-input domain retention | Preserved |
| VALUES and set-operation binding | `9cfc01cce`, `6b542e7df` | Column metadata and row/column diagnostic order | Preserved |
| Sort and group collation requirements | `971f409fb` | Aggregate/window keys and alias/ordinal metadata reuse | Preserved |
| Composite and anonymous field descriptors | `6b77733fb`, `f35d18c4a` | Named/computed ROW field metadata and retained result descriptors | Preserved |
| Materialized relation collations | `50bbbd3d4` | CTAS/SELECT INTO/materialized views, refresh and restoration | Preserved |
| Typed constants and expression rewrites | `14baa88b9`, `13bcf901f` | Retained implicit identities, codec/copy accounting and simplification | Preserved |
| Rule conditions and namespaces | `b3e9d30c9`, `892c566dd` | Condition binding and declaration diagnostics; closes #777 when delivered | Preserved |
| Rule action field binding | `42a7cc05e`, `f3ad42bb3` | Action schemas and parenthesized event fields; closes #778 and #780 when delivered | Preserved |
| Trigger condition collations | `c5573d85d` | WHEN record fields and catalog adapters; closes #779 when delivered | Preserved |
| Physical query result schemas | `8ae5701b1`, `ff8d46642` | VALUES metadata and positional materialized rows; closes #781 and #782 when delivered | Preserved |
| Procedural parameter collations | `f63cb99b9` | Cached variable references and parameter metadata; closes #783 when delivered | Preserved |

The remaining source commit `9628d8ab6` records the required automatic performance-regression policy and is carried with this extraction ledger. Every source commit appears exactly once above or in this sentence.

Issue [#784](https://github.com/cognica-io/uqa-engine/issues/784) has additional uncommitted trigger/routine record-schema work preserved in the original worktree and a verified local backup. Its owner test passes, but its four-provider integration fails because `pg_collation_for(text)` is unresolved; that work is not qualified. Preserve the independent eleven-case PostgreSQL fixture and correct the missing runtime consumer in its owning unit before claiming completion. The original worktree also contains another session's `tests/parity/pg18/README.md` edit, which is outside this extraction.

## Anonymous ROW boundary

The first extracted unit changes only SQL-owned catalog ROW encoding/decoding and selected B-tree operator membership. It retains scalar equality/inequality expansion, lexicographic operator identities, nested records, DISTINCT/NULL shape, string-list quoting and malformed Datum/list rejection. Named constructors, retained descriptor evolution and explicit COLLATE belong to the subsequent units above. No Engine algorithm, workspace dependency, Cargo feature or persistent provider format changes.

The original 41-case PostgreSQL fixture is partitioned by implementation ownership: forty anonymous/default-collation ROW observations are in `crates/uqa-sql/src/catalog/node_tree/tests/rows.json` (SHA-256 `848c2d7e4952543a098040e16f99320addbfb5f5e421485edb6767c4ffce8c40`); the remaining `ROW(VALUE COLLATE "C", 'a') >= ROW('z', 'b')` observation remains unchanged in the source fixture at `d416e7e72` for the explicit COLLATE unit. No existing main test is disabled and no PostgreSQL expectation is rewritten. Seven exact diagnostic observations remain in `tests/row_errors.json` (SHA-256 `060a494fd54d120e5d34aed50755983b6769a7183f57fd8443a678273b1c9029`).

Reference capture used PostgreSQL 18.4 Docker image `sha256:bc8a94e4a938576533c0f0e8ad6f2e8c67d5c5facef05005a5e5cf9adce2fe25`, a disposable database, UTC, ISO/MDY, postgres IntervalStyle and standard-conforming strings. The original success reference contains the complete `conbin` and both `pg_get_expr` pretty modes. Diagnostics use the existing collector with `--rows --details`, SHA-256 `13a39469bb3d558afeee4871e01b992101c49980fc49e2d246bb327248f9e0ec`. Raw captures and machine reports remain outside Git.

For a row $r=(r_1,\ldots,r_n)$ and comparison $s$, let $o_i$ be the already selected scalar operator for $(r_i,s_i)$. Encoding preserves every $o_i$ and its typed operands. Equality expands to the ordered conjunction of those operators, inequality to their ordered disjunction, and ordering stores a common B-tree strategy together with each exact operator family and input collation. DISTINCT uses the ordered disjunction of scalar DISTINCT observations, with false for the empty row. Nested ROWs retain record identity; row NULL tests retain the row/scalar distinction. Reconstructing syntax reads the retained operators rather than selecting new ones. Thus this representation preserves the original selected expression and its NULL/diagnostic observations; it introduces no new value relation, posting payload, ranking or graph operation, and existing query compositions keep the same carriers and operations.

All 1,673 SQL owner tests pass for the extracted tree, including the forty node/SQL references, seven diagnostic observations and malformed representation tests. Strict SQL Clippy (`--lib --tests -- -D warnings`), workspace formatting, dependency direction, Engine capability ownership, the single integration-harness policy, Rust headers/file limits and whitespace checks pass. The unchanged regression limits now select 186 checks and 267 required cases, including five extracted output/representation checks; all 24 runner self-tests pass. Automatic Linux and macOS bounded-work, memory and output-correctness checks must still pass for the product commit before merge; build/archive success, pending checks and evidence from the larger source branch are insufficient.
