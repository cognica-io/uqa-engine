# Extension packages, data models, and SQL types implementation plan

Status: Active. Implementation starts from main `8a99d4fd` (0.4.7), inspected on 2026-09-29. The [extension design](../design/data-model-and-type-extensions.md) defines the target contracts, proofs and acceptance criteria; this plan fixes the implementation order, owners, representation decisions and completion ledger. The [manual](../manual/README.md) remains authoritative for public behavior; a unit extends it only when its exit evidence passes.

## Outcome and fixed boundaries

Deliver the required outcomes of the design: user-defined enum, composite, range and base types with their generated arrays and domain composition; host-registered extension packages installed by PostgreSQL 18 `CREATE EXTENSION`, `ALTER EXTENSION` and `DROP EXTENSION`; functions, casts, operators, operator classes and families contributed by those packages; owned data models with generic source and command nodes; optimizer contributions consumed by normal planning; and direct authoring in Rust, Python, Node.js and browser WASM. Ordinary SQL and extension work must compose in the same query and logical transaction, and a second independently authored model or type must not require a new core enum branch or an Engine algorithm.

PostgreSQL 18.4 is the oracle wherever PostgreSQL defines the construct, including values, catalog rows, diagnostics, validation order, transaction effects and persistence. A difference is a bug fixed in its owning crate. Each unit captures its PostgreSQL evidence from a pinned `postgres:18.4` image before implementing the behavior, and never derives expected results from UQA output.

The unit order below follows ownership dependencies. PostgreSQL-native type forms come first because they exercise durable type identity, catalog projection, binding, value consumers and persistence without host code. Package registration, the extension-value envelope and host implementations then reuse those owners. Unsupported shapes keep failing explicitly with PostgreSQL diagnostics until their unit is complete; a parser-accepted but ignored declaration is forbidden.

## Inspected baseline and reuse decisions

The relevant `Cargo.toml` files, the [dependency policy](../../scripts/workspace-dependency-policy.json), the [engine capability policy](../../scripts/engine-capability-policy.json), the harness and file-line checks, and the owning implementations and tests were inspected before defining these units.

| Existing boundary | Implementation decision |
| --- | --- |
| [Core values](../../crates/uqa-core/src/types/value.rs) have hand-written total order and no enum, range or extension variant; ranges and `uuid` use a text carrier distinguished by `ColumnType` | Add one enum carrier whose order is a proven order-preserving key. Keep a separate opaque extension envelope for host types so byte order never defines SQL order for payloads without a proof. |
| Value hashing is implemented separately by [join keys](../../crates/uqa-joins/src/row_join.rs), [DISTINCT and grouping encoding](../../crates/uqa-execution/src/distinct/encoding.rs) and [partition hashing](../../crates/uqa-sql/src/semantics/partition/hash.rs) | Extend each consumer with the carrier's equality-congruent encoding; do not add a second universal hash. |
| Storage persists values through serde JSON with `$uqa_type` tags, the SQLite [typed value mirror](../../crates/uqa-storage-sqlite/src/document_store/typed_value.rs) and the binary [spill codec](../../crates/uqa-execution/src/spill/format/binary/encode.rs) | Add explicit tags to every codec and reject malformed carriers with typed errors instead of decoding them as maps. |
| [`ColumnType`](../../crates/uqa-sql/src/ast/types.rs) carries domains structurally as `Domain { schema, name, oid, base }`; `Named` is never stored | Add bound user-type variants in the same structural style and resolve them through the existing catalog type-name hook. |
| [CREATE DOMAIN](../../crates/uqa-execution/src/schema/domains.rs) reserves type names, persists one serde record per definition plus OID claims, publishes a copy-on-write registry and restores it on open and rollback | Reuse this lifecycle for enum, composite and range definitions: one record per type, claims for every allocated OID, private-change merging and savepoint restoration. |
| [Type-name reservation](../../crates/uqa-execution/src/schema/namespaces/type_names.rs) coordinates domains with relation row types | Route every new type form through the same reservation and 42710 diagnostics. |
| [Catalog OID reservation](../../crates/uqa-execution/src/catalog/identity/allocation.rs) supports constraint and relation classes | Add `pg_type`, `pg_enum` and `pg_extension` classes and reserve new OIDs through the same lock-and-recheck protocol. |
| [`build_pg_type`](../../crates/uqa-execution/src/catalog/projection/pg_catalog/types.rs) projects built-in and domain rows; `pg_enum`, `pg_cast`, `pg_operator`, `pg_opclass`, `pg_opfamily`, `pg_amop`, `pg_amproc` and `pg_extension` are absent and `pg_depend` has no rows | Project each catalog from the implemented registries only; never fabricate rows for unimplemented objects. |
| [Statement dispatch](../../crates/uqa-sql/src/compiler/dispatch.rs) rejects type and extension statements as `0A000 unknown statement` | Lower each statement form only when its execution, persistence and catalog behavior are complete. |
| [Runtime callbacks](../../crates/uqa-execution/src/functions.rs) are non-durable, name-keyed and registered after open; catalog restoration runs with empty maps | Supply a sealed extension registry before catalog decoding through explicit Engine options; keep existing callback APIs unchanged. |
| [Planner API](../../crates/uqa-planner/src/optimizer/api.rs) uses a function-pointer constant evaluator and a `SourceStatistics` estimate hook | Extend Planner-owned pure interfaces for model alternatives without a global mutable registry. |
| Engine is at its fourteen-crate dependency budget; SQL and Planner are also at budget | Put contracts in Core, SQL, Storage, Planner and Execution; Engine only threads registries, sessions and retained resources. The proposed author facade never becomes an owner dependency. |

## Representation decisions

Enum values use a dedicated Core carrier `EnumValue { type_oid, key }`. The key is an immutable byte string allocated when a label is created and compared lexicographically; it is never the PostgreSQL float4 `enumsortorder`, because PostgreSQL renumbers that column when midpoints collapse while the relative order of existing labels never changes. A new label receives a key strictly between the keys of its neighbors, so for labels $a$ and $b$ of one type, $a < b$ in declaration order if and only if $k(a) <_{\mathrm{lex}} k(b)$, and $k(a) = k(b)$ if and only if $a = b$. Every existing context-free consumer of `Value` order, equality and hashing therefore implements the SQL enum order without a catalog callback. Different enum types are never compared by SQL binding; the carrier orders them by type OID only for internal bookkeeping.

The catalog maps `(type_oid, key)` to the label OID, current label text and the PostgreSQL float4 sort position. Text output resolves labels through the statement's catalog context, so `ALTER TYPE ... RENAME VALUE` changes no stored row. A context-free text conversion of an enum carrier is a typed error rather than an invented rendering. `pg_enum.enumsortorder` follows PostgreSQL's float4 midpoint and renumbering algorithm exactly and is independent of the stored key.

Composite, range and host base types add their own bound `ColumnType` variants. Composite values reuse the named `Record` carrier with a bound type identity; user ranges generalize the closed built-in range subtype set; host base types use the separate `ExtensionValue` envelope whose SQL order, equality and hashing come only from bound operator-family operations.

## Dependency order and progress ledger

A unit is complete only when its exit evidence is recorded against the actual source revision. Units may contain several logical commits. Internal prerequisites may land before their SQL surface, but SQL continues to reject a form until its unit is complete.

| Unit | Prerequisites | Primary owners | Status and evidence |
| --- | --- | --- | --- |
| Enum value carrier | None | Core, Joins, SQL, Execution, Storage, providers, bindings | Complete: key allocation, codecs, retention and consumer tests below |
| Enum catalog and SQL surface | Enum value carrier | SQL, Execution, Engine adapters, pg-server | Complete: 293-statement PostgreSQL 18.4 oracle on every provider and over TCP, the 132-statement partition bound oracle, reopen and binding tests below |
| Type object lifecycle | Enum catalog and SQL surface | SQL, Execution | In progress: rename, schema moves, ownership, `USAGE` grants and enforcement, identity-bound stored references, catalog-aware deparse, per-session routine compilation, creation-ordered catalog OIDs, `pg_depend`, `pg_shdepend` and `pg_describe_object`, and dependency-aware removal for every `DROP` with PostgreSQL's details and notices (184-case drop dependency oracle) are implemented; `BEGIN ATOMIC` bodies in `pg_get_function_sqlbody`, counter OIDs for graph objects and temporary namespaces, identity resolution of type names recorded by earlier releases, pg-server result metadata, and the window and aggregate oracles remain |
| Composite types | Type object lifecycle | Core, SQL, Execution | Not started |
| User-defined ranges and multiranges | Composite types | SQL, Execution | Not started |
| Package registry and pre-open options | None | Core, SQL, Execution, Engine adapters | Not started |
| SQL extension lifecycle | Package registry; type object lifecycle | SQL, Execution, Engine adapters | Not started |
| Extension value envelope and host base types | SQL extension lifecycle | Core, SQL, Execution, Storage | Not started |
| Operators, casts, operator classes and families | Extension value envelope and host base types | SQL, Execution | Not started |
| Comparator indexes and semantic uniqueness | Operators, casts, operator classes and families | Storage, Execution, providers | Not started |
| Generic data models | SQL extension lifecycle; comparator indexes | SQL, Execution, Operators, Storage | Not started |
| Optimizer extensibility | Generic data models | Planner, SQL, Execution | Not started |
| Language SDKs and host bridges | Optimizer extensibility | Python, Node.js, WASM bindings | Not started |
| Migration, wire and restoration | Language SDKs and host bridges | Execution, Storage, Engine adapters, pg-server, bindings | Not started |
| Public acceptance and documentation | All preceding units | All owners | Not started |

## Unit exit evidence

### Enum value carrier

Add `EnumValue` and its immutable key to Core with key allocation for creation, append, and insertion before or after a neighbor; serde, budgeted decoding, representation identity, retention, and total order; the SQLite typed mirror and spill codec tags; DISTINCT, join and partition hashing; and Python, Node.js and WASM conversion boundaries that require catalog label resolution. Exit evidence: owner tests for key allocation order and minimality, malformed and oversized key rejection, round trips through every codec, and the written order-embedding argument above.

Evidence: `cargo test -p uqa-core enum_value` covers key allocation order, minimality, malformed, empty and oversized keys, and serde; the SQLite typed mirror, spill codec, DISTINCT encoding and join hashing have round-trip and equality tests in their owners, and value retention rejects malformed carriers with typed errors. Context-free text conversion reports `enum value of type OID N requires catalog-aware output`; every host boundary renders labels through the catalog instead.

### Enum catalog and SQL surface

Implement `CREATE TYPE ... AS ENUM`, `ALTER TYPE ... ADD VALUE [IF NOT EXISTS] [BEFORE | AFTER]`, `ALTER TYPE ... RENAME VALUE`, and `DROP TYPE [IF EXISTS] ... [CASCADE | RESTRICT]` with dependency analysis over columns, domains, routines and arrays. Project `pg_type` rows for the enum and its array and `pg_enum` rows with PostgreSQL float4 sort positions. Bind unknown-literal input, explicit text-family input casts, assignment casts to text types, the six comparison operators, `enum_first`, `enum_last`, both `enum_range` forms, `enum_cmp`, `enum_smaller`, `enum_larger`, `hashenum`, `min` and `max`, arrays, JSON output and `anyenum` polymorphic routines. Enforce PostgreSQL's uncommitted-label rule (`55P04`) with transaction and savepoint scope. Exit evidence: a checked-in PostgreSQL 18.4 stateful oracle suite, memory, native SQLite, SQLite Key/Value and redb reopen tests, and pg-server result metadata.

Evidence: `tests/parity/pg18/enum_types_oracle.expected.json` (293 PostgreSQL 18.4 statements) replays on memory, native SQLite, SQLite Key/Value and redb engines (`queries::sql_enums`) and over TCP with result metadata (`uqa-pg-server` `enums::`); reopen tests on the three persistent providers cover renamed and positioned labels, arrays, continued key allocation and dependency errors; the Node.js, Python and browser WASM suites and the Arrow `QueryBuilder` return labels; `tests/parity/pg18/literal_coercion_oracle.expected.json` records the general `unknown`-literal resolution this surface depends on. Binding folds an `unknown` literal coerced to an enum type into a typed constant once, and statement analysis reports an invalid label before assignment checks, as PostgreSQL's parse analysis does. Enum support functions are bound structurally (`FunctionDispatch::Enum`), user routines match `anyenum` through the simple polymorphic family, and overload matching identifies enum types by OID (`enum#oid`), which catalog type resolution accepts.

The oracle also covers enum partition keys and stored expressions. LIST, RANGE and HASH partitioning route enum keys; HASH hashes the label OID as `hashenumextended` does. Partition bounds are transformed as `transformPartitionBound` does for every key type: coerced to the key type in assignment context, evaluated once and stored as typed constants, so a renamed label keeps its bound and `pg_get_expr` spells bounds by key type. `tests/parity/pg18/partition_bounds_oracle.expected.json` (132 statements with DETAIL and HINT, `queries::sql_partition_bounds`) verifies bound transformation and rendering, strategy and width errors, overlap, empty-range, default-conflict and hash modulus diagnostics, default partition contents, partition key restrictions and routing failures. Generated columns, index expressions and partition key expressions share the stored-expression analysis: the stable enum support functions and every cast whose `pg_cast` function or I/O conversion is not immutable report `42P17`, invalid labels report `22P02` at definition, virtual generated columns follow PostgreSQL 18's user-defined type and function restrictions, and stored generated values reach their column type through catalog-aware assignment. NOT NULL, CHECK, partition constraint and auto-updatable view check option violations report `Failing row contains ...` with PostgreSQL's privilege rules, using each INSERT, UPDATE and MERGE statement's supplied columns, verified by `tests/parity/pg18/failing_row_detail_oracle.expected.json` (`queries::sql_failing_row_details`).

Findings outside this unit were fixed where the enum surface exposed them: operator-selected coercion of `unknown` operands for every comparison, IN list, BETWEEN, ANY/ALL, NULLIF and IS DISTINCT FROM; `anycompatible` array-function arguments; PostgreSQL `format()`; explicit `VARIADIC` arrays for `VARIADIC "any"` built-ins; JSON builder diagnostics; window output names in ORDER BY; whole-row values that exposed `tableoid`; bytea input during assignment; and the optimizer's volatility of `enum_first`, `enum_last` and `enum_range`. The remaining ones have owners. Stored expressions (views, defaults, CHECK constraints, generated columns, index expressions, partition keys, rules and trigger conditions) must hold enum constants by label identity and deparse them as `'label'::type`, dependency errors must carry PostgreSQL's DETAIL listing, a `WITH CHECK OPTION` violation through MERGE into a trigger-updatable view must describe the view row, and binding diagnostics must name types by search-path visibility: all in Type object lifecycle. Relation row types in `pg_type`, with stored generated array names, `reltype` consistency and field selection from whole-row references, belong to Composite types. Hash partitioning over the remaining hashable built-in types and default operator class diagnostics belong to Operators, casts, operator classes and families. Parse-time validation of `unknown` literals coerced to built-in types in relational predicates, multi-argument aggregates used as window functions, star projections combined with system columns, and catalog type-name resolution without a full catalog view each need their own change with PostgreSQL evidence.

### Type object lifecycle

Implement `ALTER TYPE ... RENAME TO`, `SET SCHEMA` and `OWNER TO`, `GRANT` and `REVOKE USAGE ON TYPE` with `typacl`, and dependency-following for stored type references in columns, domains, routines, indexes and views, for every implemented type form. Exit evidence: PostgreSQL 18.4 oracle cases for each form, rename and privilege lifecycle across reopen, and dependency-aware drops.

### Composite types

Implement `CREATE TYPE ... AS (...)` with a standalone composite relation row, typed record I/O, field selection, nested composites and arrays, record comparison through each field's order, and `ALTER TYPE ... ADD | DROP | ALTER | RENAME ATTRIBUTE` with dependency checks. Exit evidence: PostgreSQL 18.4 oracle suite and all-provider reopen tests.

### User-defined ranges and multiranges

Generalize the built-in range subtype set to catalog-resolved subtypes with operator-class, collation, `canonical` and `subtype_diff` validation, the generated multirange type and `pg_range` rows. Exit evidence: PostgreSQL 18.4 oracle suite including enum and text subtypes, and reopen tests.

### Package registry and pre-open options

Add package manifests with canonical bounded encoding and digest, a sealed registry split into descriptor, optimizer and runtime parts, and Engine options accepted by every constructor that opens or restores storage, including sibling sessions and binding equivalents. A database requiring an unavailable or incompatible implementation fails before decoding. Exit evidence: owner tests for canonical digests and validation, and open/reopen failures before decoder invocation.

### SQL extension lifecycle

Implement PostgreSQL 18 `CREATE EXTENSION`, `ALTER EXTENSION UPDATE | SET SCHEMA | ADD | DROP` and `DROP EXTENSION` over registered SQL packages with control metadata, directed update scripts, member tracking, `pg_extension`, `pg_available_extensions`, `pg_available_extension_versions` and extension dependencies in `pg_depend`. Exit evidence: PostgreSQL 18.4 oracle suite using an equivalent SQL-only extension installed in the reference image, transactional installation and rollback, and reopen.

### Extension value envelope and host base types

Add the Core `ExtensionValue` envelope, shell and base `CREATE TYPE` bound to host-registered I/O exports, and typed semantic dispatch in every value consumer that can observe an extension value. Exit evidence: the reference rational type implemented through the public registry, differential results against a PostgreSQL 18.4 test extension with the same specification, and consumer audit tests.

### Operators, casts, operator classes and families

Implement `CREATE CAST`, `CREATE OPERATOR`, `CREATE OPERATOR CLASS` and `CREATE OPERATOR FAMILY` with PostgreSQL resolution, and project `pg_cast`, `pg_operator`, `pg_opclass`, `pg_opfamily`, `pg_amop` and `pg_amproc` for implemented objects. Exit evidence: PostgreSQL 18.4 oracle suite for resolution, contexts and diagnostics.

### Comparator indexes and semantic uniqueness

Implement the Storage-owned resumable comparison-based ordered index and semantic uniqueness reservations with SSI coverage and independent writers. Exit evidence: the deterministic schedules in the design's concurrency section on every persistent provider.

### Generic data models

Implement the reserved `uqa_data_model` API, model descriptors, typed scan and command nodes, the resumable runtime protocol and namespaced model records. Exit evidence: the reference time-series model fixture from the design.

### Optimizer extensibility

Implement pure model optimizer interfaces, checked rewrites, access alternatives and costs in normal join enumeration and EXPLAIN. Exit evidence: the ten optimizer gates and the deterministic 1,000,000-row fixture.

### Language SDKs and host bridges

Implement Python, Node.js and browser WASM package builders, typed descriptors, pure adapters, resumable effects and pre-open registries. Exit evidence: each actual artifact authors, persists, reopens and queries a model, type and optimizer hook.

### Migration, wire and restoration

Implement explicit format edges, atomic shadow-generation upgrades, wire I/O for extension types, backup and restore with package requirements, and cross-language reopen. Exit evidence: the lifecycle cases in the design's verification section.

### Public acceptance and documentation

Complete the manual, examples, manifest items and llms.txt links for every implemented public contract, with an independent second model and type. Exit evidence: final policy checks and the manual SQL harness.

## Verification strategy

PostgreSQL behavior is captured with `tests/parity/pg18/run_routines_stateful.py` suites or focused capture scripts against a pinned PostgreSQL 18.4 image; checked-in expected files record the server version and are never regenerated from UQA. Owner tests live in their crates, and public scenarios use each crate's single integration harness. Each implementation change runs the affected owner tests, `python3 scripts/check-workspace-dependencies.py`, `python3 scripts/check-engine-capabilities.py`, `python3 scripts/check-integration-test-harnesses.py`, `bash scripts/check-rust-file-lines.sh`, formatting and Clippy with warnings denied.
