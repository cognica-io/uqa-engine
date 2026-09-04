# History

All notable changes to `uqa-engine` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Extended PostgreSQL 18 routine dependency ownership to column defaults and column- or table-level CHECK constraints, including exact overload binding at every schema publication boundary, routine rename and old-name recreation isolation, RESTRICT, expression-granular CASCADE that retains the table and column, atomic dependency replacement, transactional initial-open migration, validation-only reloads, durable reopen, and checked-in PostgreSQL 18.4 oracle evidence.
- Extended PostgreSQL 18 routine dependency ownership to SQL-standard `INSERT`, `UPDATE`, `DELETE`, and `MERGE` bodies and parameter defaults across SQL-standard, SQL string, and PL/pgSQL body forms, including creation-path binding, exact rename and old-name recreation isolation, atomic dependency replacement, transitive RESTRICT and CASCADE, transactional initial-open migration, validation-only reloads, two-stage routine and stored-view restoration, durable reopen, and a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 `ALTER FUNCTION`, `ALTER PROCEDURE`, and `ALTER ROUTINE ... RENAME TO` with persistent routine object identities, stable `pg_proc` OIDs and information-schema specific names, exact overload and kind selection, transaction rollback, bound-dependent rewrites for SQL-standard bodies, scalar and table-function views, generated columns, rules, and triggers, dynamic SQL string and PL/pgSQL body behavior, old-name recreation isolation, transactional initial catalog migration, validation-only reloads, durable reopen coverage, and a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 `LISTEN`, `UNLISTEN`, and `NOTIFY` with transactional subscriptions, commit-delayed delivery, rollback and savepoint behavior, transaction-wide duplicate collapse, read-only and `DISCARD ALL` semantics, same-process session and durable-database coordination, payload limits, a public notification drain API, and durable unconditional rewrite-rule `NOTIFY` actions with zero-row and multi-row statement cardinality, exact conditional-rule errors, and a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 scalar `OLD` and `NEW` rewrite-rule whole-row composites and creation-bound action-target `RETURNING` stars, including live event-row shape, catalog-backed local-name shadowing, command-specific event and action image namespaces, set-oriented execution, system-attribute visibility, rename and drop dependencies, durable reopen, and a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 rewrite-rule action `RETURNING` event-row namespaces and creation-time `OLD.*`/`NEW.*` expansion, including action-image alias precedence, exact namespace errors, set-oriented cardinality, rename and drop dependencies, durable reopen, added-column row-width failure atomicity, and a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 creation-time rewrite-rule action-row expansion for `OLD.*` and `NEW.*` in multi-row `VALUES` actions, `SELECT` target lists, and `ROW` constructors, including declaration order, event-side validation, nested alias shadowing, query-scope restrictions, added-column stability, rename and drop dependencies, durable reopen, and a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 rewrite-rule condition subqueries with constant, correlated, scalar, `IN`, external-relation, local-shadowing, and correlated-CTE forms across INSERT, UPDATE, and DELETE OLD/NEW rows; creation-time bound relations and routines; INSERT action-time state; rule-owner relation and invoker routine privileges; scalar-cardinality atomicity; exact event-column projection, rename, dependency, reopen, and valid `pg_get_ruledef` SQL; and a checked-in PostgreSQL 18.4 oracle.
- Verified PostgreSQL 18 rewrite-rule privilege subjects for user-defined function calls in conditions and actions, `nextval`, `currval`, `lastval`, `setval`, action-target sequence defaults, and direct sequence relation scans, including owner transfer and failure atomicity, with a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 foreign-table trigger definitions with ordinary row and statement forms, exact unsupported constraint, transition, and INSTEAD OF boundaries, target and function privileges, owner-derived ALTER and DROP authority, `ALTER FOREIGN TABLE` enable modes, `pg_trigger`, `relhastriggers`, function and relation dependency cleanup, and transaction, rollback, cross-engine refresh, and durable-reopen coverage, verified by a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 table and view trigger authorization with target `TRIGGER` checks before function lookup, trigger-function `EXECUTE` checks before return-type and duplicate-name validation, inherited-owner lifecycle authority, relation-owner-derived DROP and ALTER checks, missing-trigger `IF EXISTS` behavior, creation-time-only function authorization, and transaction, ownership-transfer, cross-engine, and durable-reopen coverage, verified by a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 standalone-index ownership by deriving every index owner from its table, requiring inherited table ownership before schema `CREATE` and definition checks, permitting the table owner or containing-schema owner to `DROP INDEX`, preflighting every multi-target drop, and preserving owner changes through transaction rollback, cross-engine refresh, and durable reopen, verified by a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 foreign-table relation and column ACLs with NULL defaults, all eight table-shaped privileges, `PUBLIC`, independent rooted grant-option paths, dependent `RESTRICT` and `CASCADE`, implicit owner rights, role dependencies, owner-transfer rewriting, `ALL TABLES IN SCHEMA`, name/OID privilege inquiry, exact direct, joined, definer-view, and security-invoker SELECT enforcement, `pg_class.relacl`, `pg_attribute.attacl`, information-schema visibility and read-only metadata, transaction, savepoint, cross-engine refresh, explicit SQLite and key-value migration, corruption rejection, and durable reopen behavior, verified by a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 foreign-table role ownership with creating-role defaults, inherited owner authority, owner-or-containing-schema-owner DROP, direct and historical `OWNER TO` spellings, target-role SET and schema-CREATE checks with superuser bypass, target foreign-server privilege independence, dependent-role protection, dependent-view `RESTRICT` and recursive `CASCADE`, `pg_class.relowner`, read-only rejection, transaction and savepoint rollback, cross-engine refresh, explicit SQLite and key-value migration, invalid-owner rejection, and durable reopen, verified by a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 regular-view and materialized-view relation and column ACLs with NULL defaults, all table-shaped privileges, `PUBLIC`, membership-aware rooted grant-option paths, dependent revoke, implicit owner rights, role dependencies, owner-transfer rewriting, replacement preservation, `ALL TABLES IN SCHEMA`, exact query and DML enforcement through nested automatic and trigger paths, regular-view definer and `security_invoker` privilege subjects, delegated materialized-view `MAINTAIN` with owner-context refresh, all name/OID `has_table_privilege` and `has_column_privilege` forms, `pg_class.relacl`, `pg_attribute.attacl`, information-schema visibility, transaction, savepoint, temporary, cross-engine, SQLite and key-value migration, corruption rejection, and durable reopen behavior, verified by a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 regular-view and materialized-view role ownership with creating-role defaults, inherited owner authority for ALTER, replacement, refresh, and direct DROP, containing-schema owner DROP authority, SET-authorized `OWNER TO` with target-schema `CREATE` enforcement and superuser bypass, dependent-role protection, exact `pg_class`, `pg_views`, and `pg_matviews` projection, transaction and savepoint rollback, temporary lifecycle, cross-engine refresh, durable catalog migration and reopen, and stable relation identity, verified by a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 ordinary-table column `SELECT`, `INSERT`, `UPDATE`, and `REFERENCES` ACLs with durable NULL and empty `attacl` state, independent grant-option paths and dependent revoke, exact query and DML column lineage including correlated and joined sources and system columns, `COPY`, foreign keys, `MERGE`, column rename and drop, owner transfer and role dependencies, all twelve `has_column_privilege` overloads including sequence and attnum behavior, `information_schema.columns`, `column_privileges`, and `role_column_grants`, transaction and savepoint rollback, cross-engine refresh, SQLite and key-value migration, and durable reopen, verified by a checked-in PostgreSQL 18.4 oracle.
- Implemented PostgreSQL 18 sequence `USAGE`, `SELECT`, and `UPDATE` ACLs with historical `ON TABLE` and `ALL SEQUENCES IN SCHEMA` targets, `PUBLIC`, grant options, membership-aware independent grantor paths, dependency-aware `RESTRICT` and `CASCADE`, owner self-revocation and transfer rewriting, value-function enforcement, all six name/OID `has_sequence_privilege` overloads, `pg_class.relacl`, role dependencies, read-only and resolution precedence, transaction and savepoint rollback, temporary lifecycle, durable catalog migration, cross-engine refresh, and reopen behavior, verified against PostgreSQL 18.4.
- Implemented PostgreSQL 18 sequence role ownership with creating-role defaults, membership-aware ALTER and DROP checks, SET-authorized direct and historical `OWNER TO`, dependent-role protection, serial and identity transfer rejection, `pg_class.relowner` and `pg_sequences.sequenceowner`, stable relation identity and definition state, transaction and savepoint rollback, temporary lifecycle, durable catalog migration, and reopen behavior, verified against PostgreSQL 18.4.
- Implemented PostgreSQL 18 `ALTER SEQUENCE ... RENAME TO` and `ALTER SEQUENCE ... SET SCHEMA`, including historical `ALTER TABLE` spellings, stable `regclass` and `pg_class` OIDs, preserved current- and sibling-session cache blocks, `currval`, and `lastval`, rewritten column-default and stored-view dependencies, serial and identity ownership metadata, exact temporary, collision, relation-kind, missing-schema, owned-sequence, and read-only errors, transaction and savepoint rollback, durable reopen, and numeric `regclass` calls, verified by the 275-case transaction-state oracle and a two-session PostgreSQL 18.4 transcript.
- Implemented PostgreSQL 18 `ALTER SEQUENCE ... SET LOGGED|UNLOGGED`, including the historical `ALTER TABLE` spelling for sequence targets, with exact no-op and changed cache behavior, serial and identity ownership, temporary and relation-kind errors, read-only precedence, transaction and savepoint rollback, durable reopen, cross-session invalidation, and `pg_class.relpersistence`, verified by the 252-case transaction-state oracle and a two-session PostgreSQL 18.4 transcript.
- Implemented hard text-to-`regnamespace` casts with catalog OID identity, direct comparison to OID catalog columns, catalog-aware output, and PostgreSQL 18 missing-schema, malformed-name, malformed-OID, and overflow SQLSTATEs.
- Implemented PostgreSQL 18 sequence `OWNED BY table.column` and `OWNED BY NONE` with stable table and column identities, serial automatic and identity internal dependencies, owner rename and drop lifecycle, transitive `RESTRICT` and `CASCADE`, `TRUNCATE` restart behavior, transaction and savepoint rollback, durable catalog migration, and `pg_get_serial_sequence`, verified by the 233-case transaction-state oracle.
- Implemented PostgreSQL 18 sequence `CACHE` for `CREATE SEQUENCE` and `ALTER SEQUENCE` with bounded durable reservations, session-local consumption and abandonment, definition-change and caller-`setval` invalidation, read-only and rollback behavior, persistent catalog migration, and `pg_sequences` metadata, verified by the 196-case transaction-state oracle.
- Implemented PostgreSQL 18 sequence `AS smallint|integer|bigint`, direction-sensitive default and explicit minimum and maximum bounds, `CYCLE` and `NO CYCLE`, bound exhaustion, matching `ALTER SEQUENCE`, durable catalog migration, and definition-owned transaction and savepoint rollback while preserving session `currval` and `lastval`, verified by the 188-case transaction-state oracle.
- Implemented PostgreSQL 18 SQL `DROP SEQUENCE` with multi-target atomic validation, `IF EXISTS`, exact relation-kind and dependency errors, `RESTRICT` and `CASCADE` for column defaults and recursive view dependencies, serial ownership direction, transactional and savepoint rollback, read-only rejection, durable reopen, and session-state identity, verified by the 166-case transaction-state oracle.
- Implemented PostgreSQL 18 `lastval()` with exact session selection, `setval` interaction, rollback and exception persistence, `DISCARD SEQUENCES`, durable sequence-lifecycle identity, `pg_proc` metadata, and simple-query transaction behavior, verified by the 149-case transaction-state oracle.
- Implemented PostgreSQL 18 three-argument `setval(regclass, bigint, boolean)` with exact called-state, `currval`, strict NULL, default-bound, relation-kind, read-only transaction, failed-statement, PL/pgSQL exception, rollback, savepoint, persistent reopen, `pg_sequences`, and function-signature behavior, verified by the 139-case transaction-state oracle.
- Implemented PostgreSQL 18 PL/pgSQL `COMMIT` and `ROLLBACK` with `AND CHAIN` and `AND NO CHAIN` for standalone procedures and anonymous blocks, including direct nested invocation, fresh transaction segments, retained local variables and chained characteristics, exact atomic-context errors, query-loop holdability, command-loop rejection, cursor cleanup, durable reopen behavior, and a 114-case stateful oracle.
- Verified PostgreSQL 18 `RETURNING` current, `OLD`, and `NEW` row images across ordinary and partitioned `INSERT`, `UPDATE`, `DELETE`, `ON CONFLICT`, and every `MERGE` mutation action, including BEFORE-trigger mutation and suppression, immutable original old images, non-retroactive AFTER-trigger writes, generated values, cross-leaf movement, and physical `tableoid` identities in the live stateful oracle.
- Implemented PostgreSQL 18 `to_regproc(text)`, `to_regprocedure(text)`, `to_regclass(text)`, `to_regnamespace(text)`, `to_regrole(text)`, and `to_regtype(text)` with native identifier-string and type-name parsing, numeric and zero OID forms, qualification and search-path semantics, routine overload handling, durable scalar and array `regrole` OID storage, PostgreSQL's stored scalar-constant dependency restriction and input-error precedence, soft and hard input behavior, exact SQLSTATEs and return aliases, and matching `pg_type`, `pg_proc`, and `information_schema.routines` metadata.
- Implemented PostgreSQL 18 `MERGE` through automatically updatable nested single-source projection views and direct or nested `INSTEAD OF` view-trigger paths, including target predicates, writable-column mapping, `INSERT`, `UPDATE`, and `DELETE`, source, target, `OLD`, and `NEW` `RETURNING`, action-path selection, statement-trigger ordering, trigger suppression, repeated candidates, post-trigger check options, base defaults and triggers, exact relation-kind and updatability errors, and a 609-case stateful PostgreSQL 18.4 oracle.
- Implemented durable PostgreSQL 18 role memberships with per-grantor `ADMIN`, `INHERIT`, and `SET` options, dependency-aware grant and revoke, transitive role assumption and privilege inheritance, `pg_auth_members`, CREATEROLE delegation limits, membership-aware routine ownership, non-owner routine ACL delegation with independent grantor paths and dependency-aware revoke, SECURITY INVOKER and DEFINER role context, all six name/OID `pg_has_role` overloads, `regrole` OID storage and stored-constant dependency behavior, and a 182-case stateful PostgreSQL 18.4 oracle.
- Implemented scalar PostgreSQL 18 `regprocedure` exact-signature input and search-path-aware output for user functions, procedures, and built-ins, including the OID carrier, `pg_type` identities, four scalar I/O `pg_proc` rows, and exact missing-overload errors.

### Fixed

- Separated tuple `xmin` from the user document namespace with a required typed storage-record contract, eager SQLite and key-value migration of legacy sentinel fields, schema-aware preservation of user `xmin` collisions, and metadata-safe scans, command overlays, portal snapshots, row-lock rechecks, OLD/NEW `RETURNING`, schema rewrites, and `VACUUM FULL`.
- Accepted PostgreSQL 18 `INSERT DEFAULT VALUES` as one input row so direct inserts and rewrite-rule actions apply defaults, serial or identity generation, event cardinality, and `RETURNING` instead of rejecting the parser's source-less AST.
- Made `DROP TABLE ... CASCADE` remove the complete transitive dependent-view closure without requiring ownership of those dependent views, while preserving owner-error precedence for direct view drops.
- Corrected implicit `smallserial`/`serial2`, `serial`/`serial4`, and `bigserial`/`serial8` backing sequences to use PostgreSQL's `smallint`, `integer`, and `bigint` types, and made `information_schema.sequences` expose exact type, precision, bounds, start, increment, and cycle metadata while excluding internal identity sequences, filtering rows by owner inheritance or sequence privileges, and hiding other sessions' temporary sequences from both sequence catalog views across durable reopen.
- Promoted legacy column-only `PRIMARY KEY` and `UNIQUE` flags into named durable table-key constraints during initial catalog restore, preventing a later catalog query, transaction commit, or unrelated `DROP TABLE` from failing because an upgraded persistent table exposed an anonymous constraint.
- Corrected stateful PostgreSQL 18 oracle schema substitution inside string literals so catalog assertions inspect the intended schema instead of a quoted placeholder.
- Rejected unauthorized routine replacement and explicit routine drops before mutation, required a SET-enabled path for ownership transfer, and prevented `SECURITY DEFINER` calls from changing the effective session role.
- Kept null-accepting predicates on the null-extended side of an outer join above the join instead of pushing them into a source scan and manufacturing false unmatched rows.

## [0.1.12] - 2026-08-30

### Added

- Implemented PostgreSQL 18 partition-moving `UPDATE` trigger behavior, including source `DELETE` and destination `INSERT` row lifecycles, destination suppression and mutation, root update transition rows, `UPDATE FROM`, and the distinct empty transition sets used by partition-moving `MERGE` actions.
- Implemented the superuser-only PostgreSQL 18 `session_replication_role` setting for `origin`, `local`, and `replica`, including trigger and rewrite-rule enable modes and replica-mode suppression of foreign-key checks and referential actions.
- Implemented durable PostgreSQL 18 `INSTEAD OF` row triggers on views for `INSERT`, `UPDATE`, and `DELETE`, including row chaining, suppression, statement timing, statement-start snapshots, `RETURNING`, catalog lifecycle, and durable reopen.

### Fixed

- Reduced encrypted request-session startup latency by sharing the storage pool's stable data-version monitor and by avoiding open-only catalog migrations after the persistent engine has already initialized them, while retaining catalog refresh after concurrent storage changes.
- Made npm publication tolerate packages that the registry has accepted but is still processing, and extended verification to wait for installability within a bounded deadline.

## [0.1.11] - 2026-08-30

### Added

- Implemented PostgreSQL 18 `REFERENCING OLD TABLE` and `REFERENCING NEW TABLE` transition relations for typed `AFTER` row and statement triggers across multi-row and zero-row `INSERT`, `UPDATE`, and `DELETE`, `INSERT SELECT`, `ON CONFLICT`, `UPDATE FROM`, `MERGE`, partition and inheritance descendants, direct and recursive foreign-key actions, catalog deparsing, nested-routine isolation, persistence guards, and a 136-case PostgreSQL 18.4 stateful oracle.

### Fixed

- Matched PostgreSQL 18 trigger-queue transition-set boundaries for recursive chain and branching cascades, including coalesced sets, split waves, and trailing empty statement-trigger invocations.
- Preserved statement-global AFTER ROW event ordering and cascade-parent relationships when multi-row `ON CONFLICT DO UPDATE` combines independently prepared cascade trees.

## [0.1.10] - 2026-08-30

### Added

- Implemented PostgreSQL 18 constraint triggers with `AFTER ROW` execution, optional `FROM` dependencies, immediate and deferred modes, captured row images, retroactive `SET CONSTRAINTS` firing, savepoint and outer-commit lifecycle, queued-event cancellation, independent trigger and constraint renames and OIDs, trigger-owned `pg_constraint` rows, partition clone identities, catalog deparsing, and a 63-case PostgreSQL 18.4 stateful oracle.

### Fixed

- Prevented durable rewrite-rule catalog restoration from recursively entering transaction snapshots or registry refresh while statically validating stored view actions.
- Assigned deterministic object identities to legacy stored trigger metadata so trigger catalog OIDs remain stable across rename and reopen after an upgrade.

## [0.1.9] - 2026-08-30

### Fixed

- Matched PostgreSQL 18 transaction and maintenance behavior for fixed repeatable-read and serializable snapshots across writer promotion, declaration-time and incremental SQL cursor execution, holdable-cursor commit rewind and deferred-constraint revalidation, read-only `ANALYZE` and `VACUUM`, targeted `VACUUM FULL`, and schemaless system `xmin` refreshes.

## [0.1.8] - 2026-08-29

### Added

- Implemented PostgreSQL 18 `SET CONSTRAINTS` for deferrable foreign keys, including persistent transaction-wide `ALL` state, schema and exact search-path name resolution, duplicate-name fanout, durable constraint-object identities, exact constraint-bound row events, retroactive `IMMEDIATE` validation, selective multi-constraint checks, savepoints, SQL routines, dynamic PL/pgSQL, reentrant host callbacks, PostgreSQL simple-query transaction-control semantics, cross-session constraint replacement and table-rename lifecycle, allocated `pg_temp` lifetime, PostgreSQL SQLSTATEs including pending trigger-event changes, and top-level outside-transaction warning followed by normal name and deferrability resolution.

### Fixed

- Matched PostgreSQL 18 deferred-trigger lifecycle behavior by creating child-side events only for inserts and foreign-key-value changes, recording parent-side key-change events even when no child currently matches, following queued events across row-identity rewrites, identifying the exact physical partition that fired each event, retaining already queued checks across later deferrability changes, firing former-deferrable events for `SET CONSTRAINTS ALL IMMEDIATE`, resetting a named mode when disabled foreign-key triggers are recreated while retaining an `ALL` mode, preserving full view, schema-expression, and foreign-key `2BP01` RESTRICT dependency precedence before pending-event checks while blocking event-sensitive ALTER, DROP, and TRUNCATE operations including referenced-parent trigger removal with `55006`, canonicalizing legacy hierarchy parents before synchronizing partition-inherited foreign-key identities during reopen, propagating partition-root foreign-key drops while rejecting direct inherited-clone drops, preventing deleted clones from rebinding pending checks to a still-live parent relation, continuing simple-query atomic segments after a pre-existing transaction closes, and validating deferred foreign keys exactly once before temporary-table `ON COMMIT` actions can remove their events.
- Made live crates.io release dispatches fail when registry credentials are absent, retry crates.io rate limits, and record the live or dry-run outcome in release notes instead of silently falling back to dry-run.

## [0.1.7] - 2026-08-28

### Added

- Implemented PostgreSQL 18 recursive CTE `SEARCH` and `CYCLE`, including depth- and breadth-first sequence values, cycle marks and paths, generated-column scope, path-aware `UNION` distinctness, iteration-wide recursive-term semantics, validation ordering, and durable stored plans; implemented `MATERIALIZED` and `NOT MATERIALIZED` folding policy and completed the related CTE/catalog row-lock validation matrix.
- Added context-aware PostgreSQL authentication sequencing, exact extended-query binary format resolution, bounded layered cancellation keys, malformed-peer coverage, and a pinned psycopg, pgx, and node-postgres PostgreSQL 18.4 client matrix covering prepared reuse, COPY, transaction recovery, and pooling.
- Implemented the PostgreSQL 18 named CHECK, foreign-key, and `NOT NULL` constraint lifecycle, including `NOT VALID`, atomic validation and enforcement transitions, initially-deferred foreign-key commit checks, savepoint rollback, dependency-aware drops, catalog persistence, OID-backed `regclass` inspection, and comma-separated `ALTER TABLE` atomicity.
- Implemented PostgreSQL 18 `FETCH FIRST ... WITH TIES` across query blocks, set operations, CTEs, aggregation, windows, distinct processing, row locking, and ranked retrieval, including complete multi-key and NULL peer boundaries.
- Implemented PostgreSQL 18 `ESCAPE` semantics for `LIKE`, `ILIKE`, and `SIMILAR TO`, including the default backslash, disabled and NULL escapes, runtime escape expressions, Unicode escape characters, and matching SQLSTATEs.
- Implemented PostgreSQL 18 `GROUP BY DISTINCT`, including duplicate elimination after `GROUPING SETS`, `ROLLUP`, and `CUBE` expansion using analyzed column, cast, type, and operator identity while preserving structurally distinct expressions and `GROUP BY ALL` multiplicity.
- Implemented PostgreSQL 18 table-function `WITH ORDINALITY`, including one-based `bigint` counters, positional and partial column aliases, multi-column functions, and per-invocation reset for LATERAL execution.
- Implemented PostgreSQL 18 named `WINDOW` clauses, including reusable definitions, left-to-right partition and ordering inheritance, legal ordering and frame extension, direct framed references, and matching definition errors.
- Implemented PostgreSQL 18 aliases on parenthesized JOIN expressions, including final-output column aliases, input-name hiding, outer JOIN and LATERAL visibility, ambiguity and alias-count errors, optimization boundaries, and row-lock targeting.
- Implemented PostgreSQL 18 column-name lists for `CREATE TABLE AS`, including positional and partial renaming, quoted identifier preservation, exact declared output types, duplicate and system-column validation, and durable reopen behavior.
- Implemented PostgreSQL 18 `CREATE TABLE AS ... WITH NO DATA`, including static query analysis without execution, exact output schemas, vector and tensor field metadata, `IF NOT EXISTS` validation order, and durable reopen behavior.
- Implemented PostgreSQL 18 `SELECT ... INTO` for ordinary durable tables with CTAS-equivalent type identity, validation order, transactionality, and persistence.
- Implemented PostgreSQL 18 `CREATE VIEW` column-name lists with positional and partial aliases, quoted identifier preservation, static creation-time analysis, durable fixed output names, duplicate and width errors, and `CREATE OR REPLACE VIEW` row-type compatibility checks.
- Implemented PostgreSQL 18 temporary and unlogged table, view, sequence, CTAS, and `SELECT INTO` persistence; temporary `pg_temp` lookup, backend-isolated storage, dependency-safe `ON COMMIT` and `DISCARD TEMP` lifecycle; materialized-view snapshots and refresh; view reloptions; durable catalog metadata; and relation-kind SQLSTATEs.
- Implemented PostgreSQL 18 polymorphic and `VARIADIC` routine resolution for represented scalar and array types across SQL, PL/pgSQL, `CALL`, `TABLE`, `SETOF`, generated columns, stored views, user `pg_proc` metadata, volatility/null-input ALTER lifecycle, and bounded routine CASCADE dependencies.
- Extended PostgreSQL 18 routine dependency handling to exact SQL-standard query-body bindings, dependent functions and procedures, transitive and multi-target CASCADE graphs, durable reopen, dynamic string-body behavior, and cascade notices.
- Implemented PostgreSQL 18 routine ownership, roles, EXECUTE ACLs, security-definer execution, routine-local configuration, planner-support metadata, and session portals for bound `refcursor` values across routine calls, savepoints, and transaction boundaries.
- Implemented PostgreSQL 18 built-in ranges and multiranges, canonical text and operator behavior, polymorphic range routines, `WITHOUT OVERLAPS` keys, aggregate `PERIOD` foreign-key coverage, atomic type rewrites, catalog identity, and durable reopen behavior.
- Implemented durable PostgreSQL 18 `BEFORE` and `AFTER` row and statement triggers for `INSERT`, `UPDATE`, `DELETE`, and `TRUNCATE`, including generated-row images, referential actions, `ON CONFLICT`, `MERGE`, partition clones, lifecycle operations, dependencies, and catalog deparsing.
- Implemented durable PostgreSQL 18 table rewrite rules for `INSERT`, `UPDATE`, and `DELETE`, including OLD/NEW row sets, ordered `ALSO` and `INSTEAD` actions, DML `RETURNING` providers, lifecycle operations, recursion and scope checks, and `pg_rewrite`/`pg_rules` catalogs.
- Implemented PostgreSQL 18 MERGE full-join candidate semantics, including `WHEN NOT MATCHED BY SOURCE` UPDATE, DELETE, and `DO NOTHING`, written-order action selection, candidate-specific name visibility, repeated-target cardinality errors, and complete INSERT, UPDATE, DELETE, and `DO NOTHING` RETURNING behavior with source columns, old/new row images, `merge_action()`, and source-before-target star expansion.
- Made the PyPI `uqa` package install the `usql` console command backed by the same Rust CLI implementation as the standalone `uqa-cli` binary.
- Added npm trusted publishing for the `@cognica-io/uqa` Node.js and `@cognica-io/uqa-wasm` browser packages, with six platform-constrained native addons published under the `@cognica-io` organization and selected through exact-version optional dependencies.

### Fixed

- Matched Apache AGE on PostgreSQL 18 for dependency-based `drop_label`: removable default and user label relations now disappear durably, vertex-label drops preserve incident edge rows and dangling endpoint ids, same-kind inherited labels and stored views retain `DROP ... RESTRICT`, graph rename and cascading drop preserve their view semantics, direct `DROP TABLE` remains protected, label relations are selectable, and missing-default graph accesses fail safely instead of crashing.
- Prevented recursive catalog synchronization from deadlocking persistent-engine reopen while vector indexes are rebound, including unlogged vector tables.
- Made the Python source distribution include every manifest-declared benchmark source so pip can build a wheel from the sdist instead of failing during Cargo metadata validation.
- Made `usql` preserve SQL-standard `BEGIN ATOMIC ... END` routine bodies as one statement by using the pinned PostgreSQL 18 scanner for lexical boundaries.
- Made scalar and array `regproc`, `regclass`, `regnamespace`, and `regtype` values use PostgreSQL 18 catalog-aware text output in explicit casts, `usql`, and `COPY TO`, including `0` as `-`, unresolved OIDs as decimal text, visible-name qualification, built-in type aliases, NULL preservation, exact virtual-catalog `regclass` OIDs, and exact `nextval`/`currval`/`setval` `pg_proc` identities.
- Matched PostgreSQL 18 SQLSTATE `42701` for duplicate columns in recursive CTE `SEARCH` and `CYCLE` lists.

## [0.1.6] - 2026-08-21

### Added

- Implemented PostgreSQL 18 `SELECT` row-locking clauses (`FOR UPDATE`, `FOR NO KEY UPDATE`, `FOR SHARE`, `FOR KEY SHARE`, `OF`, `NOWAIT`, and `SKIP LOCKED`), including join and view targets, wait and skip policies, savepoint release, and matching `UPDATE`/`DELETE` row locks.
- Added the `uqa` facade package, which re-exports the embedded `uqa-engine` API and core `Value` type as the primary Rust dependency.

### Changed

- Prepared every public Rust workspace package for crates.io, imported the PostgreSQL 18 parser pin as `uqa-pg-query`, and kept the Python, Node.js, and Browser WASM binding crates off crates.io.
- Renamed the product and canonical GitHub repository to UQA Engine and `uqa-engine`, updated documentation, legal notices, release metadata, examples, generated crate files, the research PDF, and the repository-local agent skill, kept `uqa-engine` as the engine package, and added `uqa` as the user-facing facade package.

## [0.1.5] - 2026-08-17

### Added

- Added one-shot local and Cloud project-name initialization to the Rust, Python, and Node.js `HttpEngine` bindings through the installed `uqa` CLI, including optional Cloud organization selection and explicit nonstandard CLI paths while preserving URL/token and environment constructors.

### Security

- Launched CLI discovery without a shell or token-bearing arguments, removed any ambient `UQA_TOKEN` from the child environment, bounded stdout and stderr at 64 KiB and execution at 30 seconds, cleared captured credential buffers, and kept CLI diagnostics out of public errors.

## [0.1.4] - 2026-08-16

### Added

- Added the `uqa-client` crate, a common asynchronous SQL Engine contract, and direct authenticated `HttpEngine` bindings for Rust, Python, Node.js, and browsers, including materialized SQL, atomic batches, request metadata, and bounded NDJSON streaming against the shared local and Cloud UQA data-plane API.
- Added the Apache AGE catalog surface: `ag_catalog.ag_graph` and `ag_catalog.ag_label` (bare names resolve through `search_path`), the `agtype`, `graphid`, `label_id`, and `label_kind` types in `pg_type`, one `pg_namespace` / `information_schema.schemata` entry per graph plus `ag_catalog`, and label relations and label sequences mirrored into `pg_class`, `pg_attribute`, `pg_sequences`, `information_schema.tables`, and `information_schema.columns`.
- Added `LOAD` as a session statement that loads Apache AGE as a no-op through every `$libdir` spelling and fails for other libraries with PostgreSQL's missing-file error.
- Added AGE graph and label management: `graph_exists`, `create_vlabel`, `create_elabel`, `drop_label`, and `alter_graph`, with AGE's Unicode identifier validation, messages, and SQLSTATEs, and recorded the vertex or edge kind of every graph label so `ag_label.kind` and AGE's label-kind conflicts are exact.
- Added the PostgreSQL 18 `regnamespace` type across parsing, casts, catalogs, foreign tables, and the CLI.

### Changed

- Made the Browser WASM build select a Python 3.10-or-newer interpreter explicitly when an older system Python appears first on `PATH`.
- Made Node.js HTTP requests run on the asynchronous Tokio runtime instead of occupying the four-thread libuv worker pool, so independent local and Cloud queries can make network progress concurrently.
- Made `create_graph` and `drop_graph` raise Apache AGE's messages and SQLSTATEs (`22023`, `3F000`, `42P06`, `2BP01`) instead of generic unsupported-feature errors, validate graph names with AGE's rules (3 to 63 bytes with Unicode identifier characters, dots, and dashes), and reserve the graph namespace so `create_graph`, `CREATE SCHEMA`, and `alter_graph ... RENAME` reject name collisions and `DROP SCHEMA ... CASCADE` drops a graph namespace.

### Fixed

- Made HTTP bindings reject nested non-finite parameters, preserve bytes and adversarial JSON keys, format extended dates and mixed-sign intervals exactly, accept IPv6 loopback nodes, validate the complete NDJSON body after a terminal frame, and consume large browser frames without quadratic copying.
- Kept native Node.js builds from overwriting the committed version-checking loader and declared the Python HTTP surface in the shipped type stub.

## [0.1.3] - 2026-08-16

### Added

- Added the PostgreSQL 18 baseline with a revision-pinned PostgreSQL 18 parser chain, `pg18` fixtures and differential probes, `18.0-uqa` session metadata, protocol 3.2 primitives, and exact 22-query PostgreSQL 18.4 TPC-H-derived results.
- Added PostgreSQL 18 behavior for qualified joins, DML old/new `RETURNING` row images, constraint metadata, identified functions and casts, database locale catalogs, generated columns, and the implemented PL/pgSQL datum-slot and bound-cursor surface.

### Changed

- Replaced flattened relational column names with structured `(qualifier, column)` identities across planning and execution, kept lateral and correlated rows physical until their final consumer, and made spill format version 1 persist structured identities and declared schemas without a legacy reader.
- Preserved declared row types across scans, projections, joins, aggregates, CTEs, DML, cursors, foreign tables, generated columns, and schema rewrites instead of reconstructing types from materialized values.
- Matched PostgreSQL 18 `to_hex` overload selection and SQLSTATE behavior across queries, defaults, constraints, `ALTER TABLE ... USING`, and DML, rejected row-dependent default expressions, and reported failed check constraints as `check_violation`.
- Preserved PostgreSQL 18 `regclass` identity through parsing, type resolution, casts, foreign-table boundaries, schema expressions, and `pg_catalog.pg_type`, including the canonical OID, array OID, and I/O routines.

### Performance

- Replaced dynamic per-slot lookup in direct scored aggregation with a concrete projected-row representation, retaining structured metadata while removing the PostgreSQL 18 migration's analytical-query regression and accelerating cursor result scans.

## [0.1.2] - 2026-08-13

### Added

- Added scalar, table, and aggregate host-language SQL callbacks to the Node.js and Browser WASM bindings, including synchronous result enforcement, error propagation, optimizer safety options, derived-session lifetime management, and callback re-entry protection.
- Added matching unified-search, vector-KNN, graph/Cypher, storage/transaction, and extensibility programs for Rust, Python, Node.js, and Browser WASM, plus a browser runner and CI execution of every scenario.
- Added generated Node.js and handwritten Browser WASM callback types covering table result shapes, per-group aggregate state, volatility, and engine mutation declarations.

### Changed

- Extended Python callback registration with the same volatility and engine-mutation options exposed by the JavaScript bindings.
- Made Python and Node.js `close()` idempotently release each binding object's native engine reference so persistent files can be removed immediately after all related sessions close.
- Reorganized standalone Rust examples under `examples/rust` and made `examples/README.md` the authoritative language and platform parity matrix.
- Updated the manual, repository README, `llms.txt`, and UQA Engine skill with callback contracts, threading and reverse-dispatch constraints, lifecycle rules, and executable example parity requirements.

## [0.1.1] - 2026-08-12

### Added

- Added a compact `llms.txt` discovery map, a root `AGENTS.md` entry point, and one repository UQA Engine skill shared by Codex and Claude Code.
- Added a CI gate that compiles every manual SQL fence and executes explicitly classified examples in document order.
- Added a deterministic all-22-query TPC-H-derived scale-factor `0.001` fixture, exact PostgreSQL 17.10 result gate, package-scoped release timing runner, and live differential script.
- Added a machine-checked integration-harness coverage contract so test sources cannot silently become unregistered or duplicate Cargo targets.
- Added a backend-neutral clustered posting codec, score-only lazy cursors, and automatic atomic migration of existing SQLite and Key/Value/redb full-text indexes.

### Changed

- Aligned the licensing and contribution guides on copyrightable code and documentation, contributor-agreement scope, and the currently mergeable contribution paths.
- Standardized analyzer, operator-join, graph, and Cypher documentation around syntax, argument, result, effect, error, and example contracts.
- Consolidated integration sources into domain harnesses so workspace builds and tests share linker work while retaining direct module filtering.
- Replaced map-backed relational rows with positional `RowSchema` mappings and shared-fragment `PhysicalRow` composition across scans, projections, joins, aggregates, subqueries, spill boundaries, and result collection.
- Streamed eligible single-consumer derived-table projections into their parent operators while retaining materialization for blocking, repeatable, or volatile shapes.

### Fixed

- Made `fts_index_stats(table)` reject an unknown relation instead of silently returning statistics for every indexed table.

### Performance

- Added compiled projected predicates and aggregate inputs, borrowed canonical group keys, group arenas, reusable accumulator templates, lazy decimal SUM promotion, and once-per-query aggregate output and HAVING compilation.
- Decorrelated supported immutable `EXISTS` predicates into collision-safe borrowed-key hash probes and collected direct inner keys without projected-row materialization.
- Added borrowed-slot hashing for unique-key inner equijoins with exact collision verification and encoded spill fallback when `work_mem` is exceeded.
- Replaced one physical posting value per `(term, doc_id)` with 65,536-document term clusters, split score columns from positions, and connected exhaustive scoring plus WAND/BMW directly to 128-entry lazy score blocks.
- Removed per-query Bayesian catalog validation after the first execution-epoch lookup, resolved evidence parameters once per field, loaded multi-term block bounds in bulk, merged exhaustive term cursors without per-document maps, reused HNSW revisions, and ran independent fusion signals on the shared parallel executor.
- Stopped single-table and facet retrieval plans from rematerializing search-only text and vector fields after their predicates had already been consumed, then applied an exact tie-preserving score cutoff before document reads for score-first SQL limits; three unchanged persistent-SQL SciFact reruns had median text and hybrid latencies of 0.82 ms and 3.62 ms versus the 20.13 ms and 56.49 ms pre-pass baselines with identical rankings and relevance metrics.
- Reduced the local TPC-H-derived Q20-excluded sum of per-query release medians from 45.917 ms to 14.184 ms while retaining exact PostgreSQL results; this development snapshot is documented as local directional evidence rather than an audited TPC-H score.

## [0.1.0] - 2026-08-07

Initial preproduction release of UQA Engine.

### Added

- **Unified query runtime:** PostgreSQL 18-compatible SQL, full-text retrieval, vector search, graph queries, ranking, fusion, and machine-learning operators execute through one embeddable Rust engine.
- **Explicit query carriers:** `DocSet` owns document-support Boolean algebra, `Relation<K>` owns finite-support semiring combination, `PostingList` owns decorated posting storage, `RankedView` owns score order and top-K, and generalized postings preserve join-tuple identity.
- **Typed score domains:** raw BM25 scores, evidence logits, prior logits, and posterior probabilities use distinct public types so invalid mathematical combinations are visible at API boundaries.
- **Unified planning:** statements compile to `UnifiedPlan`, pass through the plan-native optimizer, and execute through `UnifiedPlanExecutor`; specialized retrieval paths remain explicit children of the shared plan.
- **Relational SQL:** schemas, `search_path`, DDL, DML, constraints, referential actions, MERGE, CTAS, recursive CTEs, set operations, subqueries, LATERAL joins, grouping sets, window frames, sequences, views, prepared statements, JSON/JSONB, arrays, temporal values, numeric values, `BYTEA`, and virtual PostgreSQL catalog views.
- **Subquery composition:** retrieval predicates compose with `IN`, `NOT IN`, `EXISTS`, and scalar subqueries in the same WHERE clause, and correlated subqueries resolve outer references written against either the outer table name or an alias.
- **Physical execution:** pull-based row batches, columnar result batches, bounded materialization, external sorting, spillable aggregation, disk-backed set operations, bounded hash joins, and streaming `sql_cursor` and `sql_columnar` APIs.
- **Join planning:** statistics-aware cardinality and cost estimation, DPccp inner-join enumeration, hash and nested-loop strategies, and explicit preservation of outer and lateral boundaries.
- **Text retrieval:** analyzers and filters, persistent GIN-style inverted indexes, BM25, query-level Bayesian BM25, multi-field search, highlighting, facets, staged retrieval, and score-aware SQL predicates.
- **Exact text top-K:** WAND and Block-Max WAND physical plans preserve duplicate query terms, field-scoped statistics, monotone Bayesian finalization, persisted bound fingerprints, and exhaustive top-K equivalence.
- **Vector and tensor retrieval:** vector and tensor SQL types, KNN predicates, distinct persistent IVF and HNSW physical indexes, calibrated vector matching, candidate-K provenance, and one-result-per-row tensor scoring.
- **HNSW duplicate-vector connectivity:** layer-zero pruning preserves a bounded deterministic backbone, so large groups of identical or near-identical vectors cannot isolate the entry point or produce a graph that fails persistence validation.
- **Legacy HNSW index compatibility:** SQLite catalog migration v20 recognizes historical `hnsw` rows whose durable metadata is IVF and records their actual physical index kind, so persistent-HNSW restore accepts valid pre-HNSW databases.
- **Fusion contracts:** exact Bayesian evidence fusion applies one prior to signed likelihood-ratio evidence, while robust positive-evidence pooling is separately named and documented as a ranking heuristic.
- **Calibration:** persisted scoring parameters, query-length scaling, model provenance, unsupervised score transforms, labeled reliability metrics, ECE, Brier score, log loss, bootstrap confidence intervals, threshold transfer, and candidate-K stability checks.
- **Graph runtime:** memory and SQLite graph stores, named graphs, Cypher reads and mutations, Apache AGE-compatible `agtype` values, graph pattern matching, RPQ automata, centrality, message passing, embeddings, path indexes, temporal traversal, and versioned deltas.
- **Graph carrier contracts:** graph payload support is validated, overlap behavior uses explicit policies, and the versioned Phi codec preserves graph context without claiming an isomorphism between arbitrary graphs and document sets.
- **Cross-paradigm joins:** relational, text-similarity, vector-similarity, hybrid, graph-driven, and generalized tuple-preserving join operators.
- **Persistent catalogs:** schemas, documents, constraints, postings, scalar and vector indexes, tensors, analyzers, graphs, scoring parameters, models, routines, views, sequences, foreign definitions, and statistics restore through shared catalog and backend boundaries.
- **Storage abstraction:** in-memory stores, relational SQLite stores, backend-neutral key/value contracts, a physical SQLite key/value backend, atomic batches, ordered prefix scans, range deletion, and reusable persistent-engine construction.
- **Backend-neutral sessions:** `PersistentStorageProvider` creates catalog and physical backend handles bound to one session, so `Engine::new_session` serves every backend rather than depending on a hidden SQLite connection.
- **Pure-Rust redb storage:** `uqa-storage-redb` implements ordered byte keys, atomic batches, MVCC read sessions, explicit write transactions, committed generation tracking, reopen, and SQL-compatible savepoints through a transaction-local undo journal.
- **Reusable storage conformance:** third-party `KeyValueStore` implementations can run shared ordering, cursor, batch, transaction, savepoint, read-only, and session-isolation checks.
- **Persistent index lifecycle:** B-tree, inverted, vector, tensor, graph, and block-max metadata remain synchronized across insert, update, delete, truncate, schema changes, transactions, rollback, and reopen.
- **Encrypted storage:** SQLCipher-backed catalogs, authenticated compressed containers using zstd or LZ4, automatic format detection, wrong-key rejection, and an external trusted-anchor contract for whole-file rollback detection.
- **Transaction and session isolation:** each logical session owns transaction affinity, variables, search path, prepared plans, cancellation, sequence state, statement cache, and statement serialization while published generations coordinate shared state.
- **SQL routines:** SQL-language and PL/pgSQL functions and procedures, `DO`, `CALL`, control flow, dynamic execution, diagnostics, exception handling, notices, set-returning routines, catalog persistence, and guarded recursion.
- **Runtime extensions:** embedders can register Rust scalar, table, and aggregate functions with explicit properties for transaction classification and optimization safety.
- **Foreign data wrappers:** foreign server and table contracts, predicate/projection/limit pushdown, and DuckDB, Arrow IPC, and in-memory handlers.
- **Machine learning:** serializable model specifications, analytical training, CPU inference for dense, convolutional, recurrent, graph, pooling, normalization, dropout, softmax, and attention layers, plus an optional Apple MLX backend.
- **Developer APIs:** the embedded `Engine`, fluent `QueryBuilder`, structured SQL parameters and results, graph and calibration helpers, and profiling APIs for text-search candidate, scoring, skip, and latency measurements.
- **Language bindings:** Python bindings through pyo3, asynchronous Node.js bindings with generated TypeScript declarations, and browser WASM bindings with SQLite persistence on IndexedDB.
- **Command-line shell:** `usql` supports in-memory and persistent databases, SQLCipher and compressed containers, one-shot and script execution, multiline editing, durable history, completion, highlighting, introspection, timing, output control, and Python-catalog migration.
- **PostgreSQL wire codec:** a network-independent PostgreSQL v3 protocol crate decodes frontend traffic and encodes authentication, row, command, notice, error, and ready-for-query messages.
- **Compatibility validation:** PostgreSQL 17 differential probes, Apache AGE container-captured fixtures, SQL golden files, storage reopen tests, transaction tests, graph codec properties, algebraic carrier laws, and randomized optimizer and top-K differential tests.
- **Performance evidence:** Criterion suites cover storage, SQL, planning, scoring, fusion, operators, graph, retrieval, calibration, and analytical execution, with provenance manifests and ratio-based regression gates for published baselines.
- **Rust baseline:** the workspace toolchain and declared minimum supported Rust version are Rust 1.90.
- **Workspace policy:** crate dependency budgets, public-repository hygiene, Rust source-header checks, file-size gates, formatting, Clippy, workspace tests, release builds, documentation checks, dependency audits, and benchmark compilation are enforced by repository scripts and CI.
- **Licensing policy:** AGPL-3.0-only remains the open-source base, with optional FOSS and noncommercial application exceptions, separate commercial licensing, and a contributor-rights policy that preserves the public core.
