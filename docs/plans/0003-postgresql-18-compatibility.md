# PostgreSQL 18 compatibility plan

Status: Active living plan

Update rule: Update this plan, its synchronized status ledger, the manual, and `tests/parity/pg18/manifest.json` in the same compatibility PR whenever supported behavior or a remaining gate changes.

## Goal

The long-term goal is complete PostgreSQL 18 compatibility at every externally observable boundary: SQL parsing and execution, data types and casts, catalogs, errors, transactions and concurrency, frontend/backend protocol 3.0 and 3.2, and behavior seen by standard PostgreSQL clients. UQA Engine may retain its own internal storage and execution architecture, but an implementation difference is not a reason to expose different PostgreSQL behavior.

This plan distinguishes a PostgreSQL 18 baseline from complete compatibility. The baseline is reached when PostgreSQL 18 grammar, metadata, fixtures, and protocol primitives are authoritative and every unimplemented PostgreSQL 18 shape fails explicitly. Complete compatibility is reached only when the PostgreSQL 18 regression, isolation, protocol, catalog, and client matrices pass without semantic exemptions.

## Non-negotiable rules

- Parser acceptance is not execution support. Every newly accepted AST field must be implemented or rejected before any state change.
- Workarounds are prohibited. A compatibility defect must be fixed at the owning parser, type, catalog, planner, executor, storage, or protocol boundary; source rewriting, silent normalization, test-only behavior, and local dependency overrides are not shippable implementations.
- Silent approximation is a compatibility bug. This includes dropping `RETURNING` aliases, enforcing a `NOT ENFORCED` constraint, selecting a text overload for `bytea`, or omitting catalog state.
- PostgreSQL 18 is the differential oracle. Checked-in fixtures record the exact PostgreSQL server version, architecture, query text, columns, ordered rows, NULLs, value types, errors, and SQLSTATE where applicable.
- Historical PostgreSQL 17 measurements remain historical evidence. Active fixture names, paths, defaults, tests, and compatibility prose move to `pg18`; old benchmark and changelog facts are not relabeled.
- Protocol 3.0 remains supported while protocol 3.2 is added. Version negotiation and unsupported `_pq_.` options must follow PostgreSQL rather than relying on the current libpq default.
- A compatibility claim is no broader than its passing evidence. The manual must identify the current milestone until the complete-compatibility gates pass.
- Add every newly confirmed compatibility gap to the manifest and this plan immediately with an incomplete status; promote it to `verified` only after focused tests and the PostgreSQL 18 differential oracle pass.

## Current implementation status and open PostgreSQL 18 bugs

The historical starting point used `pg_query` 6.1.1 with PostgreSQL 17 grammar, reported `server_version` as `17.0-uqa`, stored the active TPC-H-derived oracle in `expected/pg17.json`, and accepted only frontend/backend protocol 3.0 primitives. Active assets now use `pg18`, session metadata reports `18.0-uqa`, and the checked-in 22-query oracle records PostgreSQL 18.4 server and platform provenance.

The PostgreSQL 18 parser migration replaces the four DML `returning_list` fields with complete `returning_clause` handling. PostgreSQL 18's PL/pgSQL JSON producer also needs to serialize `retvarno` for datum-backed `RETURN` and `RETURN NEXT`; UQA Engine consumes those slots directly and does not rewrite routine source. The reproducible parser chain is imported as the `uqa-pg-query` workspace crate from `jaepil/pg_query.rs@516b3a03fed42e606ce01bc8b5a864a1698c210d` and `jaepil/libpg_query@898cd71c96375d6d4219916996701571dbe2b239`; the latter contains the PostgreSQL 18 parser, corrected PL/pgSQL datum serialization, structured `%TYPE` and `%ROWTYPE` identifier metadata, trigger-promise and type-cache fixes, and a process-wide pthread exit key with a `PTHREAD_KEYS_MAX + 1` regression test so one integration-test executable can safely create parser threads throughout its lifetime.

The following compact ledger is the readable projection of the machine-readable compatibility manifest. Schema version 2 owns milestone names and exit gates, assigns every evidence item to exactly one milestone, derives milestone status from those item states, and synchronizes this ledger and the manual snapshot; pull-request checks reject missing, duplicate, orphaned, or textually drifting accounting.

<!-- pg18-manifest-status:start -->

| Milestone | Name | Status | Exit gate |
| --- | --- | --- | --- |
| `M0` | PG18 baseline | `in_progress` | PG18 parser pinned; all AST deltas audited; unsupported shapes fail explicitly; active names and fixtures use pg18; 22/22 TPC-H-derived results match PostgreSQL 18 |
| `M1` | Discovered semantic fixes | `complete` | Bounded DML row-image, constraint-metadata, identified-function, and independently verified semantic slices pass their PostgreSQL 18.4 evidence |
| `M2` | Protocol 3.2 | `complete` | Byte-exact codec tests and live PostgreSQL 18 libpq 3.0/3.2/latest negotiation and cancellation tests pass |
| `M3` | PG18 DDL and types | `in_progress` | Generated columns, range/multirange, temporal constraints, catalogs, dump/restore, and reopen tests pass |
| `M4` | Core regression parity | `in_progress` | PostgreSQL 18 core regression and isolation suites pass with every remaining failure recorded and reduced to zero |
| `M5` | Client parity | `in_progress` | The supported driver, migration, introspection, dump/restore, COPY, and pooling matrix passes |
| `M6` | Complete compatibility | `not_started` | M0 through M5 and every manifest item are verified, the final zero-exemption audit passes, and the manual removes the implemented-subset qualification |

| Evidence item | Milestone | Status |
| --- | --- | --- |
| `baseline.pg18-differential-probes` | `M0` | `verified` |
| `baseline.tpch-derived-queries` | `M0` | `verified` |
| `parser.pg18-chain` | `M0` | `partial` |
| `query.join-using-natural` | `M3` | `partial` |
| `query.parenthesized-join-alias` | `M1` | `verified` |
| `query.fetch-with-ties` | `M1` | `verified` |
| `query.pattern-escape` | `M1` | `verified` |
| `query.group-by-distinct` | `M1` | `verified` |
| `query.table-function-with-ordinality` | `M1` | `verified` |
| `query.rows-from-and-multi-array-unnest` | `M1` | `verified` |
| `query.named-window` | `M1` | `verified` |
| `query.row-locking-complete-matrix` | `M4` | `partial` |
| `query.cte-search-cycle-and-materialization` | `M4` | `explicitly_rejected` |
| `dml.returning-row-images` | `M1` | `verified` |
| `dml.returning-row-images-extended` | `M4` | `partial` |
| `dml.merge-not-matched-by-source` | `M4` | `explicitly_rejected` |
| `ddl.constraint-metadata` | `M1` | `verified` |
| `ddl.constraint-lifecycle` | `M3` | `partial` |
| `ddl.relation-forms-and-options` | `M3` | `explicitly_rejected` |
| `ddl.ctas-column-names` | `M1` | `verified` |
| `ddl.ctas-with-no-data` | `M1` | `verified` |
| `ddl.select-into` | `M1` | `verified` |
| `ddl.view-column-aliases` | `M1` | `verified` |
| `functions.identified-pg18-additions` | `M1` | `verified` |
| `functions.full-pg18-matrix` | `M4` | `partial` |
| `functions.fixed-builtin-overload-resolution` | `M1` | `verified` |
| `routines.scalar-domain-overload-resolution` | `M1` | `verified` |
| `routines.procedure-call-overload-resolution` | `M1` | `verified` |
| `routines.table-setof-overload-resolution` | `M1` | `verified` |
| `routines.stored-view-function-drop-restrict` | `M1` | `verified` |
| `routines.polymorphic-variadic-pseudotype-overloads` | `M4` | `partial` |
| `routines.function-procedure-drop-cascade` | `M3` | `explicitly_rejected` |
| `functions.json-null-stripping` | `M1` | `verified` |
| `functions.array-transforms` | `M1` | `verified` |
| `functions.integer-base-conversion` | `M1` | `verified` |
| `functions.random-range` | `M1` | `verified` |
| `functions.reverse-overloads` | `M1` | `verified` |
| `functions.md5-overloads` | `M1` | `verified` |
| `functions.crc-checksums` | `M1` | `verified` |
| `functions.gamma-functions` | `M1` | `verified` |
| `functions.string-binary-lengths` | `M1` | `verified` |
| `functions.uuid-extraction` | `M1` | `verified` |
| `execution.static-row-schema-and-spill-v1` | `M3` | `partial` |
| `types.declared-identity-casts-and-catalog` | `M3` | `partial` |
| `ddl.alter-type-and-migration` | `M3` | `partial` |
| `catalog.pg-database-locale` | `M3` | `partial` |
| `plpgsql.datum-slots-and-bound-cursors` | `M4` | `partial` |
| `protocol.frontend-backend-3.2` | `M2` | `verified` |
| `protocol.frontend-backend-3.2-extended` | `M5` | `partial` |
| `ddl.generated-columns` | `M3` | `partial` |
| `ddl.temporal-constraints` | `M3` | `explicitly_rejected` |
| `graph.age-default-label-drop` | `M4` | `explicitly_rejected` |
| `regression.core-and-isolation` | `M4` | `not_audited` |
| `clients.driver-and-operations-matrix` | `M5` | `partial` |
| `compatibility.complete-zero-exemption-audit` | `M6` | `not_audited` |

<!-- pg18-manifest-status:end -->

| Area | Current status | Remaining gate |
| --- | --- | --- |
| Active PG18 baseline | Active paths, scripts, tests, defaults, and fixtures use `pg18`; the parser chain is imported as `uqa-pg-query` from the recorded revisions; 22/22 TPC-H-derived results match PostgreSQL 18.4 | Complete the AST coverage inventory |
| Qualified joins | `JOIN ... USING`, `USING (...) AS alias`, and `NATURAL JOIN` preserve structural AST metadata, bind against both physical row types, resolve the implemented equality/common-type matrix before execution, coerce differently declared keys, implement merged-column ordering and outer-join value selection, preserve input qualification and duplicate non-key output slots, and report PostgreSQL column SQLSTATEs | Complete collations, domains, user-defined operators, and the full PostgreSQL equality/coercion matrix |
| Verified SELECT slices | Parenthesized JOIN aliases, `FETCH ... WITH TIES`, pattern `ESCAPE`, `GROUP BY DISTINCT` and `ALL`, table-function `WITH ORDINALITY`, multi-function `ROWS FROM`, FROM-position multi-array `unnest`, and named `WINDOW` definitions have PostgreSQL 18.4 result, metadata, and SQLSTATE coverage | Continue one independently reviewed and manifested parity slice at a time |
| Row locking | `FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE`, `FOR KEY SHARE`, `OF`, `NOWAIT`, and `SKIP LOCKED` retain row identity through supported scans, joins, subqueries, views, CTE placement, mutations, savepoints, and persistent providers | Complete the upstream row-lock, isolation, process-boundary, and unsupported-relation matrix |
| Derived relation creation | CTAS positional column names, CTAS `WITH NO DATA`, ordinary `SELECT INTO`, and CREATE VIEW positional column names preserve static types, validation ordering, transactionality, and durable reopen behavior | Add temporary and unlogged relation forms and complete upstream DDL/catalog coverage |
| `RETURNING WITH (OLD/NEW ...)` | Old and new images and custom aliases are preserved through SQL AST, planning, and DML execution | Expand live differential coverage to triggers, partitions, and every MERGE action as those features become available |
| Constraint metadata | CHECK and foreign-key enforcement flags and named NOT NULL catalog rows are represented and tested | Complete `NOT VALID`, `ALTER CONSTRAINT`, and all dump/reopen cases |
| Bounded identified PG18 functions and casts | The original bounded array, bytea, Unicode, UUID-generation, checksum, JSON, numeric, interval, Roman-numeral, aggregate, and regular-expression inventory is implemented and differentially verified, including its identified `pg_proc` metadata; separately tracked slices retain their own exact evidence | This bounded inventory is complete; newly discovered gaps are tracked independently and do not reopen it |
| Full PG18 function, operator, cast, and catalog matrix | Dedicated verified evidence items cover the implemented slices | Inventory every PostgreSQL 18 signature and catalog identity and reduce every unverified entry to zero |
| Implemented fixed-signature overload resolution | The documented fixed-signature built-in subset uses one registry and candidate-selection path for ordinary execution and generated-expression typing and binding, including visible SQL user functions, explicit `pg_catalog` qualification, search-path order, exact and implicit matches, preferred and unknown categories, domain base types, named and default slots, declared return types, and durable stored bindings where volatility permits; exact `pg_proc` evidence now includes unit `random()`, `to_hex(integer\|bigint)`, `gen_random_uuid()`, `casefold(text)`, `uuidv4()`, and both `uuidv7` signatures | Polymorphic families retain specialized type substitution; complete the PostgreSQL built-in, operator, cast, extension, and `pg_proc` matrix |
| Scalar SQL routine overload resolution | Ordinary scalar SQL functions retain direct-cast and scalar-subquery declared types through candidate selection, structurally match named and default slots before effective-signature search-path shadowing, preserve typed identities through catalog reopen, prefer exact implemented information-schema domain signatures over their base types, including nested casts that restore the domain at the call boundary, and distinguish unqualified polymorphic function-like syntax from quoted, qualified, and regular search-path-selected routines | Complete polymorphic, variadic, pseudo-type, extension-language, and privilege parity |
| Procedure, `CALL`, and scalar PL/pgSQL overload resolution | Scalar PL/pgSQL functions retain direct-cast and scalar-subquery declarations, while procedures and `CALL` retain direct-cast and concrete PL/pgSQL datum declarations through candidate selection, including typed NULL variables and exact implemented information-schema domains; `CALL` rejects subqueries and distinguishes missing required OUT/TABLE placeholders (`42883`) from a structurally matching wrong-kind function (`42809`) | Complete polymorphic, variadic, pseudo-type, extension-language, and privilege parity |
| Table-returning and `SETOF` overload resolution | Table-returning and `SETOF` functions retain direct-cast and scalar-subquery declared types, exact implemented information-schema domains, named and default slots before effective-signature search-path shadowing, stable scalar-versus-set projection bindings, user-versus-built-in search-path selection, exact DML and correlated LATERAL source bindings, and durable table-routine and stored-view identities across catalog reopen | Complete polymorphic, variadic, pseudo-type, extension-language, and privilege parity |
| Range-function groups and multi-array `unnest` | Explicit `ROWS FROM` preserves independently bound members, anonymous-record member descriptors and type checks, concatenated column order, longest-member zip with NULL padding, outer aliases, group ordinality, implicit LATERAL behavior, and durable stored-view bindings; unqualified multi-array `unnest` in FROM expands to canonical unary `pg_catalog.unnest` members while qualified calls retain ordinary overload resolution | Continue the remaining PostgreSQL table-function signatures, polymorphic carriers, and upstream range-function regression matrix |
| Stored-view function dependencies | Stored scalar, table-function, function-group, CTE, subquery, join, and set-operation plans retain exact user-function identities; `DROP FUNCTION` RESTRICT reports `2BP01` for the referenced overload, leaves unrelated overloads droppable, preflights multi-target drops atomically, combines generated-column and view dependents, and follows `CREATE OR REPLACE VIEW` dependency replacement | Implement `DROP FUNCTION ... CASCADE` and complete dependency-graph parity for every remaining stored object kind |
| Explicitly tracked syntax and lifecycle gaps | Unsupported recursive CTE controls, `MERGE WHEN NOT MATCHED BY SOURCE`, relation forms and options, routine CASCADE, and Apache AGE default-label removal fail explicitly without partial mutation | Implement the exact PostgreSQL 18.4 with Apache AGE behavior and promote each independent manifest item only after live differential evidence |
| PG18 database locale catalog | `pg_database` exposes PostgreSQL 18's builtin provider, `datlocale`, `daticurules`, `datcollversion`, and `dathasloginevt` shape for the engine database, with Unicode behavior tests | Implement the complete database, collation, locale-provider, ownership, ACL, and lifecycle surface |
| PL/pgSQL datum slots and bound cursors | `retvarno`, the `-1` cursor sentinel, bound cursor arguments, named arguments, `OPEN`, `FETCH NEXT`, and `CLOSE` are structural AST and interpreter state backed by the pinned parser revisions; scalar and cursor-argument `SelectStmt` envelopes reject unsupported structure; qualified named types and `%TYPE` references resolve against actual table metadata and every ordinary assignment/return coercion propagates SQL cast errors | Add session portal state before supporting refcursor parameters, returns, or cursors surviving routine exit |
| Protocol 3.2 | Byte-exact tests cover minor negotiation, ordered `_pq_.` reporting, message tag `v`, variable cancellation keys, legacy 3.0 validation, FunctionCall, GSS/SSPI authentication messages, notifications, COPY format validation, and the PostgreSQL 18 reserved-3.1 edge; PostgreSQL 18.4 `psql`/libpq live tests cover 3.0, 3.2, `latest`, 3.2-to-3.0 downgrade, authentication ordering, SSL rejection and retry, both cancellation-key shapes, and extended Parse/Bind/Describe/Execute/Sync flow | Add a credentialed Kerberos environment, non-trust authentication exchanges, binary-format coverage, and the wider driver matrix |
| Static row types and spill format | Scans, projections, filters, joins, CTEs, aggregates, windows, DML `RETURNING`, cursors, foreign tables, and empty virtual relations carry declared `ColumnType` metadata without reconstructing it from runtime values; materialization remains at the final consumer boundary; spill format version 1 stores logical `(alias, column)` identities and declared schemas without a compatibility reader | Extend exact static typing to every remaining expression/operator and persistent relation kind |
| Declared types, casts, and catalogs | Integer widths, OID/XID, floating widths, character variants, UUID, temporal types, arrays, domains, foreign schemas, and migrations retain exact identities; source-sensitive OID/XID/bytea casts preserve declared width and PostgreSQL SQLSTATEs; legacy `int2vector` and `oidvector` values retain their declared type through text casts and emit PostgreSQL's space-separated text and COPY representation; `pg_type` exposes PostgreSQL 18 layouts and I/O routine OIDs for implemented built-ins, domains, `record`, `_record`, `void`, and `information_schema_catalog_name` together with its `pg_class`/`pg_attribute` identity | Complete all built-in and extension type I/O routines in `pg_proc`, composite/domain constraints, collations, enums, ranges, typmods, and binary formats |
| `ALTER COLUMN TYPE` and migration | `USING` remains structural AST and is evaluated against every old row inside the atomic ALTER transaction; source-sensitive implicit casts retain the old declared type; failed rewrites roll back schema and data; migration preserves supported scalar widths and rejects unknown source types instead of converting them to text | Complete the PostgreSQL assignment-cast matrix, dependency rewrites, domain checks, collation changes, and every `ALTER TYPE` regression case |
| Virtual and stored generated columns | Core definition, durable reopen, dependency rewrites, selective virtual evaluation, exactly-once stored evaluation, DML row images, DDL-time static typing for the implemented expression surface, exact stored SQL routine overload binding and dependencies, supported constraints and indexes, catalogs, ALTER operations, and failure atomicity are implemented and covered by the consolidated engine integration executable | Complete the PostgreSQL built-in function and operator overload matrix, privileges, inheritance and partition propagation, dump/restore, and the upstream regression cases |
| `md5(text\|bytea)` overloads | Exact text/bytea overload resolution occurs before runtime carrier erasure, text hashes its UTF-8 bytes, bytea hashes its raw payload, unsupported signatures report SQLSTATE `42883`, stored generated expressions retain the selected built-in or user binding, and `pg_proc` exposes the strict, immutable, parallel-safe, leakproof OIDs 2311 and 2321 | Continue the independently manifested PostgreSQL 18 function, operator, type, and catalog matrix |
| `crc32(bytea)` and `crc32c(bytea)` checksums | Bytea-only overload resolution occurs before runtime carrier erasure, unknown inputs acquire bytea context, both algorithms return unsigned 32-bit values as `bigint`, unsupported and ambiguous signatures report SQLSTATEs `42883` and `42725`, stored generated expressions retain the selected built-in or user binding across reopen, and `pg_proc` exposes the strict, immutable, parallel-safe, leakproof OIDs 6364 and 6365 | Continue the independently manifested PostgreSQL 18 function, operator, type, and catalog matrix |
| `gamma(double precision)` and `lgamma(double precision)` | Native execution follows PostgreSQL's host C math-library contract, numeric inputs bind through the exact float8 signature, poles and range failures preserve SQLSTATE `22003`, unsupported and ambiguous signatures preserve SQLSTATEs `42883` and `42725`, user overloads obey implicit and explicit `pg_catalog` search order, stored generated expressions retain the selected binding across reopen, and `pg_proc` exposes exact OIDs 6383 and 6384 | Continue the independently manifested PostgreSQL 18 function, operator, type, and catalog matrix |
| One-argument string and binary length overloads | The twelve PostgreSQL 18.4 `length`, `char_length`, `character_length`, `octet_length`, and `bit_length` overloads for text, character, and bytea preserve Unicode character counts, blank-padding rules, UTF-8 and raw byte counts, strict NULL, integer results, preferred text context, scalar-subquery and user-overload resolution, durable generated-expression bindings, and exact `pg_proc` rows including the two `bit_length` SQL bodies | Continue the separately manifested encoding-aware, bit-string, geometric, path, and text-search length overload families |
| Temporal constraints | Open compatibility bug; range and multirange carriers are absent | Implement the carrier, operator, index, and constraint layers before accepting the syntax |

## Workstreams

### 1. PostgreSQL 18 parser and AST safety

Use the reviewed PostgreSQL 18 parser chain imported as `uqa-pg-query` from `jaepil/pg_query.rs@516b3a03fed42e606ce01bc8b5a864a1698c210d` and `jaepil/libpg_query@898cd71c96375d6d4219916996701571dbe2b239`. The wrapper exposes PostgreSQL's raw parser modes directly, so PL/pgSQL expressions and one-, two-, and three-part assignments are parsed structurally without rewriting input text, and PL/pgSQL `%TYPE` and `%ROWTYPE` declarations retain each normalized identifier component separately from their display spelling. The C library creates its pthread destructor key exactly once per process and associates each thread's parser memory context with that shared key, preventing key exhaustion without splitting test executables or pre-initializing unrelated libraries. Any future parser update must be reviewed, tested in both repositories, pushed first, and then adopted through `scripts/sync-uqa-pg-query.py` with a new recorded revision, checksum list, and regenerated `Cargo.lock`; native, Python, Node.js, Browser WASM, and supported-platform builds remain required because the dependency contains C code and generated protobuf types.

All four DML paths now consume the complete `ReturningClause` instead of the removed `returning_list` fields. Remaining parser-baseline work is to audit every changed protobuf message and field between PostgreSQL 17 and 18, especially constraint enforcement, generated-column kind, temporal key flags, MERGE variants, COPY options, and utility statements, with compiler tests proving each unsupported field fails before catalog or storage mutation.

Add an AST coverage inventory that maps every PostgreSQL 18 top-level statement and every semantically relevant option to implemented, explicitly rejected, or not-yet-audited status. The `not-yet-audited` count must be zero for the baseline milestone, and the explicitly rejected count must be zero for complete compatibility.

### 2. Active compatibility baseline and fixture rename

The active compatibility assets and live references were renamed atomically:

| Old | New |
| --- | --- |
| `scripts/run-tpch-pg17.py` | `scripts/run-tpch-pg18.py` |
| `benchmarks/tpch/expected/pg17.json` | `benchmarks/tpch/expected/pg18.json` |
| `tests/parity/pg17/` | `tests/parity/pg18/` |
| `crates/uqa-engine/tests/pg17_semantics.rs` | `crates/uqa-engine/tests/pg18_semantics.rs` |
| `target/benchmark-runs/tpch-pg17.json` | `target/benchmark-runs/tpch-pg18.json` |
| `uqa-tpch-pg17` and `uqa-pg17-age` defaults | PostgreSQL 18-specific defaults |

Fixture loaders, test modules and functions, assertion text, manifest provenance, current README and manual sections, container images, output labels, and `server_version = 18.0-uqa` now use the PostgreSQL 18 baseline. Version-neutral environment variable names such as `UQA_TPCH_PG_CONTAINER` remain stable, while historical `HISTORY.md` entries and dated PostgreSQL 17 performance snapshots remain historical rather than being relabeled.

Regenerate active expected data only after the script confirms a live PostgreSQL 18 server and every UQA result matches it. Commit the exact PostgreSQL 18 patch version in the fixture and manifest.

### 3. PG18 DML row images

`RETURNING` row-image aliases are represented in the SQL AST and plan instead of being flattened into ordinary projections. INSERT exposes a NULL old image and the inserted new image, DELETE exposes the deleted old image and a NULL new image, UPDATE retains both versions, and the implemented MERGE actions select their corresponding images while retaining source columns and `merge_action()` behavior.

Default `old` and `new` qualification and `WITH (OLD AS ..., NEW AS ...)` renaming are implemented, including conflicts with table aliases and user columns. Remaining work is live differential coverage for `*`, qualified stars, expressions combining both images, triggers, CTEs, partitions, and every action of all four DML statements as those owning features become available.

### 4. PG18 constraints and generated columns

Extend catalog definitions so constraints preserve name, type, enforcement, validation, inheritance, temporal flags, referenced columns, and expression. Runtime validation must consult those flags rather than inferring behavior from the presence of a compiled expression.

Implement named `NOT NULL` as a first-class constraint with `pg_constraint.contype = 'n'`, while keeping `pg_attribute.attnotnull` consistent. Implement `NOT ENFORCED` for CHECK and foreign keys, `NOT VALID` lifecycle, `ALTER CONSTRAINT`, and dump/reopen behavior. Add failure-atomicity tests proving rejected DDL leaves no partial metadata.

Virtual and stored generated columns now persist the generation expression and kind, reject direct writes except PostgreSQL-supported `DEFAULT` forms, evaluate virtual columns only when a projection or enforced constraint requires them without physical storage, maintain stored columns exactly once at the prepared-write boundary, reject generated-to-generated references, statically type the implemented expression surface before catalog mutation, bind and persist the exact stored SQL routine overload used for evaluation and dependency checks, rewrite relation and column dependencies, expose `attgenerated`, `pg_attrdef`, and information-schema metadata, and preserve definitions across storage reopen and schema changes. Remaining work is the complete PostgreSQL built-in function and operator overload matrix, privileges, inheritance and partition propagation, dump/restore, and the complete upstream regression matrix.

Implement range and multirange types before temporal constraints. Add comparison, containment, overlap, emptiness, canonicalization, casts, text and binary I/O, GiST-compatible or semantically equivalent enforcement support, and exclusion behavior. Then implement `WITHOUT OVERLAPS` primary/unique keys and `PERIOD` foreign keys with PostgreSQL-equivalent empty-range and referential-action rules.

### 5. PG18 functions, casts, collations, and output

The bounded original PostgreSQL 18 function-and-cast inventory is implemented and differentially verified: `array_sort`, `array_reverse`, `reverse(bytea)`, identified integer/`bytea` casts, Unicode 16 full case mapping, `casefold`, `json_strip_nulls` and `jsonb_strip_nulls` array-null options, `uuidv4`, `uuidv7`, `crc32`, `crc32c`, `gamma`, `lgamma`, interval week and negative-quarter extraction, Roman-numeral parsing, array/composite aggregates, and named regular-expression arguments. This closes only `functions.identified-pg18-additions`; `functions.full-pg18-matrix` and `routines.polymorphic-variadic-pseudotype-overloads` remain incomplete, while PL/pgSQL cursor work remains under `plpgsql.datum-slots-and-bound-cursors`. The `array_sort(anyarray [, boolean [, boolean]])` and `array_reverse(anyarray)` discovery is independently verified with first-dimension multidimensional behavior, preserved bounds, concrete base-array return types and array-domain flattening, declaration-order-independent named arguments, unknown literal and bare-parameter Boolean context, explicit non-Boolean rejection, concrete user-overload ranking and ambiguity, polymorphic and undefined-function errors, scalar-subquery and generated-column typing, comparator failures for `json` and nested composites, and exact OIDs 6381 and 6388 through 6390. The `reverse(text)` and PostgreSQL 18 `reverse(bytea)` overload pair is independently verified with Unicode and raw-byte results, preferred `text` resolution for unknown literals, NULL, and untyped parameters, character-family implicit casts, scalar-subquery typing, exact undefined-function errors for invalid types, names, and arities, concrete user-overload ranking with explicit and implicit `pg_catalog` search order, stored generated-expression binding, and exact OIDs 3062 and 6382. The `md5(text|bytea)` slice shares that exact overload-resolution boundary while always returning `text`; it hashes raw `bytea` payloads, rejects unrelated types and invalid arities as undefined functions, persists generated-expression bindings, and exposes PostgreSQL OIDs 2311 and 2321 with their leakproof metadata. The subsequent `uuid_extract_version(uuid)` and `uuid_extract_timestamp(uuid)` discovery is implemented as its own verified parity slice with declared UUID overload resolution, PostgreSQL 18 version 1 and 7 timestamp conversion, exact return types and errors, immutable generated-column support, and `pg_proc` metadata. The `to_bin(integer|bigint)` and `to_oct(integer|bigint)` slice likewise preserves the declared 32-bit or 64-bit width through execution, scalar-subquery output binding, and generated-expression validation, emits PostgreSQL's two's-complement text for negative values, and exposes OIDs 6330 through 6333 with exact `pg_proc` metadata. The `random(integer,integer)`, `random(bigint,bigint)`, and `random(numeric,numeric)` slice uses PostgreSQL's shared xoroshiro128** stream and exact inclusive sampling algorithms, preserves overloads and arbitrary-precision numeric scale, keeps consumed draws and reseeding nontransactional across statement, transaction, and savepoint rollback, rejects invalid bounds and generated-column use with PostgreSQL SQLSTATEs, and exposes OIDs 6339 through 6341 with their strict, volatile, parallel-restricted `pg_proc` metadata; future discoveries must remain independently accounted instead of being hidden inside the broad inventory.

The documented fixed-signature subset now uses one registry and resolver for ordinary execution and generated-expression typing and binding. The shared path covers the listed case-folding, reverse, hash, checksum, one-argument string and binary length, gamma, JSON null-stripping, integer-base conversion, random, and UUID generation and extraction signatures together with visible SQL user-function candidates, qualification, search-path order, exact and implicit matches, preferred and unknown categories, domain base types, named and default slots, declared return types, and durable stored bindings where volatility permits. Consolidated catalog tests verify the PostgreSQL 18 `pg_proc` identities for unit `random()` (1598), `to_hex(integer|bigint)` (2089 and 2090), `gen_random_uuid()` (3432), `casefold(text)` (6412), `uuidv4()` (6428), and `uuidv7()` / `uuidv7(interval)` (6429 and 6430). Polymorphic array functions retain specialized type substitution, and this verified slice does not claim PostgreSQL's complete built-in, operator, cast, extension, or `pg_proc` matrix.

Ordinary scalar SQL function overload resolution is independently verified for direct and scalar-subquery declared integer widths, named and default argument filtering before effective-signature search-path shadowing, typed identity across catalog reopen, exact information-schema domain selection over its base type, and nested casts that restore the domain at the call boundary. Compiler and generated-column evidence also distinguishes unqualified `COALESCE`, `GREATEST`, `LEAST`, and `NULLIF` syntax from quoted or schema-qualified calls and preserves search-path selection for ordinary names such as `upper` and `concat`. Focused evidence is isolated in `sql_routine_identity::scalar_overloads`, compiler syntax-binding tests, and generated fixed-binding tests; procedure, `CALL`, PL/pgSQL datum, table-routine, and `SETOF` evidence is tracked in the adjacent ledger items.

Procedure, `CALL`, and scalar PL/pgSQL overload resolution is independently verified for PostgreSQL's implicit preference of `double precision` over `numeric` for integer input, scalar-function widths from direct casts and scalar subqueries, procedure widths from direct casts, named and default arguments, concrete PL/pgSQL datum declarations and typed NULL variables, and exact information-schema domain selection for `%TYPE` variables. `CALL` rejects subqueries, and its wrong-kind evidence covers functions with OUT and TABLE parameters without exercising their table-returning execution: omission of the required output placeholder is undefined procedure (`42883`), while a structurally matching call that supplies the placeholder is wrong object type (`42809`).

Table-returning and `SETOF` function overload resolution is independently verified for direct and scalar-subquery declared integer widths, unknown-category selection and ambiguity, exact information-schema domain selection, named and default arguments before effective-signature search-path shadowing, and typed table-routine identity across catalog reopen. Exact table-function source bindings reach UPDATE, DELETE, MERGE, and correlated LATERAL execution, and stored views serialize each selected identity across reopen. `DROP FUNCTION` RESTRICT checks those exact stored-view identities before mutation, reports dependent-objects-still-exist (`2BP01`) for the referenced overload, leaves unrelated overloads droppable, and preflights multi-target drops atomically; scalar expressions and table-function groups nested through CTEs, subqueries, joins, set operations, and scalar subqueries participate in the same dependency scan, and `CREATE OR REPLACE VIEW` replaces the prior dependency set. Select-list projection keeps scalar and set-returning overloads distinct and preserves the selected signature through execution, while visible user `SETOF` and ordinary single-argument `unnest` overloads and their built-in namesakes follow search-path order without inheriting built-in row-shape handling. Focused evidence is isolated in `sql_routine_identity::table_routine_*`, `sql_routine_identity::scalar_view_function_dependencies_are_exact_replaceable_and_drop_atomic`, `sql_routine_identity::view_function_dependency_scan_covers_function_groups_and_nested_query_shapes`, `sql_routine_identity::multi_argument_from_unnest_uses_postgresql_syntax_before_user_overloads`, `sql_table_functions::table_function_scalar_subqueries_preserve_declared_argument_types`, `sql_plpgsql::set_returning::set_projection_*`, and `pg18_semantics::md5_overloads::pg18_set_returning_user_overload_uses_the_combined_stable_binding`.

The independently verified range-function slice represents explicit `ROWS FROM` as one structural group whose members retain individual function identities, arguments, declared columns, and stored bindings while the outer alias, positional aliases, and `WITH ORDINALITY` apply to the concatenated output. Execution zips member streams to the longest cardinality, NULL-pads exhausted members, preserves declaration-order columns, appends one one-based `bigint` ordinality column, resets ordinality for each correlated invocation, and follows implicit and explicit LATERAL semantics including left-join null extension. Typed member column definitions are required for anonymous `record` and `SETOF record` routines, preserve SQL and PL/pgSQL composite fields, validate source-to-target type compatibility and field counts, apply compatible coercions and type modifiers, and distinguish non-record results from known OUT-shaped records with PostgreSQL's `42601` errors. PostgreSQL's FROM-position transform for unqualified multi-argument `unnest` expands each array argument into a canonical unary `pg_catalog.unnest` member before ordinary user-overload selection; explicit `ROWS FROM(unnest(a, b))` uses the same transform, while unqualified singleton members resolve normally, schema-qualified multi-argument calls remain ordinary functions, `pg_catalog.unnest(a, b)` is undefined (`42883`), and select-list calls do not receive the FROM-only transform. Focused evidence is isolated in the compiler and planner range-function tests, `sql_table_functions::rows_from_*`, `sql_routine_identity::multi_argument_from_unnest_uses_postgresql_syntax_before_user_overloads`, and the PostgreSQL 18.4 with Apache AGE live differential matrix.

The `crc32(bytea)` and `crc32c(bytea)` checksum slice is independently verified with exact raw-byte results, unsigned 32-bit values represented as `bigint`, bytea inference for unknown literals and untyped parameters, strict NULL propagation, scalar-subquery typing, undefined-function and cross-schema ambiguity errors, concrete user-overload ranking with explicit and implicit `pg_catalog` order, durable generated-expression bindings, and exact OIDs 6364 and 6365 including leakproof metadata.

The `gamma(double precision)` and `lgamma(double precision)` slice is independently verified against PostgreSQL 18.4 on the same native C math library, including exact stable results and platform-owned last-bit behavior, implicit numeric conversion to float8, strict NULL, infinity and NaN handling, pole, overflow, and underflow SQLSTATEs, invalid-input and undefined-function errors, scalar-subquery typing, combined built-in and user-overload ranking with implicit and explicit `pg_catalog` order, durable generated-expression bindings, and exact OIDs 6383 and 6384 with their full `pg_proc` and information-schema metadata.

The `json_strip_nulls(json [, boolean])` and `jsonb_strip_nulls(jsonb [, boolean])` slice is independently verified against PostgreSQL 18.4 with recursive object-null removal, the default retention and optional removal of array nulls, strict NULL, declaration-order-independent `target` and `strip_in_arrays` names, unknown and domain typing, scalar subqueries, exact undefined and ambiguous function errors, effective-signature shadowing by user overloads under implicit and explicit `pg_catalog` order, immutable generated-expression bindings across reopen, exact OIDs 3261 and 3262, textual `json` order, duplicate-key, decoded-string, and numeric-lexeme preservation, and canonical `jsonb` output.

For each function, add exact overload resolution, return type metadata, strictness and volatility, NULL propagation, boundary and error behavior, text and binary result encoding, and `pg_proc` visibility. Differential cases must include Unicode, empty arrays, multidimensional arrays, NaN and infinity, malformed JSON, invalid formats, and type ambiguity.

Preserve declared source and result types through the physical plan instead of inferring them from shared runtime carriers. OID, XID, bytea, narrowing integer, character, domain, array, and assignment casts must resolve from the declared source type, emit PostgreSQL 18 SQLSTATEs, and retain exact metadata through scans, joins, CTEs, spills, DML, cursors, and wire row descriptions. `ALTER COLUMN TYPE USING` is an executable expression over the old row, not ignorable syntax, and migration must reject unknown type identities.

Audit PostgreSQL 18 migration changes that overlap UQA Engine, including timezone-abbreviation precedence, CSV `COPY` end markers, inheritance-aware `VACUUM` and `ANALYZE`, full-text collation behavior, and EXPLAIN output. Server-internal AIO and optimizer implementation changes do not need identical internals, but their externally visible plans, errors, and results remain differential targets.

### 6. Frontend/backend protocol 3.2

Add protocol constants and a negotiation API that accepts major version 3 startup packets, selects the highest supported minor version not greater than the request or the embedding server's explicit maximum, reports every unsupported `_pq_.` option, and encodes `NegotiateProtocolVersion` with message tag `v`. A request for an unsupported major version remains a protocol error. The embedding server's implementation maximum must be 3.0 or 3.2; PostgreSQL 18 nevertheless accepts a frontend 3.1 request unchanged and echoes selected version 3.1 when it must report an unsupported `_pq_.` option, while 3.2 enables variable-length keys. PostgreSQL 18 libpq never requests 3.1 and rejects a 3.2-to-3.1 downgrade.

Represent cancellation secrets as validated opaque bytes. Protocol 3.0 requires exactly four bytes. Under PostgreSQL 18 protocol 3.2, `BackendKeyData` emits 4 through 256 bytes and the server's `CancelRequest` parser accepts 1 through 256 bytes. Preserve zero bytes, enforce the distinct message boundaries, and provide an explicit four-byte constructor for existing integrations.

Byte-exact unit tests cover PostgreSQL 18 message formats, downgrade responses, unsupported `_pq_.` options, malformed lengths, SSL/GSS pre-startup requests, and cancellation-key boundaries. Ignored live interoperability tests use PostgreSQL 18.4 `psql` as a thin libpq driver and a server assembled directly from `uqa-pg-wire` codecs; they verify `max_protocol_version=3.0`, `3.2`, and `latest`, an explicit 3.2-to-3.0 server downgrade, authentication sequencing, SSL rejection and startup retry, legacy cancellation, and a 256-byte protocol 3.2 cancellation key.

Complete the remaining protocol evidence with a credentialed Kerberos environment that actually emits `GSSEncRequest`, non-trust authentication exchanges, extended-query binary parameter/result formats, layered middleware keys near the 256-byte limit, malformed live peers, and the client matrix in workstream 8.

### 7. Complete SQL, catalog, and transaction compatibility

Drive remaining work from the PostgreSQL 18 official regression schedules rather than an ad hoc feature list. Import queries and expected behavior in license-compatible differential harnesses, categorize failures by parser, binder, type system, planner, executor, catalog, transaction, protocol, or administration, and maintain a burn-down manifest with owners and evidence.

Close the existing embedded-runtime gaps: PostgreSQL integer widths and overflow, numeric precision and formatting, collations, domains, enums, composite and range types, temporary and unlogged relations, materialized views, inheritance and partitioning, complete row-lock and isolation regression coverage, triggers and rules, sequences, COPY, complete system catalogs, roles and ACLs, MVCC snapshots and deadlock behavior, prepared statements and portals, large objects, extensions, replication-facing protocol, WAL, and administration surfaces.

Use PostgreSQL 18 SQLSTATE, primary error text where clients depend on it, transaction-abort behavior, command tags, row descriptions, OIDs, typmods, binary formats, and catalog visibility as part of the contract. Performance optimizations may differ but must not alter these observations.

Keep row schemas as static plan metadata from the first scan through the last consumer. Materialize only where a final API or blocking physical operator requires owned rows; spilling does not justify materializing identity into ad hoc field names. Spill format version 1 records the declared schema and logical `(alias, column)` identity directly, and no legacy compatibility reader is maintained.

### 8. Client and operational compatibility

Run a versioned client matrix covering PostgreSQL 18 `psql`, libpq, JDBC, Npgsql, psycopg, pgx, node-postgres, SQLAlchemy, common migration tools, `pg_dump`, `pg_restore`, and schema introspection. Exercise simple and extended query protocols, binary parameters and results, prepared statement reuse, COPY, cancellation, notices, errors, transactions, and connection-pool reset behavior.

Add long-running concurrency, restart, crash-recovery, backup/restore, and upgrade tests. A full PostgreSQL compatibility claim requires operationally observable behavior, not only an in-memory SQL test suite.

## Milestones and exit gates

Milestone names, exit gates, evidence ownership, and derived states are defined once in manifest schema version 2 and rendered in the synchronized ledger above. A milestone is `complete` only when every owned item is `verified`, `not_started` only when every owned item is `not_audited`, and otherwise `in_progress`; M6 additionally requires M0 through M5 and every manifest item to be complete.

## Required verification on every compatibility change

Run `cargo fmt --all --check`, focused owning-crate tests, the relevant Docker PostgreSQL 18.4 with Apache AGE oracle matrix, and manifest validation while iterating. Do not dispatch the full cross-platform suite for intermediate commits.

```sh
cargo fmt --all --check
python3 tests/parity/pg18/run_diff.py --validate-manifest
```

After implementation and review changes converge, push the final pull-request head and run `bash scripts/run-premerge-ci.sh` exactly once. The dispatcher selects Rust, JavaScript/WebAssembly, and Python suites from the final diff; any later push invalidates that result and requires one replacement run for the new head.

Live wire verification requires `UQA_PG18_DOCKER_HOST` to name the test server host as reachable from the client container; this is explicit because the correct address differs between Docker Desktop, native Linux Docker, and remote container runtimes. It uses a PostgreSQL 18 client container named `pg-parity` by default, and `UQA_PG18_WIRE_CONTAINER` selects another container.

Run repository policy scripts, binding builds and examples, and supported-platform CI whenever the parser dependency, public AST, catalog serialization, value representation, or wire types change.

## Completion accounting

Maintain the machine-readable PostgreSQL 18 compatibility manifest at `tests/parity/pg18/manifest.json` alongside the differential harness. Schema version 2 records each milestone title and exit gate, assigns every item to exactly one owning milestone, and records each item's PostgreSQL reference, UQA Engine test, supported boundary, status, and open issue. Every compatibility PR must update its manifest evidence, this plan's narrative, the generated ledger above, and the authoritative manual together. `run_diff.py --validate-manifest` rejects invalid ownership, derived-status contradictions, plan or manual drift, and any complete-compatibility claim before M6 is derived complete. Milestone completion requires positive evidence for every owned row; absence of a failing test is not evidence.

The final complete-compatibility audit must inspect the current parser revision, manifest, test results, live server provenance, protocol traces, client matrix, catalog diffs, persistent reopen behavior, and manual. The project must not declare complete PostgreSQL 18 compatibility while any item is missing, explicitly rejected, silently approximated, or supported only by an indirect test.
