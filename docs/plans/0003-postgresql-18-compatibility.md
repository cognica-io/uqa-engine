# PostgreSQL 18 compatibility plan

## Goal

The long-term goal is complete PostgreSQL 18 compatibility at every externally observable boundary: SQL parsing and execution, data types and casts, catalogs, errors, transactions and concurrency, frontend/backend protocol 3.0 and 3.2, and behavior seen by standard PostgreSQL clients. UQA-RS may retain its own internal storage and execution architecture, but an implementation difference is not a reason to expose different PostgreSQL behavior.

This plan distinguishes a PostgreSQL 18 baseline from complete compatibility. The baseline is reached when PostgreSQL 18 grammar, metadata, fixtures, and protocol primitives are authoritative and every unimplemented PostgreSQL 18 shape fails explicitly. Complete compatibility is reached only when the PostgreSQL 18 regression, isolation, protocol, catalog, and client matrices pass without semantic exemptions.

## Non-negotiable rules

- Parser acceptance is not execution support. Every newly accepted AST field must be implemented or rejected before any state change.
- Workarounds are prohibited. A compatibility defect must be fixed at the owning parser, type, catalog, planner, executor, storage, or protocol boundary; source rewriting, silent normalization, test-only behavior, and local dependency overrides are not shippable implementations.
- Silent approximation is a compatibility bug. This includes dropping `RETURNING` aliases, enforcing a `NOT ENFORCED` constraint, selecting a text overload for `bytea`, or omitting catalog state.
- PostgreSQL 18 is the differential oracle. Checked-in fixtures record the exact PostgreSQL server version, architecture, query text, columns, ordered rows, NULLs, value types, errors, and SQLSTATE where applicable.
- Historical PostgreSQL 17 measurements remain historical evidence. Active fixture names, paths, defaults, tests, and compatibility prose move to `pg18`; old benchmark and changelog facts are not relabeled.
- Protocol 3.0 remains supported while protocol 3.2 is added. Version negotiation and unsupported `_pq_.` options must follow PostgreSQL rather than relying on the current libpq default.
- A compatibility claim is no broader than its passing evidence. The manual must identify the current milestone until the complete-compatibility gates pass.

## Current implementation status and open PostgreSQL 18 bugs

The historical starting point used `pg_query` 6.1.1 with PostgreSQL 17 grammar, reported `server_version` as `17.0-uqa`, stored the active TPC-H-derived oracle in `expected/pg17.json`, and accepted only frontend/backend protocol 3.0 primitives. Active assets now use `pg18`, session metadata reports `18.0-uqa`, and the checked-in 22-query oracle records PostgreSQL 18.4 server and platform provenance.

The PostgreSQL 18 parser migration replaces the four DML `returning_list` fields with complete `returning_clause` handling. PostgreSQL 18's PL/pgSQL JSON producer also needs to serialize `retvarno` for datum-backed `RETURN` and `RETURN NEXT`; UQA-RS consumes those slots directly and does not rewrite routine source. The reproducible parser chain is pinned by full revision to `jaepil/pg_query.rs@7f020727f9fcdefa434b944bbe9a8f0ef029bef7`, whose submodule points to `jaepil/libpg_query@55a99be3294f0392e4123a983e3eb18e57ec938b`; the latter contains the PostgreSQL 18 parser, the corrected PL/pgSQL datum serialization, trigger-promise and type-cache fixes, and a process-wide pthread exit key with a `PTHREAD_KEYS_MAX + 1` regression test so one integration-test executable can safely create parser threads throughout its lifetime.

| Area | Current status | Remaining gate |
| --- | --- | --- |
| Active PG18 baseline | Active paths, scripts, tests, defaults, and fixtures use `pg18`; the parser chain is pinned by full remote revisions; 22/22 TPC-H-derived results match PostgreSQL 18.4 | Complete the AST coverage inventory |
| `RETURNING WITH (OLD/NEW ...)` | Old and new images and custom aliases are preserved through SQL AST, planning, and DML execution | Expand live differential coverage to triggers, partitions, and every MERGE action as those features become available |
| Constraint metadata | CHECK and foreign-key enforcement flags and named NOT NULL catalog rows are represented and tested | Complete `NOT VALID`, `ALTER CONSTRAINT`, and all dump/reopen cases |
| Identified PG18 functions and casts | The discovered array, bytea, Unicode, UUID, checksum, JSON, numeric, interval, Roman-numeral, aggregate, and regex cases are implemented and tested, including the identified `pg_proc` overload metadata | Expand the PostgreSQL 18 function, type, and catalog matrix beyond the identified additions |
| PG18 database locale catalog | `pg_database` exposes PostgreSQL 18's builtin provider, `datlocale`, `daticurules`, `datcollversion`, and `dathasloginevt` shape for the engine database, with Unicode behavior tests | Implement the complete database, collation, locale-provider, ownership, ACL, and lifecycle surface |
| PL/pgSQL datum slots and bound cursors | `retvarno`, the `-1` cursor sentinel, bound cursor arguments, named arguments, `OPEN`, `FETCH NEXT`, and `CLOSE` are structural AST and interpreter state backed by the pinned parser revisions; scalar and cursor-argument `SelectStmt` envelopes reject unsupported structure | Add session portal state before supporting refcursor parameters, returns, or cursors surviving routine exit |
| Protocol 3.2 | Byte-exact tests cover minor negotiation, ordered `_pq_.` reporting, message tag `v`, variable cancellation keys, legacy 3.0 validation, and the PostgreSQL 18 reserved-3.1 edge; PostgreSQL 18.4 `psql`/libpq live tests cover 3.0, 3.2, `latest`, 3.2-to-3.0 downgrade, authentication ordering, SSL rejection and retry, and both cancellation-key shapes | Add credentialed GSS negotiation, non-trust authentication methods, extended-query coverage, and the wider driver matrix |
| Virtual and stored generated columns | Core definition, durable reopen, dependency rewrites, selective virtual evaluation, exactly-once stored evaluation, DML row images, DDL-time static typing for the implemented expression surface, exact stored SQL routine overload binding and dependencies, supported constraints and indexes, catalogs, ALTER operations, and failure atomicity are implemented and covered by the consolidated engine integration executable | Complete the PostgreSQL built-in function and operator overload matrix, privileges, inheritance and partition propagation, dump/restore, and the upstream regression cases |
| Temporal constraints | Open compatibility bug; range and multirange carriers are absent | Implement the carrier, operator, index, and constraint layers before accepting the syntax |

## Workstreams

### 1. PostgreSQL 18 parser and AST safety

Use the reviewed PostgreSQL 18 parser chain pinned by full revision: `jaepil/pg_query.rs@7f020727f9fcdefa434b944bbe9a8f0ef029bef7` and its `jaepil/libpg_query@55a99be3294f0392e4123a983e3eb18e57ec938b` submodule. The wrapper exposes PostgreSQL's raw parser modes directly, so PL/pgSQL expressions and one-, two-, and three-part assignments are parsed structurally without rewriting input text. The C library creates its pthread destructor key exactly once per process and associates each thread's parser memory context with that shared key, preventing key exhaustion without splitting test executables or pre-initializing unrelated libraries. Any future parser update must be reviewed, tested in both repositories, pushed first, and then adopted through a new full revision and regenerated `Cargo.lock`; native, Python, Node.js, Browser WASM, and supported-platform builds remain required because the dependency contains C code and generated protobuf types.

Replace all four DML `returning_list` accesses with a compiler that consumes the complete `ReturningClause`, including its options. Audit every changed protobuf message and field between PostgreSQL 17 and 18, especially constraint enforcement, generated-column kind, temporal key flags, MERGE variants, COPY options, and utility statements. Add compiler tests proving each unsupported field fails before catalog or storage mutation.

Add an AST coverage inventory that maps every PostgreSQL 18 top-level statement and every semantically relevant option to implemented, explicitly rejected, or not-yet-audited status. The `not-yet-audited` count must be zero for the baseline milestone, and the explicitly rejected count must be zero for complete compatibility.

### 2. Active compatibility baseline and fixture rename

Rename active assets and all live references as one atomic change:

| Old | New |
| --- | --- |
| `scripts/run-tpch-pg17.py` | `scripts/run-tpch-pg18.py` |
| `benchmarks/tpch/expected/pg17.json` | `benchmarks/tpch/expected/pg18.json` |
| `tests/parity/pg17/` | `tests/parity/pg18/` |
| `crates/uqa-engine/tests/pg17_semantics.rs` | `crates/uqa-engine/tests/pg18_semantics.rs` |
| `target/benchmark-runs/tpch-pg17.json` | `target/benchmark-runs/tpch-pg18.json` |
| `uqa-tpch-pg17` and `uqa-pg17-age` defaults | PostgreSQL 18-specific defaults |

Update fixture loaders, test module names, test function names, assertion text, manifest provenance, current README and manual sections, container images, output labels, and `server_version = 18.0-uqa`. Keep version-neutral environment variable names such as `UQA_TPCH_PG_CONTAINER`. Preserve historical `HISTORY.md` entries and dated PostgreSQL 17 performance snapshots; add freshly measured PostgreSQL 18 sections rather than relabeling old numbers.

Regenerate active expected data only after the script confirms a live PostgreSQL 18 server and every UQA result matches it. Commit the exact PostgreSQL 18 patch version in the fixture and manifest.

### 3. PG18 DML row images

Represent `RETURNING` row-image aliases in the SQL AST and plan instead of flattening them into ordinary projections. For INSERT, expose a NULL old image and the inserted new image. For DELETE, expose the deleted old image and a NULL new image. For UPDATE, retain both the pre-update and post-update documents. For MERGE, select the appropriate images for INSERT, UPDATE, DELETE, and DO NOTHING actions and retain source-column and `merge_action()` behavior.

Implement default `old` and `new` qualification and `WITH (OLD AS ..., NEW AS ...)` renaming, including conflicts with table aliases and user columns. Test `*`, qualified stars, expressions combining both images, triggers when implemented, CTEs, partitions when implemented, and all four DML statements against PostgreSQL 18.

### 4. PG18 constraints and generated columns

Extend catalog definitions so constraints preserve name, type, enforcement, validation, inheritance, temporal flags, referenced columns, and expression. Runtime validation must consult those flags rather than inferring behavior from the presence of a compiled expression.

Implement named `NOT NULL` as a first-class constraint with `pg_constraint.contype = 'n'`, while keeping `pg_attribute.attnotnull` consistent. Implement `NOT ENFORCED` for CHECK and foreign keys, `NOT VALID` lifecycle, `ALTER CONSTRAINT`, and dump/reopen behavior. Add failure-atomicity tests proving rejected DDL leaves no partial metadata.

Virtual and stored generated columns now persist the generation expression and kind, reject direct writes except PostgreSQL-supported `DEFAULT` forms, evaluate virtual columns only when a projection or enforced constraint requires them without physical storage, maintain stored columns exactly once at the prepared-write boundary, reject generated-to-generated references, statically type the implemented expression surface before catalog mutation, bind and persist the exact stored SQL routine overload used for evaluation and dependency checks, rewrite relation and column dependencies, expose `attgenerated`, `pg_attrdef`, and information-schema metadata, and preserve definitions across storage reopen and schema changes. Remaining work is the complete PostgreSQL built-in function and operator overload matrix, privileges, inheritance and partition propagation, dump/restore, and the complete upstream regression matrix.

Implement range and multirange types before temporal constraints. Add comparison, containment, overlap, emptiness, canonicalization, casts, text and binary I/O, GiST-compatible or semantically equivalent enforcement support, and exclusion behavior. Then implement `WITHOUT OVERLAPS` primary/unique keys and `PERIOD` foreign keys with PostgreSQL-equivalent empty-range and referential-action rules.

### 5. PG18 functions, casts, collations, and output

Implement and differentially test the PostgreSQL 18 additions already identified: `array_sort`, `array_reverse`, `reverse(bytea)`, integer/`bytea` casts, Unicode 16 full case mapping, `casefold`, `json_strip_nulls` and `jsonb_strip_nulls` array-null option, `uuidv4`, `uuidv7`, `crc32`, `crc32c`, `gamma`, `lgamma`, interval `EXTRACT(WEEK)`, negative-quarter output, `to_number(..., 'RN')`, array/composite `MIN` and `MAX`, and named regex and PL/pgSQL cursor arguments.

For each function, add exact overload resolution, return type metadata, strictness and volatility, NULL propagation, boundary and error behavior, text and binary result encoding, and `pg_proc` visibility. Differential cases must include Unicode, empty arrays, multidimensional arrays, NaN and infinity, malformed JSON, invalid formats, and type ambiguity.

Audit PostgreSQL 18 migration changes that overlap UQA-RS, including timezone-abbreviation precedence, CSV `COPY` end markers, inheritance-aware `VACUUM` and `ANALYZE`, full-text collation behavior, and EXPLAIN output. Server-internal AIO and optimizer implementation changes do not need identical internals, but their externally visible plans, errors, and results remain differential targets.

### 6. Frontend/backend protocol 3.2

Add protocol constants and a negotiation API that accepts major version 3 startup packets, selects the highest supported minor version not greater than the request or the embedding server's explicit maximum, reports every unsupported `_pq_.` option, and encodes `NegotiateProtocolVersion` with message tag `v`. A request for an unsupported major version remains a protocol error. The embedding server's implementation maximum must be 3.0 or 3.2; PostgreSQL 18 nevertheless accepts a frontend 3.1 request unchanged and echoes selected version 3.1 when it must report an unsupported `_pq_.` option, while 3.2 enables variable-length keys. PostgreSQL 18 libpq never requests 3.1 and rejects a 3.2-to-3.1 downgrade.

Represent cancellation secrets as validated opaque bytes. Protocol 3.0 requires exactly four bytes. Under PostgreSQL 18 protocol 3.2, `BackendKeyData` emits 4 through 256 bytes and the server's `CancelRequest` parser accepts 1 through 256 bytes. Preserve zero bytes, enforce the distinct message boundaries, and provide an explicit four-byte constructor for existing integrations.

Byte-exact unit tests cover PostgreSQL 18 message formats, downgrade responses, unsupported `_pq_.` options, malformed lengths, SSL/GSS pre-startup requests, and cancellation-key boundaries. Ignored live interoperability tests use PostgreSQL 18.4 `psql` as a thin libpq driver and a server assembled directly from `uqa-pg-wire` codecs; they verify `max_protocol_version=3.0`, `3.2`, and `latest`, an explicit 3.2-to-3.0 server downgrade, authentication sequencing, SSL rejection and startup retry, legacy cancellation, and a 256-byte protocol 3.2 cancellation key.

Complete the remaining protocol evidence with a credentialed Kerberos environment that actually emits `GSSEncRequest`, non-trust authentication exchanges, extended-query and binary formats, layered middleware keys near the 256-byte limit, malformed live peers, and the client matrix in workstream 8.

### 7. Complete SQL, catalog, and transaction compatibility

Drive remaining work from the PostgreSQL 18 official regression schedules rather than an ad hoc feature list. Import queries and expected behavior in license-compatible differential harnesses, categorize failures by parser, binder, type system, planner, executor, catalog, transaction, protocol, or administration, and maintain a burn-down manifest with owners and evidence.

Close the existing embedded-runtime gaps: PostgreSQL integer widths and overflow, numeric precision and formatting, collations, domains, enums, composite and range types, temporary and unlogged relations, views and materialized views, inheritance and partitioning, row locks, triggers and rules, sequences, COPY, complete system catalogs, roles and ACLs, MVCC snapshots, isolation and deadlock behavior, prepared statements and portals, large objects, extensions, replication-facing protocol, WAL and administration surfaces.

Use PostgreSQL 18 SQLSTATE, primary error text where clients depend on it, transaction-abort behavior, command tags, row descriptions, OIDs, typmods, binary formats, and catalog visibility as part of the contract. Performance optimizations may differ but must not alter these observations.

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

Run focused compiler and engine tests while iterating, followed by:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo test -p uqa-engine --test integration engine_queries::manual_sql_examples::manual_sql_examples_compile_or_execute
cargo test -p uqa-engine --test integration sql_tpch::
UQA_PG18_DOCKER_HOST=container-reachable-host cargo test -p uqa-pg-wire --test protocol libpq_interop:: -- --ignored
python3 tests/parity/pg18/run_diff.py --validate-manifest
python3 tests/parity/pg18/run_diff.py
python3 scripts/run-tpch-pg18.py --iterations 3
```

The live wire command requires `UQA_PG18_DOCKER_HOST` to name the test server host as reachable from the client container; this is explicit because the correct address differs between Docker Desktop, native Linux Docker, and remote container runtimes. It uses a PostgreSQL 18 client container named `pg-parity` by default, and `UQA_PG18_WIRE_CONTAINER` selects another container.

Run repository policy scripts, binding builds and examples, and supported-platform CI whenever the parser dependency, public AST, catalog serialization, value representation, or wire types change.

## Completion accounting

Maintain the machine-readable PostgreSQL 18 compatibility manifest at `tests/parity/pg18/manifest.json` alongside the differential harness. Each item records the PostgreSQL reference section or regression test, UQA-RS test, supported version, status, and any open issue. `run_diff.py` validates the manifest independently of the live oracle and refuses a complete-compatibility claim while an item or milestone remains incomplete. Milestone completion requires positive evidence for every row; absence of a failing test is not evidence.

The final complete-compatibility audit must inspect the current parser revision, manifest, test results, live server provenance, protocol traces, client matrix, catalog diffs, persistent reopen behavior, and manual. The project must not declare complete PostgreSQL 18 compatibility while any item is missing, explicitly rejected, silently approximated, or supported only by an indirect test.
