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
- Add a newly confirmed compatibility gap to the manifest and this plan with an incomplete status when its implementation starts; promote it to `verified` only after focused tests and the PostgreSQL 18 differential oracle pass.

## Current implementation status and open PostgreSQL 18 bugs

The historical starting point used `pg_query` 6.1.1 with PostgreSQL 17 grammar, reported `server_version` as `17.0-uqa`, stored the active TPC-H-derived oracle in `expected/pg17.json`, and accepted only frontend/backend protocol 3.0 primitives. Active assets now use `pg18`, session metadata reports `18.0-uqa`, and the checked-in 22-query oracle records PostgreSQL 18.4 server and platform provenance.

The PostgreSQL 18 parser migration replaces the four DML `returning_list` fields with complete `returning_clause` handling. PostgreSQL 18's PL/pgSQL JSON producer also needs to serialize `retvarno` for datum-backed `RETURN` and `RETURN NEXT`; UQA Engine consumes those slots directly and does not rewrite routine source. The reproducible parser chain is imported as the `uqa-pg-query` workspace crate from `jaepil/pg_query.rs@516b3a03fed42e606ce01bc8b5a864a1698c210d` and `jaepil/libpg_query@898cd71c96375d6d4219916996701571dbe2b239`; the latter contains the PostgreSQL 18 parser, corrected PL/pgSQL datum serialization, structured `%TYPE` and `%ROWTYPE` identifier metadata, trigger-promise and type-cache fixes, and a process-wide pthread exit key with a `PTHREAD_KEYS_MAX + 1` regression test so one integration-test executable can safely create parser threads throughout its lifetime.

The following compact ledger is the readable projection of the machine-readable compatibility manifest. Pull-request checks compare every milestone and evidence-item status, so adding or changing manifest accounting without updating this plan fails immediately.

<!-- pg18-manifest-status:start -->

| Milestone | Status |
| --- | --- |
| `M0` | `in_progress` |
| `M1` | `complete` |
| `M2` | `complete` |
| `M3` | `not_started` |
| `M4` | `not_started` |
| `M5` | `not_started` |
| `M6` | `not_started` |

| Evidence item | Status |
| --- | --- |
| `baseline.pg18-differential-probes` | `verified` |
| `baseline.tpch-derived-queries` | `verified` |
| `parser.pg18-chain` | `partial` |
| `query.join-using-natural` | `partial` |
| `query.parenthesized-join-alias` | `verified` |
| `query.fetch-with-ties` | `verified` |
| `query.pattern-escape` | `verified` |
| `query.group-by-distinct` | `verified` |
| `query.table-function-with-ordinality` | `verified` |
| `query.named-window` | `verified` |
| `dml.returning-row-images` | `partial` |
| `ddl.constraint-metadata` | `partial` |
| `ddl.ctas-column-names` | `verified` |
| `ddl.ctas-with-no-data` | `verified` |
| `ddl.select-into` | `verified` |
| `ddl.view-column-aliases` | `verified` |
| `functions.identified-pg18-additions` | `partial` |
| `functions.array-transforms` | `verified` |
| `functions.integer-base-conversion` | `verified` |
| `functions.random-range` | `verified` |
| `functions.reverse-overloads` | `verified` |
| `functions.md5-overloads` | `verified` |
| `functions.string-binary-lengths` | `verified` |
| `functions.uuid-extraction` | `verified` |
| `execution.static-row-schema-and-spill-v1` | `partial` |
| `types.declared-identity-casts-and-catalog` | `partial` |
| `ddl.alter-type-and-migration` | `partial` |
| `catalog.pg-database-locale` | `partial` |
| `plpgsql.datum-slots-and-bound-cursors` | `partial` |
| `protocol.frontend-backend-3.2` | `partial` |
| `ddl.generated-columns` | `partial` |
| `ddl.temporal-constraints` | `explicitly_rejected` |
| `regression.core-and-isolation` | `not_audited` |
| `clients.driver-and-operations-matrix` | `partial` |

<!-- pg18-manifest-status:end -->

| Area | Current status | Remaining gate |
| --- | --- | --- |
| Active PG18 baseline | Active paths, scripts, tests, defaults, and fixtures use `pg18`; the parser chain is imported as `uqa-pg-query` from the recorded revisions; 22/22 TPC-H-derived results match PostgreSQL 18.4 | Complete the AST coverage inventory |
| Qualified joins | `JOIN ... USING`, `USING (...) AS alias`, and `NATURAL JOIN` preserve structural AST metadata, bind against both physical row types, resolve the implemented equality/common-type matrix before execution, coerce differently declared keys, implement merged-column ordering and outer-join value selection, preserve input qualification and duplicate non-key output slots, and report PostgreSQL column SQLSTATEs | Complete collations, domains, user-defined operators, and the full PostgreSQL equality/coercion matrix |
| Verified SELECT slices | Parenthesized JOIN aliases, `FETCH ... WITH TIES`, pattern `ESCAPE`, `GROUP BY DISTINCT` and `ALL`, table-function `WITH ORDINALITY`, and named `WINDOW` definitions have PostgreSQL 18.4 result, metadata, and SQLSTATE coverage | Continue one independently reviewed and manifested parity slice at a time |
| Row locking | `FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE`, `FOR KEY SHARE`, `OF`, `NOWAIT`, and `SKIP LOCKED` retain row identity through supported scans, joins, subqueries, views, CTE placement, mutations, savepoints, and persistent providers | Complete the upstream row-lock, isolation, process-boundary, and unsupported-relation matrix |
| Derived relation creation | CTAS positional column names, CTAS `WITH NO DATA`, ordinary `SELECT INTO`, and CREATE VIEW positional column names preserve static types, validation ordering, transactionality, and durable reopen behavior | Add temporary and unlogged relation forms and complete upstream DDL/catalog coverage |
| `RETURNING WITH (OLD/NEW ...)` | Old and new images and custom aliases are preserved through SQL AST, planning, and DML execution | Expand live differential coverage to triggers, partitions, and every MERGE action as those features become available |
| Constraint metadata | CHECK and foreign-key enforcement flags and named NOT NULL catalog rows are represented and tested | Complete `NOT VALID`, `ALTER CONSTRAINT`, and all dump/reopen cases |
| Identified PG18 functions and casts | The original array, bytea, Unicode, UUID generation, checksum, JSON, numeric, interval, Roman-numeral, aggregate, and regex inventory is implemented and tested, including its identified `pg_proc` overload metadata; separately tracked slices cover `array_sort` / `array_reverse`, `reverse(text\|bytea)`, `md5(text\|bytea)`, the one-argument string and binary length family, UUID extraction, the `to_bin(integer\|bigint)` / `to_oct(integer\|bigint)` overloads, and the three `random(min,max)` overloads with exact results, scalar-subquery typing, errors, generated-column behavior, session-state semantics where applicable, user-overload ranking, and catalog metadata | Continue expanding the PostgreSQL 18 function, type, overload, and catalog matrix one independently verified slice at a time |
| PG18 database locale catalog | `pg_database` exposes PostgreSQL 18's builtin provider, `datlocale`, `daticurules`, `datcollversion`, and `dathasloginevt` shape for the engine database, with Unicode behavior tests | Implement the complete database, collation, locale-provider, ownership, ACL, and lifecycle surface |
| PL/pgSQL datum slots and bound cursors | `retvarno`, the `-1` cursor sentinel, bound cursor arguments, named arguments, `OPEN`, `FETCH NEXT`, and `CLOSE` are structural AST and interpreter state backed by the pinned parser revisions; scalar and cursor-argument `SelectStmt` envelopes reject unsupported structure; qualified named types and `%TYPE` references resolve against actual table metadata and every ordinary assignment/return coercion propagates SQL cast errors | Add session portal state before supporting refcursor parameters, returns, or cursors surviving routine exit |
| Protocol 3.2 | Byte-exact tests cover minor negotiation, ordered `_pq_.` reporting, message tag `v`, variable cancellation keys, legacy 3.0 validation, FunctionCall, GSS/SSPI authentication messages, notifications, COPY format validation, and the PostgreSQL 18 reserved-3.1 edge; PostgreSQL 18.4 `psql`/libpq live tests cover 3.0, 3.2, `latest`, 3.2-to-3.0 downgrade, authentication ordering, SSL rejection and retry, both cancellation-key shapes, and extended Parse/Bind/Describe/Execute/Sync flow | Add a credentialed Kerberos environment, non-trust authentication exchanges, binary-format coverage, and the wider driver matrix |
| Static row types and spill format | Scans, projections, filters, joins, CTEs, aggregates, windows, DML `RETURNING`, cursors, foreign tables, and empty virtual relations carry declared `ColumnType` metadata without reconstructing it from runtime values; materialization remains at the final consumer boundary; spill format version 1 stores logical `(alias, column)` identities and declared schemas without a compatibility reader | Extend exact static typing to every remaining expression/operator and persistent relation kind |
| Declared types, casts, and catalogs | Integer widths, OID/XID, floating widths, character variants, UUID, temporal types, arrays, domains, foreign schemas, and migrations retain exact identities; source-sensitive OID/XID/bytea casts preserve declared width and PostgreSQL SQLSTATEs; legacy `int2vector` and `oidvector` values retain their declared type through text casts and emit PostgreSQL's space-separated text and COPY representation; `pg_type` exposes PostgreSQL 18 layouts and I/O routine OIDs for implemented built-ins, domains, `record`, `_record`, `void`, and `information_schema_catalog_name` together with its `pg_class`/`pg_attribute` identity | Complete all built-in and extension type I/O routines in `pg_proc`, composite/domain constraints, collations, enums, ranges, typmods, and binary formats |
| `ALTER COLUMN TYPE` and migration | `USING` remains structural AST and is evaluated against every old row inside the atomic ALTER transaction; source-sensitive implicit casts retain the old declared type; failed rewrites roll back schema and data; migration preserves supported scalar widths and rejects unknown source types instead of converting them to text | Complete the PostgreSQL assignment-cast matrix, dependency rewrites, domain checks, collation changes, and every `ALTER TYPE` regression case |
| Virtual and stored generated columns | Core definition, durable reopen, dependency rewrites, selective virtual evaluation, exactly-once stored evaluation, DML row images, DDL-time static typing for the implemented expression surface, exact stored SQL routine overload binding and dependencies, supported constraints and indexes, catalogs, ALTER operations, and failure atomicity are implemented and covered by the consolidated engine integration executable | Complete the PostgreSQL built-in function and operator overload matrix, privileges, inheritance and partition propagation, dump/restore, and the upstream regression cases |
| `md5(text\|bytea)` overloads | Exact text/bytea overload resolution occurs before runtime carrier erasure, text hashes its UTF-8 bytes, bytea hashes its raw payload, unsupported signatures report SQLSTATE `42883`, stored generated expressions retain the selected built-in or user binding, and `pg_proc` exposes the strict, immutable, parallel-safe, leakproof OIDs 2311 and 2321 | Continue the independently manifested PostgreSQL 18 function, operator, type, and catalog matrix |
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

The original PostgreSQL 18 additions inventory is implemented and differentially covered: `array_sort`, `array_reverse`, `reverse(bytea)`, integer/`bytea` casts, Unicode 16 full case mapping, `casefold`, `json_strip_nulls` and `jsonb_strip_nulls` array-null option, `uuidv4`, `uuidv7`, `crc32`, `crc32c`, `gamma`, `lgamma`, interval `EXTRACT(WEEK)`, negative-quarter output, `to_number(..., 'RN')`, array/composite `MIN` and `MAX`, and named regex and PL/pgSQL cursor arguments. The `array_sort(anyarray [, boolean [, boolean]])` and `array_reverse(anyarray)` discovery is independently verified with first-dimension multidimensional behavior, preserved bounds, concrete base-array return types and array-domain flattening, declaration-order-independent named arguments, unknown literal and bare-parameter Boolean context, explicit non-Boolean rejection, concrete user-overload ranking and ambiguity, polymorphic and undefined-function errors, scalar-subquery and generated-column typing, comparator failures for `json` and nested composites, and exact OIDs 6381 and 6388 through 6390. The `reverse(text)` and PostgreSQL 18 `reverse(bytea)` overload pair is independently verified with Unicode and raw-byte results, preferred `text` resolution for unknown literals, NULL, and untyped parameters, character-family implicit casts, scalar-subquery typing, exact undefined-function errors for invalid types, names, and arities, concrete user-overload ranking with explicit and implicit `pg_catalog` search order, stored generated-expression binding, and exact OIDs 3062 and 6382. The `md5(text|bytea)` slice shares that exact overload-resolution boundary while always returning `text`; it hashes raw `bytea` payloads, rejects unrelated types and invalid arities as undefined functions, persists generated-expression bindings, and exposes PostgreSQL OIDs 2311 and 2321 with their leakproof metadata. The subsequent `uuid_extract_version(uuid)` and `uuid_extract_timestamp(uuid)` discovery is implemented as its own verified parity slice with declared UUID overload resolution, PostgreSQL 18 version 1 and 7 timestamp conversion, exact return types and errors, immutable generated-column support, and `pg_proc` metadata. The `to_bin(integer|bigint)` and `to_oct(integer|bigint)` slice likewise preserves the declared 32-bit or 64-bit width through execution, scalar-subquery output binding, and generated-expression validation, emits PostgreSQL's two's-complement text for negative values, and exposes OIDs 6330 through 6333 with exact `pg_proc` metadata. The `random(integer,integer)`, `random(bigint,bigint)`, and `random(numeric,numeric)` slice uses PostgreSQL's shared xoroshiro128** stream and exact inclusive sampling algorithms, preserves overloads and arbitrary-precision numeric scale, keeps consumed draws and reseeding nontransactional across statement, transaction, and savepoint rollback, rejects invalid bounds and generated-column use with PostgreSQL SQLSTATEs, and exposes OIDs 6339 through 6341 with their strict, volatile, parallel-restricted `pg_proc` metadata; future discoveries must remain independently accounted instead of being hidden inside the broad inventory.

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

| Milestone | Exit gate |
| --- | --- |
| M0: PG18 baseline | PG18 parser pinned; all AST deltas audited; unsupported shapes fail explicitly; active names and fixtures use `pg18`; 22/22 TPC-H-derived results match PostgreSQL 18 |
| M1: Discovered semantic fixes | DML row images, constraint enforcement metadata, named NOT NULL catalogs, `reverse(bytea)`, and identified PG18 functions pass live differential tests |
| M2: Protocol 3.2 | Byte-exact codec tests and live PG18 libpq 3.0/3.2/latest negotiation and cancellation tests pass |
| M3: PG18 DDL and types | Generated columns, range/multirange, temporal constraints, catalogs, dump/restore, and reopen tests pass |
| M4: Core regression parity | PostgreSQL 18 core regression and isolation suites pass with every remaining failure recorded and trending to zero |
| M5: Client parity | The supported driver, migration, introspection, dump/restore, COPY, and pooling matrix passes |
| M6: Complete compatibility | Regression, isolation, protocol, catalog, client, concurrency, recovery, and administration matrices have zero semantic exemptions; the manual removes the implemented-subset qualification |

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

Maintain the machine-readable PostgreSQL 18 compatibility manifest at `tests/parity/pg18/manifest.json` alongside the differential harness. Each item records the PostgreSQL reference section or regression test, UQA Engine test, supported version, status, and any open issue. Every compatibility PR must update its manifest evidence, this plan's narrative, the synchronized ledger above, and the authoritative manual together. `run_diff.py --validate-manifest` rejects ledger drift and refuses a complete-compatibility claim while an item or milestone remains incomplete. Milestone completion requires positive evidence for every row; absence of a failing test is not evidence.

The final complete-compatibility audit must inspect the current parser revision, manifest, test results, live server provenance, protocol traces, client matrix, catalog diffs, persistent reopen behavior, and manual. The project must not declare complete PostgreSQL 18 compatibility while any item is missing, explicitly rejected, silently approximated, or supported only by an indirect test.
