# History

All notable changes to `uqa-engine` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.2] - 2026-10-08

### Fixed

- Resolve partitioned INSERT conflict arbiters from the target relation, preserving named parent constraints, child-only unique indexes for targetless DO NOTHING and physical row identity across partitions.

- Initialize the user catalog OID counter independently of reserved bootstrap namespaces while retaining persisted user-object claims and creation order.

- Preserve enum label identities when storing IN-list conditions, including trigger definitions after label renames, and accept peer-only RANGE window frames for types without offset arithmetic.

- Allow borrowed record scans without additional cursor workspace when a transaction has no private changes, preserving exhausted read allowances and cancellation.

- Preserve computed integer ordering keys as values after constant folding, including custom plans in SQL routines, instead of reinterpreting them as output-column positions.

- Restore separately persisted HNSW edges with one bounded source-node buffer, avoiding per-edge rewrites of spilled vectors while preserving topology validation and memory limits.

- Retain HNSW indexes for SQL replacements whose canonical vector bits and ordinals are unchanged, while preserving ordinary row publication and actual vector changes.

- Exclude native vector field conflict guards from catalog registry invalidation, including private transaction views and upgrades of existing cache triggers.

- Preserve statically selected graph dependencies after SQL literal coercion, so cursor snapshots do not decode unrelated graph entities.

- Preserve the recorded OIDs of private sequences when a callback refreshes committed catalog state, so bound `nextval` calls continue resolving the same sequence under REPEATABLE READ and SERIALIZABLE.

- Synchronize native SQLite WAL sequence-value logs at the consuming transaction boundary, including values obtained from another session's cache, instead of forcing FULL synchronization for each logged block. Preserve autonomous visibility, rollback, exact retry, generation lifetimes and sequence allocation; retain FULL publication on other storage paths. SQLite record format 60 adds the durable-prefix certificate and atomically upgrades predecessor metadata.

- SQLite managed transaction completion validates its exact durable receipt without a second physical acknowledgement commit. Ordinary writes retain FULL publication, retry ownership, manual acknowledgement and SSI recovery semantics.

- Reclaim SQL key-reservation identities after requests, waiters and held grants finish instead of retaining every historical digest for the database lifetime. Preserve native release, savepoint ownership and existing permanent table/key identifiers.

- Reject incompatible live SQLite storage owners before record initialization and retain owner admission through native catalog restoration, preventing upgrades from bypassing pre-table row locks. Current owners remain compatible, and process death releases admission through the existing owner leases.

- Compare complete cross-process row and key identities, including wait edges, instead of conflating hash collisions. Retain native pins by relation and keep temporary dependency admission separate from relation-registry lifetime locks.

- Retain successfully staged HNSW graphs across own writes on memory, SQLite and redb instead of reconstructing every generation. Certify private and uncontended autocommit boundaries, while preserving refresh after concurrent commits, undo, canonical validation and the original memory allowance.

- Batch already evaluated UNIQUE and PRIMARY KEY reservations under one shared claim-table arbitration per bounded group. Keep single-key acquisition, contention order, snapshot refresh and savepoint release semantics.

- Reuse ordinary statement analysis across data-only commits when the selected snapshot, definitions and parameter types still permit it. Re-optimize against current data on every invocation, preserving fresh parameter values, temporal input lifetimes, rollback and schema diagnostics.

- Reuse completed data-commit receipts for session cache adoption on native SQLite, SQLite Key/Value and redb, avoiding an extra read transaction and catalog-generation scan after a data-only commit. Preserve refresh after intervening commits, definition changes, rollback and uncertain completion.

- Store SQLite MVCC version payloads by stable physical address, with keyed metadata for historical reads and reclamation, reducing WAL write amplification when a commit changes several record families. Initial open atomically converts SQLite record format 58 and earlier to the addressed layout now retained by format 60 while preserving histories, receipts, snapshots and FULL synchronization.

- Retain parsed SQL and structural plans across data-only changes while invalidating executable access paths. Persistent and explicit-transaction statements still lower and optimize under their selected snapshot, preserving live data, rollback and schema checks.

- Share SQLite managed transaction allocation commits through a bounded reserve of already durable receipts, preserving receipt quotas, per-call memory and cancellation checks, process-loss recovery and SERIALIZABLE publication ordering.

- Publish automatic ANALYZE samples while concurrent row writes continue, retaining changes after the sample for the next refresh. Reject obsolete samples after a newer ANALYZE, column changes or relation replacement across native SQLite, SQLite Key/Value and redb.
- Reuse custom/generic executable plans inside SQL-language and PL/pgSQL routine bodies, including static cursor sources, with the existing `plan_cache_mode` policy. Preserve per-call arguments, live data, input-constant lifetimes, catalog invalidation and result checks without repeating analysis and optimization of a reusable plan.

- Retain PL/pgSQL record-field parameter types across executions. Report PostgreSQL’s `42804` diagnostic when an evaluated field changes type, preserve unevaluated CASE branches and compatible typmod changes, and accept fresh types after plan invalidation.

## [0.5.1] - 2026-10-07

### Fixed

- Initialize CREATE TABLE AS column identities and attribute slots through ordinary schema publication, preserving valid catalogs after relation-name waits, across sessions and on persistent reopen.
- Probe complete composite UNIQUE tuples during mutation checks instead of reading all rows sharing the leading key. Preserve partial predicates, NULL semantics, command-local changes and rollback; initial open repairs missing tuple postings atomically, and SQLite record format 58 and redb record format 56 exclude older writers.
- Prepare operator-join predicates in each input relation’s own scope, restoring unified-search examples across language bindings while retaining constant-error ordering and namespace checks.
- Reduce nested routine execution stack frames so shallow recursion succeeds in development builds without weakening the native stack guard or recursive-error recovery.
- Preserve legacy key constraint names when initial analyzer restoration saves a table before its owned index registry is converted. Databases combining predecessor column keys and full-text fields now reopen with their constraints, documents and search index intact; converted catalogs continue to store key names only on owned indexes.

## [0.5.0] - 2026-10-07

### Added

- Add PostgreSQL composite attributes with ordered validation, stable attribute identities and NULL extension of existing nested values, stored expressions and prepared constants. Preserve atomic rollback and initial-only conversion of predecessor constructors across supported storage providers.

- Support mutable EXECUTE privileges for builtin catalog routines, including schema-wide targets, aggregate/window identities, grant dependencies, transaction rollback and persistent reopening. Retain analyzed function identities through constant planning, preserve column authorization when pruning unused outputs, and check surviving calls before row production or argument side effects.
- Reconstruct PostgreSQL routine definitions with `pg_get_functiondef`, preserving stored source, SQL bodies, argument modes, set results, attributes and configuration without executing the routine. Allow equivalent inferred/explicit result declarations during replacement, preserve PostgreSQL result-change diagnostics, and retain host callback precedence during builtin preparation.

- Implement PostgreSQL domain constraint addition, removal and validation, including NOT VALID checks, dependent stored values, catalog identities and transactional restoration across all storage providers.
- Support PostgreSQL 18 configuration definitions, custom parameter placeholders, `set_config`, `SHOW ALL`, `client_min_messages` and startup values restored by `RESET`.
- Implement `statement_timeout`, `lock_timeout`, `idle_in_transaction_session_timeout`, `idle_session_timeout`, `transaction_timeout`, and the `pg_sleep` functions. Report the cancellation reason and preserve permanent session termination through `Engine::session_termination`.
- Expose `Engine::set_query_memory_limit` for host-controlled query workspace limits below SQL `work_mem`'s 64 kB minimum.
- Add enum types: `CREATE TYPE ... AS ENUM`, `ALTER TYPE ... ADD VALUE [IF NOT EXISTS] [BEFORE | AFTER]`, `RENAME VALUE` and `DROP TYPE`, `pg_type` and `pg_enum` projection with PostgreSQL's sort positions, unknown-literal input, text casts, comparisons, the `enum_*` routines, arrays and JSON output, the uncommitted-label rule (`55P04`), and enum partition keys; labels reach the Python, Node.js and WASM boundaries through the catalog.
- Add standalone composite types: `CREATE TYPE ... AS (...)` with a generated array type, composite values as named records with position-wise coercion, `record_in` and `record_out`, field selection and assignment, nested composites and composite arrays, catalog and `information_schema` projections, and `DROP TYPE ... CASCADE` of an attribute's type.
- Add the type object lifecycle: `ALTER TYPE | DOMAIN ... RENAME TO | SET SCHEMA | OWNER TO`, `GRANT | REVOKE USAGE ON TYPE | DOMAIN` with `typacl`, `has_type_privilege`, `aclexplode`, `pg_depend` and `pg_shdepend` derived from the catalog, `pg_get_constraintdef`, `pg_get_function_arguments`, `pg_get_function_identity_arguments`, `pg_get_function_result` and `pg_get_function_sqlbody`, and `DROP TYPE | DOMAIN` and `DROP ROLE` details in PostgreSQL's order.

### Changed

- Replace repeated native row/key claim header and slot I/O with bounded shared file mappings on supported local filesystems. Preserve exact lock conflicts, cross-process publication, interrupted-writer recovery, bounded fallback and post-reservation snapshot freshness.

- Reuse native SQLite file identity while supported local filesystem watches report no path or mount change, avoiding a pathname lookup on every connection checkout. Preserve immediate replacement rejection, including ancestor renames, and direct checks on unsupported filesystems.

- Avoid repeated historical reads of newly inserted native SQLite records during COMMIT and skip unrelated spilled values during original-row and retired-owner validation, preserving conflict checks, physical-key ownership, atomic publication and receipt retry.

- Read unchanged native SQLite rows sequentially in retained snapshots and resolve historical values only for newer row versions, preserving private changes, deleted rows, pagination and selected BLOB hydration.

- Stream native GIN document keys with memory bounded by one document’s indexed fields instead of retaining and sorting the entire corpus; preserve exact counts, private snapshots and ordered key pagination.

- Read consecutive native SQLite index results through one bounded document cursor instead of resolving each row separately. Preserve missing rows, private changes, historical snapshots, selected BLOB hydration and early cancellation. GIN corpus counts traverse document identities without fetching every field length or retaining a distinct-document set; count-only result presence can use the latest committed document projection.
- Carry anonymous SQL rows in `RowValue` so their field type identities survive storage and copying. Rust callers constructing `Value::Row` from a vector use `Value::Row(values.into())`; existing serialized rows remain readable.

- Use scalar indexes for row-independent parameter expressions such as `qty = $1 + 1`, including reversed comparisons, ranges and membership bounds. Evaluate each execution's parameters while preserving NULL behavior, comparison errors and CASE branch selection.
- Probe exact field indexes before stored-row scans when private changes are present. Mask replaced or deleted identities, preserve private matches and explicit NULL fields, and retain typed comparison errors and serializable read dependencies.
- Stream native SQLite private document and B-tree merges through retained spill entries, and reuse selected private readers for requested-ID projections in native SQLite and common Key/Value storage, including SQLite Key/Value and redb. Sparse requests seek spill blocks instead of scanning gaps; presence reads avoid private payloads, and stopped value callbacks do not load later payloads. Preserve input order, duplicates, fixed views, savepoint undo and callback reentry.
- Use the default NEON ChaCha20 backend on supported ARM64 targets for encrypted temporary files and compressed SQLite containers, preserving ciphertext formats and key and stream-buffer zeroization.
- Maintain native SQLite transaction cache revisions by changed record family and owner, so catalog refresh after a large write does not rescan every private row. Revision summaries spill under the same memory allowance and preserve savepoints and retained reads.
- Default `search_path` to `"$user", public`, preserve its assigned text and empty paths, and make `Engine::set_search_path` return a `Result`. `QueryCancelled` now carries a `CancellationReason` instead of being a unit struct; see the [0.5.0 upgrade notes](docs/manual/reference/10-upgrading.md#050).
- Store type references by OID identity in routine signatures, casts, typed constants and view plans, and enum constants by label identity, so that names follow renames; deparse stored definitions through the catalog with search-path visibility; compile string-bodied routines at first use in each session, as PostgreSQL's function cache does.
- Run `CREATE TABLE` in `DefineRelation`'s order: `MergeAttributes`, `transformColumnDefinition` and `PARTITION OF` column options with PostgreSQL's diagnostics and notices, CHECK constraints named and merged in written order, defaults and generation expressions cooked as `cookDefault` cooks them, and the OIDs of the relation, its defaults, constraints and indexes allocated where `heap_create_with_catalog` and `DefineRelation` allocate them.

### Fixed

- Compile SQLite path watching only on supported native targets, restoring browser WASM builds while preserving direct file-identity checks and replacement rejection on other targets.

- Use scalar index candidates through table aliases, column aliases and join inputs, and reconcile indexed equality reads with cached command keys instead of scanning every staged row. Retain fixed query views, private replacements, tombstones and serializable predicate observations.

- Let point UPDATE modify a row staged by an enclosing command, and honor staged replacements and tombstones in integer-key reads. Preserve nested-trigger effects when the outer batch publishes.

- Cache staged UNIQUE expression keys so batch checks do not reevaluate every preceding row. Preserve partial predicates, composite and NULL keys, nested triggers, encrypted spill and statement rollback across all storage providers.

- Use child indexes for foreign-key parent checks and referential actions, avoiding reads of unrelated child payloads while preserving private writes, latest-committed references and partition behavior. Deleting a NULL parent key now leaves NULL child keys unchanged, matching PostgreSQL.

- Render stored partition key expressions with their current type and enum-label metadata during bound validation and catalog projection, preserving partition creation, diagnostics, rename and durable reopen.

- Reject set-valued operands in individual IN comparisons before rewriting, while preserving PostgreSQL scalar-array expansion and ordered analysis errors.
- Preserve PL/pgSQL RAISE diagnostic options for SQLSTATE, message, detail and hint, including typed output, ordered effects, NULL and duplicate diagnostics and bare rethrow.

- Support SQL foreign-data wrapper declarations and deletion, exact validator signatures, native handler aliases, ordered options, transactional callbacks, durable identities and foreign catalog projection. Preserve dependency-aware RESTRICT/CASCADE and missing-reference behavior across rollback and reopen.

- Support foreign-column DROP through both ALTER FOREIGN TABLE and ALTER TABLE, preserving PostgreSQL notices, dependency checks, CASCADE, atomic multi-column changes, rollback and durable attribute slots.

- Preserve PostgreSQL relation attribute numbers and dropped slots across column deletion, addition, rename, type changes, rollback and persistent reopening. Keep catalog constraints, indexes, column privileges and stored definitions bound to the surviving columns.

- Preserve PostgreSQL grouped subquery assignments in UPDATE, ON CONFLICT and MERGE, including positional values, original-row correlation, statement-scoped volatile evaluation, NULLs, constraints, stored bodies and second-row cardinality errors.

- Initialize builtin namespace records consistently in memory and persistent catalogs, allowing user relations in `information_schema` and `ag_catalog` to survive creation, rename and reopen while preserving existing schema grants and identities.

- Preserve temporary foreign definitions, generated sequences and privileges through creation, catalog refresh, rename and rollback; keep their metadata and dependent temporary-view reference rewrites out of durable storage, retain role dependencies across sessions, and remove session-owned memory rows on `DISCARD TEMP`.

- Create session-local materialized views in `pg_temp`, including names resolved through `search_path`, and preserve their private rows and population state through refresh, rollback and rename without writing them to the durable catalog.

- Restore generated-column rejection of host callbacks without declared SQL return types, including nested calls, explicit casts and branches eliminated by planning. Explicitly qualified builtin calls remain valid.

- Register the PostgreSQL builtin identities and catalog attributes of array dimension, bound and cardinality functions. Bind their polymorphic array and integer dimension signatures consistently, preserving concrete array types and PostgreSQL input diagnostics while enforcing EXECUTE privileges on direct and stored calls.

- Validate CREATE TABLE foreign keys in written order, preserving repeated column REFERENCES, constraint naming and catalog order. Match PostgreSQL key diagnostics and defer generated sequence ownership until after table constraints; retain the correct owner for explicitly named cross-schema identity sequences and require an owned sequence for identity DEFAULT values.

- Preserve stored role-constant dependency checks after input conversion and partition-key optimization, and match PostgreSQL routine declaration error ordering and CHECK diagnostics.

- Resolve named array-transform calls alongside user overloads during preparation and stored binding, preserving argument positions and declared Boolean option types. Reconstruct stored named calls with PostgreSQL argument notation.

- Preserve wildcard syntax while binding SQL-standard routine parameters, including stored-column rename and deletion, and omit redundant casts of parameters whose declared type already matches the selected argument.
- Honor the session DateStyle order for numeric date inputs, including arrays, domains and ranges; normalize partial settings and aliases with PostgreSQL diagnostics, preserve prepared and stored input values, and carry the active order through transactions, cursors and parallel execution. Short BC years retain their written era. Stored domain-array inputs execute their checks once, and interval input retains its declared fields.
- Resolve scalar element types for multidimensional ANY/ALL and polymorphic array functions, accept catalog vectors as anyarray inputs, and retain array-domain conversions in stored comparisons.
- Preserve PostgreSQL IN-list analysis and selected OID-alias conversions in stored definitions. Keep array versus individual comparisons, volatile evaluation counts, independent scalar-subquery initialization, current versus enclosing row scopes and existing index/key access. Correct result-type derivation for correlated scalar subqueries in stored views, including qualified columns after rename and reopen.
- Select and retain the common element type and array concatenation overload of `||`, including scalar append/prepend and typed NULL elements. Preserve selected conversions in stored definitions, PostgreSQL operator/input/dimension diagnostics, lower bounds and domain-array reconstruction.
- Correct multidimensional array text input and casts to use the scalar element type for every leaf, including declarations with repeated array brackets. Preserve actual dimensions, lower bounds, numeric modifiers and PostgreSQL input diagnostics across assignment and reopen.
- Preserve analyzed operator and common-type conversions in stored definitions, including numeric arithmetic/comparison inputs, CASE result casts, domain-to-base relabels and array coercions. Cross-type operators retain their independently selected operand types. Keep implicit array conversions distinct from explicit constructor casts through SQL syntax, scalar IR and persistent reopening; retain original values, types, lazy evaluation and input diagnostics. Preserve expression equality for inherited CHECKs, conflict-index inference and grouping when identical conversions have different display origins. Apply parameter-default assignment conversions and domain checks during SQL function inlining instead of treating declaration metadata as an already converted value.

- Check generated expressions, index expressions and predicates, and partition keys for immutability after SQL planning. Preserve original stored calls and dependencies while allowing eligible SQL function inlining, discarded arguments and lazy branches; retain PostgreSQL error order, VIRTUAL restrictions, argument cast volatility and constant partition-key rejection. Reuse common SQL analysis instead of a separate generated-expression type checker. Preserve unrelated stored generated values when backfilling or rewriting another column, reject invalid common array/temporal signatures, render renamed partition-key functions by their retained identity, and merge inherited CHECK constraints containing the same analyzed integer constant without erasing integer-width differences.
- Prepare each PL/pgSQL expression and static statement at its first reached execution, preserving independent branch settings, live variables, ordered block defaults, polymorphic/trigger specializations and recursive reuse. Retry failed initial analysis, retain successful preparation across execution errors, and reanalyze invalidated inputs from the retained syntax without changing string escapes or repeating lexical warnings.
- Match PostgreSQL SQL-function caller-plan lifetimes: retain selected arguments and result coercions through safe inlining, preserve custom/generic scanner behavior and notices, and keep static inspection separate from actual invocation. Retain successfully analyzed SQL body inputs across calls, including enum identities after label rename, while reanalyzing changed dependencies and concrete parameter types. Correct strict constant-NULL privilege behavior and reject DISCARD ALL inside implicit multi-statement transaction blocks before changing session state.
- Apply GRANT and REVOKE across user-defined functions and procedures in named schemas with PostgreSQL target order, namespace privileges, atomic ACL publication and durable identities. Correct the permission error for a grant attempted without EXECUTE or grant-option authority, including NOINHERIT role membership.
- Honor PostgreSQL string scanner settings at SQL-message boundaries, including legacy backslash escapes, prepared literals and routine-local settings during compilation and dynamic SQL execution. Preserve original parser SQLSTATE, DETAIL, HINT and warnings, including warnings before errors and current notice filtering on cache hits. Ordinary statement caches reanalyze session-dependent input values while preserving reusable immutable inputs and PREPARE's separate lifetime.
- Preserve the separate integer `generate_series` signatures and selected result type in stored SQL bodies, views and `ROWS FROM`. Correct legacy two-argument builtin bindings before catalog restoration, preserving user identities and later search-path shadows. Expose the four implemented overloads and their planner support metadata in `pg_proc`.
- Restore `pg_get_function_sqlbody` output for built-in SQL routines from their existing typed catalog bodies. Preserve selected routine identities when reconstructing SQL bodies, views, defaults and generated expressions under search-path shadows.
- Correct finite `EXTRACT` and `date_part` values, exact numeric microseconds, symbolic interval epoch, BC calendar fields, invoking-session time zones and TIMETZ offsets. Preserve the selected source type, result type, NULL handling and PostgreSQL unit diagnostics through prepared and stored expressions. Restore all twelve routine identities and the SQL date wrapper; built-in routine metadata reports its actual language and exposes source text only to the owner or enabled roles. The missing `pg_language` catalog and its row descriptors now support routine-language joins with consistent stored SQL/PLpgSQL identities.
- Correct finite `date_trunc` interval and timestamp behavior: preserve symbolic interval fields, truncate signed values correctly, follow BC calendar boundaries and PostgreSQL unit aliases, and distinguish unsupported units from unknown-unit diagnostics. Stored defaults, generated columns, views and prepared expressions use the same scalar behavior. Timestamp input and truncation results enforce the PostgreSQL Julian lower bound; timestamptz input checks its final instant after applying the UTC offset. Timestamptz truncation honors explicit and session time zones, historical offsets and DST transitions; TimeZone settings validate and normalize named, numeric and interval forms, and generated expressions use the selected overload's volatility. Typed date/timestamp casts to timestamptz interpret local time in the invoking session zone, including prepared and default expressions. Date-only timestamp inputs accept PostgreSQL's adjacent numeric offsets and retain exact displacement and BC-calendar diagnostics.

- Restore PostgreSQL catalog identities for the five clock routines, text lower/upper overloads, eleven mod/power/pow/sqrt/cbrt overloads and seven truncation/interval-normalization overloads, including regproc/regprocedure input and output, catalog metadata, stored references and search-path shadowing.

- Match PostgreSQL's action-specific ALTER TABLE errors on regular and materialized views, including the local relation name and DETAIL. Resolve the target and ownership before validating declarations, preserve written action order, and skip invalid type modifiers when IF EXISTS or IF NOT EXISTS skips the declaration. Keep missing-column and USING errors ahead of target type modifiers and retain ordinary-table key validation.
- Preserve named WINDOW declarations and selected OVER references in stored view definitions, including quoted names, inheritance, equivalent inline definitions and unused declarations. Analyze each definition's literal inputs once, retain its dependencies, and preserve prepared values and legacy expanded definitions across reopen. Correct window passes over projected rows so nested views and multiple sorts retain the actual physical row layout in memory and spill.
- Analyze CREATE VIEW literal inputs before output aliases and target checks, retaining definition-time conversions across execution and reopen while leaving runtime expressions deferred. Failed replacement preserves the previous view.

- Resolve ordinary WITHIN GROUP calls with both direct and ordering arguments, preserving PostgreSQL overload selection, modifier diagnostics, FILTER order and implicit-input effects. Retain ordered-set syntax and function identity in stored definitions, restore legacy expressions and accept typed prepared percentile fractions. Preserve EXTRACT keyword syntax independently of explicit extract function calls in stored catalog definitions and executable SQL reconstruction.
- Read unknown domain-array literals through catalog-aware input functions and retain their converted values before optimization. Preserve element constraints, array bounds, prepared input lifetime and single evaluation of input-function effects through DEFAULT, CHECK and USING. Retain default source types during assignment and preserve scalar-domain runtime checks, including NULL.
- Store sequence-function column defaults as regclass OID constants and retain selected argument coercions. Preserve sequence identity through rename and reopen, explicit text late binding, user-defined overloads and PostgreSQL's definition-time versus execution-time errors for ordinary and foreign tables.
- Analyze ordinary SQL literal inputs before optimization in PostgreSQL's relation, expression and clause order, including UPDATE RETURNING and window specifications. Retain catalog-only cursor references without capturing unscanned table rows.
- Validate SQL source-body literal inputs and return layouts in PostgreSQL order without executing the body. Preserve failed replacements and typed row descriptors through CASE, materialization, spill and routine returns; resolve relation row types for routine signatures.

- Analyze routine parameter defaults in PostgreSQL declaration order, including unknown input conversion and delayed call-time evaluation. Preserve NULL defaults, polymorphic default types, creation-time constants and durable bindings; reject removal or type changes of existing defaults during routine replacement.
- Match PostgreSQL materialized-view creation order for source analysis, target collisions, column lists and schema/type privileges; report IF NOT EXISTS notices and skip source evaluation for existing targets. Unpopulated materialized-view errors use the relation's unqualified name and include the REFRESH hint, including through stored views and after reopen.
- Preserve foreign-server catalog identity, owning role and TYPE/VERSION metadata through transactions, refresh and reopen; prevent dangling owners after DROP ROLE, and match PostgreSQL server creation diagnostics and notices. Implement DROP SERVER ownership, IF EXISTS notices, RESTRICT/CASCADE dependency deletion and replacement revalidation after waits. Foreign tables retain their original server references through concurrent deletion and same-name recreation. Upgrade old rows once while preserving connection options; see the [upgrade guide](docs/manual/reference/10-upgrading.md#foreign-server-identities-and-owners).
- Restore stored SQL-standard routine relation bindings without requiring the rollback caller to have access to their schemas, preserving the original statement error. New routine definitions retain their namespace permission checks.

- Report PostgreSQL `22023` diagnostics for invalid NUMERIC precision, scale and modifier counts in declarations and casts; validate cast types even for NULL inputs, empty results and unselected CASE branches.
- Preserve PostgreSQL foreign-table declaration order and diagnostics across type lookup, constraint attributes, forbidden keys and EXCLUDE. Retain table-level NOT NULL constraints and their names through transactions and reopen, keep namespace and IF NOT EXISTS checks ahead of definition analysis, and report PostgreSQL's unsupported-utility error in SQL-standard routine bodies while preserving quoted bodies.
- Expose relation row types and generated arrays through `pg_type`, `regtype` and `format_type`, preserving catalog identities, generated names and search-path shadowing across rename, schema movement, rollback and reopen. Move indexes and owned sequences with their table. Reject extra dimensions on generated array names with PostgreSQL's `42704` error.
- Evaluate multi-column type rewrites from original typed rows and propagate changes once through inheritance and partition hierarchies. Preserve dropped-column inputs, generated values, callback effects, constraints, indexes and rollback across all storage providers; spill retained rows and callback identities under the statement allowance.
- Preserve prepared input constants and reanalyze original syntax after changes to selected catalog dependencies or the effective namespace. Match PostgreSQL's DDL rollback behavior, identical routine replacement, enum label identity and sequence OID diagnostics; refresh domain checks without rereading unrelated inputs.
- Analyze `ALTER COLUMN TYPE USING` before the target and new type, including on empty tables. Preserve PostgreSQL's inherited and partition-key rejection, generated-column and identity-sequence diagnostics, assignment errors, constant-planning errors and lazy conditional evaluation.
- Check an added column's target before its type and clauses: a directly targeted partition reports `42809`, duplicate and system-column names report `42701`, and `IF NOT EXISTS` skips an existing ordinary column with PostgreSQL's notice. Preserve recursive additions and statement and savepoint rollback.
- Release the selected role catalog guard before describing DROP ROLE dependencies, preventing a fixed-snapshot catalog refresh from deadlocking while preserving dependency errors and role tuple identity checks.
- Release SQLite coordination descriptors when their last transport or lease closes, without waiting for another database open; preserve live aliases, snapshot readers and serializable participants.
- Read spilled record metadata without loading its payload, and reuse exact transaction-local requirements instead of rereading endpoint and membership revisions for every graph edge. Keep requirement keys and lookup nodes within the original session allowance, preserve savepoint undo and external observed revisions, and retain conflict validation at command refresh and commit.
- Evaluate retrieval predicates in table UPDATE through their document support instead of the unsupported scalar-call path. Preserve boolean conditions, CTE scope, nested-write conflicts and lock-wait rechecks, and infer text query parameters for `text_match`.
- Reclaim the cross-process row-change journal before the oldest live snapshot instead of retaining every update for the database lifetime. Preserve update chains, key reuse and logical change numbers across reclamation; release dead processes' retained history through operating-system liveness locks.
- Preserve writes made by nested VOLATILE routines when INSERT, UPDATE, DELETE and MERGE publish prepared rows, including primary-key and partition movement. Make preceding UPDATE FROM rows visible to later callbacks, keep original RETURNING images, and restore enclosing command rows and exact-key caches through nested exception and savepoint rollback.
- Include staged command rows in indexed UPDATE and DELETE candidates, including equality, range and NULL predicates; newer replacements and deletions mask stored index entries.
- Preserve PostgreSQL SQLSTATE `53200` when a text-index build or analyzer rebuild exhausts its allowance, including GIN removal rebuilds, without duplicating the operation prefix. Keep typed cancellation and statement rollback through the same adapters.
- Spill retained DiskANN population inputs and ordered IVF/HNSW transaction inputs so vector transactions can exceed the session allowance across native SQLite, SQLite Key/Value and redb. Preserve fingerprints, counts, conflicts, refresh and savepoint undo; DiskANN validation and header replacement seek exact field ranges instead of rescanning unrelated origins.
- Stream individual private spill entries through a charged 1 KiB buffer when complete read blocks cannot fit; preserve lookup/cursor bounds and record metadata while allowing native HNSW deletion/rebuild publication under its unchanged session allowance.
- Reuse resident record prefixes when evaluated MVCC batches spill, share their memory allowance across record groups, and avoid unnecessary cursor allocations and spilled-run handle overhead.
- Admit retained HNSW mutation inputs before constructing a derived graph, so an oversized input fails without first building a graph that cannot be published.
- Release completed IVF reconstruction scratch before later resident preparation; stream native SQLite, SQLite Key/Value, redb and shared commit-time IVF vectors/assignments through encrypted temporary roots, and spill tensor-score reduction under the unchanged session allowance. Preserve centroids, cosine payloads, retained readers and transaction undo; see the [preservation argument](docs/plans/0017-native-ivf-bounded-storage.md).
- Synchronize deadline cancellation with handle cleanup so a dequeued timer cannot cancel the next statement after its original handle is dropped; retain explicit cancellation and permanent session termination.
- Validate PRIMARY KEY, UNIQUE and partitioned unique-index declarations with PostgreSQL's column requirements, duplicate-declaration handling, creation order and index-build diagnostics.
- Check immediate foreign keys after the statement writes its rows, and order referential actions with AFTER triggers in one statement queue. Preserve statement-trigger sharing and reject rows already modified by triggered commands with SQLSTATE `27000`; expose `pg_trigger_depth()`.
- Finish unreferenced data-modifying CTEs after the main query in reverse definition order, defer their AFTER events to the complete statement, and preserve command-level repeated-row handling.
- Validate SQL and PL/pgSQL routine bodies under `check_function_bodies`, check declared result types, and defer string-body analysis when validation is disabled. Preserve SQL-standard body validation and reopen of deferred bodies.
- Compare complete relation identities when coordinating locks within and between processes, preventing unrelated relations with colliding hashes from blocking each other or creating false deadlocks. Retain native slots through holder and waiter lifetimes and reject stale wait metadata when slots are reused.
- Route `INSERT`, `UPDATE`, `DELETE` and `MERGE` through automatically updatable views onto an underlying view's `INSTEAD OF` trigger, count the rows a suppressed trigger lets through, and report non-updatable views with `view_query_is_auto_updatable`'s DETAIL and HINT.
- Check `CREATE FUNCTION` and `CREATE PROCEDURE` attributes as PostgreSQL does (repeated and procedure-only attributes, `SET`, `COST`, `ROWS`, `SUPPORT`, `PARALLEL` and the language), and place new relations in the namespace and with the persistence `RangeVarGetCreationNamespace` and `RangeVarAdjustRelationPersistence` assign.
- Select common types as `select_common_type` does for `IN` lists, `CASE` and same-category operands, cast OID aliases to `name`, `varchar` and `char` through their output functions, and draw every catalog OID from one database counter in creation order, unchanged by reopening.
- Create a new table's NOT NULL constraints as `AddRelationNotNullConstraints` does: after the CHECK constraints, one per column from the column clauses, table constraints and PRIMARY KEY columns in declaration order and then from the parents, with PostgreSQL's `42601`, `42703`, `0A000`, `42804`, `42710` and `23505` diagnostics, inherited names kept unless the table holds them, every constraint validated, and the OIDs PostgreSQL allocates.
- Report a common type conflict with its construct as `select_common_type` and `coerce_to_common_type` do (`UNION types integer and date cannot be matched`, `42846` `CASE/WHEN could not convert type regtype to regclass`), resolve inputs that are all `unknown` to `text` in set operations and `CASE`, and read an `unknown` literal with the selected type's input function when the statement is analyzed, so `CASE WHEN true THEN 1 ELSE 'x' END` and `1 IN (1, 'x')` report `22P02` as PostgreSQL does.
- Read `regclass` and `regclass[]` literals in column defaults, generation expressions, CHECK constraints, domain defaults, views and prepared statements when the statement is analyzed, as `regclassin` does: a missing relation reports `42P01` at definition, stored constants carry the relation OID and print by its current name, `EXECUTE` resolves a prepared statement's names again, digit strings are OIDs with `oidin`'s `22P02` and `22003` for every OID alias type, malformed names report `42602` `invalid name syntax`, lookup failures name the parsed components, `to_regclass` returns NULL for those inputs, and casts of string literals to `regclass[]`, `regproc[]`, `regprocedure[]` and `regnamespace[]` read each element.
- Read `regtype`, `regproc`, `regprocedure` and `regnamespace` literals in column defaults, generation expressions, CHECK constraints, domain defaults, views and prepared statements when the statement is analyzed, as their input functions do: a missing type, function or schema and an ambiguous function name report `42704`, `42883`, `3F000` and `42725` at definition, stored constants carry the object OID, print with the output functions' canonical names and follow renames, `DROP TYPE`, `DROP FUNCTION` and `DROP SCHEMA` report the dependent defaults, constraints and views, prepared statements keep the OIDs at `EXECUTE`, and every statement checks its `reg*` literals before it runs.
- Resolve comparisons on `oid` and its alias types through the `oid` operators as `oper_select_candidate` does: alias operands are relabeled to `oid`, integer operands cast and `unknown` literals read by `oidin` (`22P02` for a name), CHECK constraints and views store and print the relabels (`(a)::oid <> ('t'::regclass)::oid`), `BETWEEN`, `IS DISTINCT FROM`, `NULLIF`, `CASE` tests and `= ANY` relabel the same way, `max` and `min` of an alias column return `oid`, rows of alias columns hold OIDs, and `oidin` reads text as `strtoul` with base 0.
- Select operators with `unknown` operands as `oper_select_candidate` and `func_select_candidate` do: an `unknown` literal is read by the operand type the selected operator declares (`1 + '1'` stores `(a + 1)`, `1.5 + 'x'` and `true = 'x'` report `22P02`), the last heuristic assumes the known operand's type after a category conflict (`time '10:00' + '1 hour'` selects `time + interval`), `-'1'` and `'1' + '2'` report `42725` `operator is not unique` with PostgreSQL's hint, `1 || 2` reports `42883` since `||` needs a text operand, an array pair, a `bytea` pair or a `jsonb` pair, the `unknown` operand of `||` takes the typed operand's type (`'x'::bytea || 'y'` joins bytes), stored expressions print the read constants, a domain's default is stored in `pg_type.typdefaultbin`, and `numeric field overflow` reports PostgreSQL's DETAIL.
- Rename schemas with `ALTER SCHEMA ... RENAME TO` as `RenameSchema` does: the namespace keeps its OID, owner and privileges, every relation, routine and type it holds follows under the new name with the definitions that name them rewritten, `regnamespace` and `regclass` constants print the new name, and a missing schema, a taken or reserved name and a non-owner report `3F000`, `42P06`, `42939` with PostgreSQL's DETAIL and `42501`.
- Read date and time text with PostgreSQL's input functions: the special values `now`, `today`, `tomorrow`, `yesterday`, `epoch` and `allballs` resolve against the transaction start so `'now'::timestamp = now()::timestamp` holds across transaction boundaries in a Simple Query message, temporal comparisons and range bounds use that same clock, and `DEFAULT 'now'` stores the creation time; compact, slash, dot and month-day-year dates, `am` and `pm`, the sixtieth second and offsets with seconds are accepted, the diagnostics are `22007` `invalid input syntax`, `22008` `date/time field value out of range` with the `DateStyle` hint, `22009` `time zone displacement out of range`, `22023` for an unknown zone and `22015` `interval field value out of range` instead of `42804`, a generation expression selects the operator for `ts + '1 day'` as `oper_select_candidate` does, `time + date` and `timetz + date` produce timestamps, a value of another type has no cast to a temporal type (`42846`), years print without a sign past 9999 and with `BC` before the common era, a written cast of a literal in a stored expression prints as the constant the input function read with `format_type`'s spelling (`'11:00:00'::time(3) without time zone`, `'\x79'::bytea`), and the `~`, `~*`, `!~` and `!~*` operators name their output `?column?` and print as operators.

## [0.4.9] - 2026-10-03

This release bounds HNSW and transaction retention with encrypted temporary storage and corrects PostgreSQL 18 sequence, foreign-key, transaction and diagnostic behavior. Native SQLite mapping advances from 13 to 14, and persistent sequence definitions and values are stored separately. Stop every database owner, retain a closed pre-upgrade backup and update all owners together. Rust persistence and HNSW adapters and consumers of Rust/Python notices need API updates; see the [0.4.9 upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.4.9/docs/manual/reference/10-upgrading.md).

### Changed

- Store sequence values independently of catalog definitions, avoiding catalog refreshes when reserved values are published. Native SQLite migrates retained sequence revisions to mapping format 14; SQLite Key/Value and redb migrate sequence definitions and value records on open. Earlier binaries cannot consume the upgraded sequence representation.
- Preserve PostgreSQL notice levels, SQLSTATEs, messages, details and hints through Rust, Python, Node.js, WASM, the PostgreSQL server and `usql`. Rust returns `SQLNotice` values and Python returns dictionaries instead of pairs; Node.js and WASM notice objects gain the diagnostic fields.

### Fixed

- Spill private transaction changes and prepared publication records to encrypted temporary files under the existing session allowance. Preserve savepoints and conflict preconditions, stream SQLite/redb publication and release decoded run caches when reading ends.
- Keep HNSW graph construction, restoration, mutation generations, publication and search workspaces bounded by their retention allowance using encrypted temporary storage across memory, native SQLite, standalone SQLite, SQLite Key/Value and redb. Preserve graph topology, canonical scores, retained snapshots and transaction rollback while streaming provider inputs and persistence deltas; see the [Rust API changes](https://github.com/cognica-io/uqa-engine/blob/v0.4.9/docs/manual/reference/10-upgrading.md).
- Keep committed sequence allocation outside transactions that change sequence names, ownership or privileges, preventing duplicate values, allocation conflicts and rollback to values already issued by another session. Preserve transactional allocation for newly created or restarted sequence generations.
- Match PostgreSQL foreign-key behavior across ordinary inheritance and partitioned tables: restrict referential actions to the declaring relation, propagate and validate partition constraints, preserve derived constraint identities and deferrability, and reject dropping, truncating or detaching referenced partitions when required.
- Validate table rewrites against the affected rows and validated constraints, including referencing foreign keys. Check unique keys across the complete rewritten result so values moved by `USING` do not conflict with rows being replaced, and preserve PostgreSQL index-build diagnostics.
- Treat a repeated SQL `BEGIN` as a warning without opening another transaction frame; the first `COMMIT` commits the block. Report PostgreSQL warnings for `SET LOCAL` and `SET TRANSACTION` outside transaction blocks while preserving explicit Rust nested frames.
- Keep error details and hints separate from primary messages, preserve referenced-side diagnostics for deferred foreign-key failures, and follow PostgreSQL partition-attachment validation order.

## [0.4.8] - 2026-10-03

This release improves embedded reads and writes and corrects PostgreSQL 18 identity, input and diagnostic behavior. SQLite databases advance to record format 55, and cooperating processes use new row-lock and sequence sidecars. Stop all database owners, retain a closed pre-upgrade backup and update every owner together; see the [0.4.8 upgrade guide](docs/manual/reference/10-upgrading.md).

### Added

- Answer eligible relational queries from B-tree key and included-column postings without fetching documents, controlled by `enable_indexonlyscan`. Support `INCLUDE` on PRIMARY KEY and UNIQUE constraints with PostgreSQL column validation, naming, catalog output and rename/drop dependencies.
- Honor identity sequence declarations and ALTER COLUMN identity actions, including sequence options, generated mode, restart and removal. Adding serial or identity columns creates the sequence and populates existing rows.

### Changed

- Reduce embedded query work with projected native reads, retained decoded scalar columns, shared graph scans, bounded top-K and window storage, HNSW visited bitmaps and generation-checked exact-vector reuse. Reduce repeated snapshot, catalog, index, trigger and commit preparation during writes while retaining transaction visibility and resource ownership.
- Advance SQLite main records from format 54 to 55 with atomic, validated migration of version metadata; native SQLite mapping 13, redb main records 53 and SQL catalog 49 remain unchanged. Older SQLite binaries reject the new record format.
- Coordinate row claims through `.uqa-row-claims` and sequence positions through `.uqa-sequences` beside the database. Update all cooperating processes together; orderly close retains exact sequence continuation, while an unclean final close may skip reserved values.
- Coalesce statistics maintenance counts between durable refresh thresholds. Stored change counts can be a lower bound between publications; the existing stale-age limit still schedules refreshes. SQLite commit connections use a temporary page-cache allowance bounded at 256 MiB and release it after commit.

### Fixed

- Match PostgreSQL 18 identity writes for `OVERRIDING SYSTEM VALUE`, `OVERRIDING USER VALUE`, explicit NULL, DEFAULT, COPY and GENERATED ALWAYS diagnostics. Reject non-DEFAULT writes to generated columns before row execution, including empty sources.
- Preserve distinct rows when a serial or identity column is not the primary key, and handle negative or large integer keys without colliding with generated document identities. Repair legacy key mappings on open and evaluate `_doc_id` and `_meta.doc_id` filters against the correct row identity.
- Keep the statement's row-trigger definitions stable across trigger creation or deletion inside a trigger, while applying the replication role at each firing.
- Match PostgreSQL integer text prefixes, underscores and range errors; parse BYTEA assignment and COPY input through the binary input rules and preserve PostgreSQL hex, escape and base64 behavior in `encode` and `decode`.
- Match PostgreSQL NOT NULL, CHECK and partition violation messages, constraint ordering and permission-filtered row details. Keep their row details and COPY diagnostic hints separate from the primary message.
- Preserve private row/count/index visibility and rollback state while reusing committed caches; reject stale automatic-statistics publication and retain committed notification outcomes when background delivery fails.
- Avoid repeated operand-tree type inference during non-integer scalar arithmetic and comparisons while preserving exact NUMERIC results, REAL precision, integer overflow checks and SQL NULL behavior.
- Avoid full native document scans for maximum-ID lookups, repeated catalog restoration for unchanged committed/private command views, per-key SQLite metadata statements and unchanged maintenance metadata reads. Preserve pinned visibility, savepoint undo, serializable observations, resource limits and existing maintenance deadlines.
- Restore named analyzer bindings on document tables without requiring declared SQL columns. Declared SQL tables retain TEXT-column and physical FTS validation, and missing field bindings remain rejected.
- Release notification test resources on the supported Node.js 16 runtime, including assertion failures and cancellation, so the HTTP compatibility suite completes without requiring newer test-context hooks.

## [0.4.7] - 2026-09-29

### Fixed

- Preserve default document-API full-text field registrations during catalog migration and reopen, including registered fields with non-text column types. Existing string-only indexing and stored values remain unchanged; explicit analyzer assignments and SQL GIN definitions retain their column validation.
- Migrate legacy Python graph-name aliases into the native graph catalog without reclassifying catalog entities as standalone graphs, and accept historical FTS accelerator column declarations while preserving canonical binary keys and rejecting incompatible stored values. Failed conversions retain the complete source state.

## [0.4.6] - 2026-09-28

### Fixed

- Preserve PostgreSQL 18 overload selection when procedures compete with functions: rank all visible signatures before reporting a selected procedure as `42809`, including scalar and table calls, named-argument diagnostics and the `CALL` hint.
- Restore the existing Nori allocation limits across analysis, phrase matching and persistent indexing by compacting shared memory allowances, keeping candidate-scoring statistics inline and reusing retained redb snapshot identities; preserve parent budgets, score calculations and retained-resource lifetimes.

## [0.4.5] - 2026-09-28

### Added

- Add native DiskANN vector indexes for memory, native SQLite (including encrypted and compressed databases), SQLite Key/Value and redb. Bounded graph navigation and product quantization select candidates while complete-tensor reranking preserves canonical cosine scores and the existing probability-conversion contract. Indexes participate in transactions, retained snapshots, rollback and reopen; TEXT/JSON EXPLAIN and EXPLAIN ANALYZE expose selected generations, estimates and actual query work. Rust, Python, Node.js and Browser WASM examples cover configuration, typed parameters, scores and persistence. See the [SQL and score contract](docs/manual/sql/02-ddl.md#diskann-vector-indexes).
- Add independently owned SQL notification subscriptions in Rust, Python, Node.js and Browser WASM, retaining the original database and selected role with bounded queues and explicit cleanup. Add authenticated HTTP/SSE clients with exact sequences, cancellation, typed failures and visible resynchronization across reconnects; stateless SQL hosts reject LISTEN/UNLISTEN without changing transactional NOTIFY.
- Resolve catalog owner names with PostgreSQL-compatible `pg_get_userbyid(oid)`, including NULL, missing OIDs, role rename/deletion and its stable `name` return type.

### Changed

- Advance SQLite main record format to 54, native SQLite mapping to 13 and redb main record format to 53; catalog format remains 49. Atomic upgrades preserve data, history, receipts and identifiers while excluding incompatible readers and writers. Update database-owning processes together and retain a closed pre-upgrade backup. See the [0.4.5 upgrade guide](docs/manual/reference/10-upgrading.md#045-vector-indexes-and-storage-formats).

### Fixed

- Include the vector benchmark fixtures, manifests and source attribution in the Python source distribution so its included Rust benchmarks have all required inputs.
- Release owned notification listener leases and join final recovery without waiting for another SQLite registry writer. Closed handles no longer retain their original provider; independent listeners preserve their delivery and recovery boundaries.
- Match PostgreSQL 18's `42P17` diagnostic when a generated expression calls a non-immutable function.

## [0.4.0] - 2026-09-25

This release delivers shared MVCC for overlapping SQL writers, PostgreSQL comparison/catalog/assignment corrections, retained-resource fixes, and committed notification recovery. It includes breaking low-level Rust API changes and one-way persistent-format upgrades. Update packages and database-owning processes together; retain a closed pre-upgrade backup and follow the [0.4.0 upgrade guide](docs/manual/reference/10-upgrading.md).

### Added

- Support overlapping logical write transactions through Engine SQL on native SQLite, SQLite Key/Value and redb. Independent sessions can commit unrelated writes while another write transaction remains open; shared MVCC merges evaluated document, full-text, vector, graph and catalog changes before short atomic provider publication. READ COMMITTED refreshes each command, while REPEATABLE READ and SERIALIZABLE retain their transaction views and conflict tracking. Rust, Python, Node.js, browser WASM and PostgreSQL TCP APIs use the same transaction contract.
- Add managed commit-receipt ownership, explicit terminal acknowledgement and bounded reclamation. Preserve live owners, unresolved outcomes and serializable dependencies; the default database-wide limit is 65,536 entries and exhaustion reports SQLSTATE `53400`. See the [receipt retention contract](docs/manual/reference/10-upgrading.md#040-mvcc-writer-compatibility).
- Add explicit restoration of closed SQLite backups through `DatabaseRestore` and `ManagedConnection::open_restored`, including encrypted and compressed variants. Restoration preserves stored data addresses and allocation watermarks while replacing transaction-history identity; interrupted restoration resumes only the original persisted request. See the [backup restoration contract](docs/manual/reference/10-upgrading.md#040-sqlite-backup-restoration).

### Changed

- Encrypt only the populated prefix of temporary spill blocks and authenticate its length and block position, avoiding full-block cryptography for short records while preserving bounded workspace and interrupted-write recovery. Coalesce encrypted DISTINCT and hash-join bucket reads through one bounded probe buffer and publish adjacent fields through Storage's complete vectored writes, also shared by indexed spill records. Exact key comparison, malformed-record rejection and partial-append rollback remain covered by owner tests.
- Run Linux and macOS workspace tests from one reusable build per platform across eight shards, with individual results and a five-minute failing timeout. Split independent catalog, callback, container, role deletion, publication, vector and file-mode scenarios into named cases, omit unused container setup tables, and remove busy polling from lock-observation helpers without reducing assertions or provider coverage. Repeated catalog reads retain the same session across both isolation levels for each provider. Optimize Argon2's dependency code in the test profile while retaining its full encryption parameters and workspace debug checks. Recapture the 60-case prepared-plan oracle with 1,000 rows instead of 10,000 after PostgreSQL confirms identical plan choices and counters; expose its four TCP transcripts independently and include the active SQL in read failures.

- Advance SQLite main record format to 48, redb main record format to 47 and native SQLite mapping format to 9; catalog format remains 49. Atomic upgrades preserve records, identities, histories and identifier allocations, while preserving existing receipt capacity, acknowledgement and ownership; predecessors without ownership retain their receipts as manually owned outcomes. Reopened and retained incompatible readers and writers are rejected. Restore a pre-upgrade backup to return to an earlier format. See the [writer compatibility contract](docs/manual/reference/10-upgrading.md#040-mvcc-writer-compatibility).

- Keep produced and retained query values under their original memory and cancellation allowance through defaults, generated expressions, row/page construction and callback handoff. Selected analyzer/catalog generations, text-index nodes and vector collections retain their owners until the final reader releases them; failed production preserves previously admitted results and the original error.
- Share immutable memory document/text-index snapshots and retain versioned provider views without copying the entire corpus. Controlled identity and field readers keep bounded pages alive through consumers; custom providers must forward the [controlled read and compound operation contracts](docs/manual/reference/10-upgrading.md#040-rust-keyvalue-compound-operations).
- Return `Budgeted<RetainedAnalyzedField>` from `analyze_index_field_budgeted`, with Core-owned term nodes. Ordinary `AnalyzedField` keeps its existing carrier. See the [Rust source upgrade](docs/manual/reference/10-upgrading.md#040-retained-text-analysis-owners).
- Retain durable namespace OIDs and incarnations through ACL/owner changes, undo and reopen, while recreation gets a new identity. Coordinate schema creation names, lifetime binding, catalog tuple changes and relation-creation dependencies across native SQLite, SQLite Key/Value and redb; preserve PostgreSQL committed-update errors and rollback outcomes, including unchanged ACL commands. Schema security format 2 and common record format 31 fence incompatible predecessors.
- Preserve private schema creation, deletion, ACLs and ownership when direct catalog reads refresh a fixed transaction snapshot after a peer commit. Namespace authority overlays exact private records, including deletions, while unrelated committed schemas stay current.
- Persist ordinary relation and column ACL tuples independently across native SQLite, SQLite Key/Value and redb. GRANT/REVOKE now coordinate catalog tuple updates, preserve independent column commits and combine private ACL changes with fresh authority under fixed data snapshots. Record format 30 rejects writers that cannot read these tuples; definition lifecycle operations compact, move and remove the records atomically.
- Routed bound native SQLite occurrence indexes through common logical sessions, including retained cursors, source rebuilds and scorer-versioned block maxima. Mapping format 4 normalizes legacy skip/block-max tables; source and column changes invalidate derived accelerators and reject late competing builds.
- Routed bound native SQLite exact, IVF and HNSW vector APIs through common logical sessions. Canonical tensors and index metadata publish atomically, retained snapshots survive rollback and lifecycle changes, and HNSW caches distinguish committed and private graph generations. Existing IVF assignments and HNSW topology reopen without rebuilding.
- Routed native SQLite catalog cache generations through retained committed/private snapshots, including savepoint restoration, independent writers and bounded changed-key projections.
- Routed bound native SQLite graph catalog mutations, snapshot replacement/removal and path-index definitions/pairs through common logical transactions and shared cache validation. Mapping format 3 atomically adds versioned graph-to-path ownership while preserving predecessor histories and receipts.
- Added versioned native SQLite graph lookup entries for label, adjacency and membership reads, and routed native catalog graph reads and named-graph hydration through one retained logical snapshot. The development native adapter atomically upgrades mapping format 1 to 2 while preserving source history, original commit boundaries and receipts.
- Routed redb Key/Value, catalog and backend sessions through common logical transactions with pinned reads, private savepoints, bounded retention and durable commit receipts. Independent direct Key/Value writers can commit concurrently.
- Added an atomic, one-way redb record-format upgrade that rejects released 0.3.6 writers after migration. The default private session allowance is 64 MiB and can be configured with `RedbStorage::open_with_options`. See the [0.4.0 upgrade contract](docs/manual/reference/10-upgrading.md#040-redb-record-format).
- Routed SQLite Key/Value sessions and their connection clones through the same logical transactions, including SQLCipher and compressed variants. Independent direct writers retain private changes without holding SQLite's physical writer. Legacy Key/Value files migrate atomically and reject released 0.3.6 writers. See the [SQLite upgrade contract](docs/manual/reference/10-upgrading.md#040-sqlite-keyvalue-record-format).

### Fixed

- Validate index expressions, predicates, options and declarations before an existing relation name can skip `CREATE INDEX IF NOT EXISTS`. Valid skips preserve the original index without scanning rows or rebuilding physical vector state; invalid declarations retain PostgreSQL diagnostics and transaction rollback.
- Make added text columns visible before constraint-name refresh and validate newly backfilled UNIQUE keys atomically. SQLite, SQLite Key/Value and redb preserve defaults through reopen; duplicate-producing additions return PostgreSQL 23505 and DETAIL, and failed statements or savepoint rollback leave no column, analyzer or index residue.
- Keep compressed SQLite chunk reads bound to the opened physical file when another writer compacts and replaces the pathname. Initial unlocked header reads retain their original data, and the next shared-lock refresh observes the new committed generation, with unchanged encryption authentication and storage formats.
- Recover committed NOTIFY payloads and LISTEN/UNLISTEN changes when a sender disappears before the notification registry finishes. Preserve original sender identity, commit order, read-only behavior and unresolved commit resources; surviving listeners recover without callback replay or duplicate queue publication. Indeterminate commits release the registry writer while preserving their original publication reservation, allowing independent recovery and listener progress; resumption honors session cancellation without losing the original effects. Registry schema 2 fences older notification writers. SQLite Key/Value now forwards its encryption credential to auxiliary notification files.
- Preserve decimal and typed literal identity in grouped expressions, including numeric scale, type modifiers, parameter slots and catalog-resolved domain/OID casts. Validate ungrouped inputs before constant folding or empty-input execution, bind equivalent casts to the same group value, preserve computed HAVING keys and missing grouping-set NULLs, and retain positional ordering with duplicate output labels. Ordinary/prepared queries and stored-view rename/reopen use the same SQL analysis.
- Keep unbound SQLite inverted-index snapshots fixed after source replacement, deletion, rollback and closure. Capture occurrence rows, legacy-format presence, analyzer revisions and field-specific skip/block-max data through one physical read boundary under the original retention allowance; nested readers preserve that captured data and release it with the final owner.
- Remove duplicate memory-index position buffers and reverse-term nodes, share document metadata and field counters with their existing owners, and move staged posting nodes during batch publication. Preserve complete token graphs, atomic mutation, retained snapshots and exact memory admission while restoring the existing Nori allocation contract.
- Release local MVCC participant and receipt registry capacity when the final owner drops, including failed lease admission and redb commit/rollback with retained readers. Completed transactions no longer retain an empty 48-byte lease buffer, and local payloads share the participant allocation.
- Reuse the Key/Value reader's analysis scratch allowance, preserve it through token projection, and move staged term keys after borrowed reverse-vocabulary encoding. Keep document metadata, postings and counters atomic without duplicate term buffers.
- Reuse unique private-record tree nodes within atomic MVCC batches. Core reserves each complete insertion before mutation and copies only paths still shared by a snapshot or savepoint, preserving failure atomicity and the original retained allowance.
- Preserve array element and slice assignment targets across INSERT, UPDATE, ON CONFLICT, MERGE, updatable views and stored routines/rules. PostgreSQL bounds, repeated partial writes, domains, original-row expression evaluation, generated values, constraint failures, renames and durable reopen share the same bounded mutation path. Reject partial assignments to untyped callback-view columns before RHS or trigger effects instead of treating them as whole-column writes.
- Report PostgreSQL unique-key violation DETAIL using catalog expression syntax, default B-tree input type output and the invoking role's table/column SELECT permissions. Array access in stored index definitions retains subscript and slice syntax instead of internal function labels.

- Preserve PostgreSQL `int2vector` and `oidvector` identity, array-type OIDs, bounds and atomic outer-array elements through base/domain casts, UNNEST, compatible-array functions and storage. Ordered aggregates, DISTINCT, extrema and B-tree keys propagate comparison errors; UPDATE retains index entries only when all indexed inputs keep their stored representations, including expression, predicate and included-column dependencies. Existing-row schema validation still checks other rows, including during partition attachment. Initial restoration normalizes predecessor carriers and validates rebuilt indexes atomically without repeating domain checks; failed validation rolls back the conversion. Fixes [#123](https://github.com/cognica-io/uqa-engine/issues/123).
- Match PostgreSQL's `0A000`, primary message and separate DETAIL when a virtual generated column calls a user-defined function, including CREATE and ALTER paths; preserve the fields through the PostgreSQL protocol.
- Compile restored routines against the already captured catalog and namespace view, preventing recursive catalog refresh from waiting on its own registry lock during rollback.
- Retain empty graph snapshots without constructing a temporary database. Necessary detached graph snapshots use fresh 256-bit SQLCipher raw keys without repeating password derivation.
- Retain exclusive relation locks while routine and sequence cascades remove dependent columns, defaults and checks, preventing concurrent statistics publication from invalidating their private removals. Advance the storage write view after dependent lock waits.
- Count SQL-equal values together in `mode()` and retain the first group in the requested order when frequencies tie. Signed zero, equivalent intervals, JSONB spellings and NaN use Core equality in both memory and spilled execution. Fixes [#145](https://github.com/cognica-io/uqa-engine/issues/145).
- Order JSONB zero between negative and positive numbers, including fractions, exponents and nested values. Comparisons and ordered index keys follow independently captured PostgreSQL results; join hashes use semantic JSONB equality while persisted equality keys remain stable. Fixes [#122](https://github.com/cognica-io/uqa-engine/issues/122).
- Preserve PostgreSQL TIME day endpoints and TIMETZ timezone tie-breaking in comparisons, equality keys, grouping and indexes. Independently captured PostgreSQL results replace the former incorrect equality expectations; predecessor reservation aliases remain available during upgrades. Fixes [#121](https://github.com/cognica-io/uqa-engine/issues/121).
- Resolve `GROUP BY` input columns before output aliases, including grouping sets, prepared queries and stored views. Retain PostgreSQL ambiguity and aggregate/window context errors. Fixes [#139](https://github.com/cognica-io/uqa-engine/issues/139).
- Preserve NaN and both infinities as typed floating-point values through JSON-backed document/index storage and reopen, including nested values. Older binaries are rejected before accessing the new format. Existing NULL records cannot recover float information already lost. Fixes [#138](https://github.com/cognica-io/uqa-engine/issues/138).
- Restore transitive Core equality and ordering across integers, binary floats and exact decimals, and align canonical and join hash keys, DECIMAL index boundaries and aggregate extrema with that ordering. SQL comparisons retain PostgreSQL's selected operand conversions, including rounding at the integer/float precision boundary. Preserve predecessor numeric key reservations when processes share a database. Fixes [#120](https://github.com/cognica-io/uqa-engine/issues/120).
- Resolve bound `pg_catalog.pg_sequence_parameters`, `pg_sequence_last_value` and `pg_get_sequence_data` calls through the existing catalog handlers. Preserve `oid`/`regclass` signatures, scalar/table record results, user-function shadowing and privilege revocation/regrant behavior; classify sequence parameters as stable and physical sequence values as volatile. Fixes [#126](https://github.com/cognica-io/uqa-engine/issues/126).
- Let automatic statistics yield when a serialized custom backend's writer is held, releasing maintenance relation locks and retaining durable pending work. Internal catalog readers no longer prolong background maintenance ownership.
- Preserve transactional statistics through commit, rollback and savepoints, including read-only ANALYZE. Independent maintenance counters merge without replacing unrelated committed changes.
- Preserve selected user routines and registered callbacks when their names match catalog functions. Execute bound catalog calls by their retained identity and preserve binding errors before catalog interception.
- Preserve numeric operator identity, declared operand widths, PostgreSQL errors and output labels through stored definitions and execution. Resolve ordinary numeric functions with the shared search-path-aware signature registry. Common record format 40 fences incompatible stored-expression writers.
- Retain a separate durable incarnation and public OID for each domain CHECK and NOT NULL constraint. Reserve addresses against table, foreign-table and trigger constraints; finalize legacy conversion against the complete catalog and reject corrupt current identities. Domain format 3 and common record format 39 fence incompatible writers.
- Reserve domain and relation row-type names across concurrent creation and rename, preserving PostgreSQL duplicate diagnostics, transaction/savepoint undo and independent sequence/index names.
- Keep automatic constraint and index names within PostgreSQL’s 63-byte identifier limit, including collision suffixes and UTF-8 boundaries. Preserve quoted case, spaces, punctuation and repeated key labels when assigning unnamed indexes.
- Choose automatic constraint names from the complete schema namespace across ordinary/foreign tables, domains and constraint triggers while keeping explicit duplicate checks local to their owner. Carry parent-selected names through recursive CHECK and NOT NULL additions. Preserve every column CHECK declaration, including its name, enforcement and inheritance attributes, during CREATE TABLE and ADD COLUMN.
- Preserve constraint ownership when cascading domain or schema deletion through indexed domain columns. Remove column-owned key indexes and foreign keys through the column lifecycle while retaining direct expression and predicate dependencies, savepoint undo and reopen behavior.
- Persist independent domain definitions and OID claims so concurrent transactions creating or dropping different domains can commit without overwriting one shared registry. Preserve private changes and peer commits through catalog refresh, savepoint undo and reopen; convert earlier domain catalogs atomically and fence incompatible development writers.
- Bind legacy unqualified foreign-key targets before converting their stored index identities. Resolve against the complete stored table catalog, reject ambiguous or dangling targets before conversion writes, and keep the canonical target after later same-name table creation.
- Coordinate CHECK, NOT NULL, foreign-key, key and constraint-trigger names within their owning table across SQLite and redb. Preserve concurrent index renames through constraint and relation-name waits, distinguish pre-existing duplicates from conflicting commits, and avoid trigger constraint names when assigning automatic column and key names. Keep explicit PRIMARY KEY and UNIQUE declarations, including names and NULLS NOT DISTINCT, when adding a column.
- Coordinate concurrent relation creation and rename through schema-local name reservations across SQLite and redb. A competitor waits for commit or rollback and reports PostgreSQL catalog uniqueness SQLSTATE `23505` after a conflicting commit, including for conditional creation and implicit indexes; fixed data snapshots remain unchanged.
- Keep internal fixed snapshots, current-catalog readers and retrieval rechecks from registering automatic-statistics clients. Public sibling sessions retain their own maintenance client lease; internal readers no longer restart background work after application clients release it.

- Preserve foreign-key catalog identities through column, table and constraint renames while keeping partition copies independent. Retain deferred modes and queued checks across local renames, and validate all converted rows before initial restoration writes. Common record format 33 excludes writers that cannot preserve those identities.

- Preserve NOT NULL constraint OIDs through table, column and constraint renames, allocate a new identity after removal/recreation, and share inherited rename locking and diagnostics with CHECK constraints. Initial catalog conversion preserves predecessor OIDs and rolls back with restoration failures; common record format 32 excludes writers that cannot retain the identity.

- Retain AccessExclusive on foreign-key references and every removed partition clone or CASCADE referrer. Follow original relation and constraint identities through waits and name reuse, preserve refreshed metadata, and rebuild DROP INDEX dependencies after its parent lock wait. Savepoint rollback restores the removed constraints and releases their locks.

- Remove inherited NOT NULL constraints through the same origin-aware traversal as CHECK constraints, preserve local and multiply inherited definitions, and make retained ONLY children local. Retain original child identities and per-level locks through waits, restore removals at savepoints, and reject removal beneath primary keys and identity columns with PostgreSQL diagnostics.

- Validate inheritable NOT NULL constraints on descendants before marking the parent valid, match child constraints by column, and reject ONLY validation while children require validation. Retain foreign-key reference RowShare locks and inheritance descendant AccessShare locks, preserving original identities through waits and releasing acquisitions at savepoint rollback.

- Retain RowExclusive sequence locks through the outer transaction for nextval, currval, lastval and setval, including cached values and savepoint rollback. Recheck definitions and authority after waits while preserving the original object identity and ordinary query snapshot. Direct calls release implicit locks, and value execution reuses an active query transaction without reentering its statement gate.

- Select ALTER TABLE locks by action and retain secondary targets for foreign-key addition, inheritance changes, inheritable additions, CHECK validation/rename and partition attachment. Foreign-key addition and trigger mode changes allow readers; CHECK validation allows writers. Recheck explicit secondary names after waits, retain inherited child identities through rename, and enforce parent ownership for INHERIT.

- Rebind ordinary ALTER TABLE targets and current authority after definition-lock waits, including name removal, search-path fallback and relation-kind replacement. Table renames require source-schema CREATE or current database TEMP. Historical table owner syntax now reaches regular and materialized views, and missing table/schema errors preserve PostgreSQL SQLSTATEs.

- Apply actual-owner, system-catalog and source-schema CREATE checks before view, materialized-view and foreign-table ALTER kind errors. Recheck authority after relation waits, preserve temporary-view TEMP requirements, and defer foreign-table write admission until definition binding succeeds.

- Check the actual relation owner before ALTER SEQUENCE reports the requested kind, and require source-schema CREATE for sequence renames before and after lock waits. Temporary sequence renames honor current database TEMP authority; pinned system catalogs retain their protection.

- Coordinate sequence definition, persistence, name and DROP operations with PostgreSQL relation lock modes and retained destination namespaces. Recheck replaced names, relation kinds, owner/CREATE authority and destination collisions after waits; retain locks for unchanged schema moves and preserve rollback/savepoint behavior.

- Expose live SQL and PL/pgSQL cursor declarations in `pg_catalog.pg_cursors`, preserving original SQL, statement start time and declared options through FETCH, hold materialization and transaction cleanup. Keep metadata visible while its executor is detached, isolate sessions, and skip occupied names when allocating unnamed cursors.

- Select default cursor scrollability from native backward-scan support, including outer ordering over windows, target sets and `UNION ALL`, reused window ordering, and deferred versus materialized CTEs. For example, `SELECT 1` and an inlined constant CTE default to forward-only; explicit `SCROLL` retains the existing materialization behavior. Expand streaming target sets at the correct side of sorting and apply output slicing after expansion, including `WITH TIES`.

- Compact eligible current SQLite MVCC records into bounded lossless runs while preserving exact keys, values, revisions, snapshots and conditional-write conflicts. Split and restore individual predecessors atomically on later writes. Development record format 29 adds guarded run storage; redb retains its existing physical layout.
- Reclaim obsolete MVCC history while retaining live snapshots across SQLite sessions and native processes, and across redb adapters. Preserve deletion conflicts and every commit receipt; compact SQLite head tombstones without duplicating their keys in history, and restore deleted revisions before later replacements. Share native file coordination primitives below execution. Development record format 28 rejects readers and writers that do not register snapshot leases.

- Preserve PostgreSQL target order during multi-role deletion: wait and remove memberships before resolving later targets, check initial CREATEROLE once, and perform dependency checks and tuple deletion in target order. Retain statement/savepoint atomicity and original identities. Reuse transitive ADMIN authority for role alteration, rename and deletion independently of INHERIT/SET options.

- Reject special role specifiers in DROP ROLE after checking CREATEROLE authority, matching PostgreSQL 18 error and notice order. Preserve quoted uppercase role names and reject invalid multi-target deletions without publishing partial removals.

- Prevent SECURITY DEFINER code from dropping its caller’s selected role. Keep effective, outer-selected and session identity protection distinct, with PostgreSQL 18 error precedence and session-user diagnostics.

- Implement SQL role renaming with retained OIDs, incarnations, memberships, ownership and ACLs. Preserve selected sessions and SECURITY DEFINER authority through name reuse, including bootstrap-role renaming. Bind ACL recipients and new owners before waits so peer renames cannot retarget publication. Coordinate tuple and destination-name conflicts with transaction/savepoint undo; development record format 27 excludes incompatible writers.

- Serialize competing role attribute changes and deletion against the originally selected definition tuple, including identical attribute assignments. Match PostgreSQL 18 commit, rollback and savepoint outcomes without blocking independent membership publication. Initial restoration atomically initializes legacy tuple revisions; current missing revisions fail without repair. Role catalog format 3 and development record format 26 exclude preceding writers.
- Retain routine owner, EXECUTE grantee and grantor incarnations through invocation, ownership changes, catalog snapshots, refresh, undo and reopen. SECURITY DEFINER uses the original owner identity, and replacement role names acquire no stored authority. Initial restoration converts legacy authority atomically while preserving both historical owner EXECUTE meanings; malformed current references never rebind. Preserve private routine metadata during committed refresh. Development record format 25 excludes preceding writers.
- Retain domain owner OIDs and incarnations through snapshots, undo and reopen, and prevent DROP ROLE while owned domains remain. Preserve private domain creations and deletions during committed catalog refresh. Initial restoration converts legacy owner names atomically; malformed current references never rebind. Execution owns domain catalog encoding and publication. Development record format 24 excludes preceding writers.
- Wait through physical SQLite record-writer contention without replaying evaluated writes. Autonomous sequence reservations, document identity allocation and logical publication observe execution cancellation; a busy COMMIT retains its staged transaction while waiting. Rollback cleanup and ordinary sibling sessions retain independent cancellation. Custom versioned storage wrappers must forward the new [write cancellation contract](docs/manual/reference/10-upgrading.md#040-storage-write-cancellation).
- Retain table, view, materialized-view and foreign-table owner/ACL role incarnations through catalog snapshots, refresh, undo and reopen. Initial restoration converts legacy names atomically; malformed current identities never rebind to replacement roles. Include column ACLs in table refresh fingerprints so unrelated catalog publication preserves private grants. Development record format 22 excludes prior writers.
- Read sequence definitions, ACLs, roles and memberships from one retained provider snapshot for value functions and introspection, preserving private and temporary state. Concurrent grants and membership revocations no longer mix new sequence metadata with stale authorization. Direct Rust value calls refresh command metadata outside active SQL callbacks. Ordinary transaction data snapshots remain fixed under REPEATABLE READ and SERIALIZABLE.
- Preserve private sequence creation, alteration, ownership, ACLs, rename and deletion when unrelated commits refresh a fixed transaction's catalog. Nested host queries can still resolve sequences created by their own transaction, and temporary entries stay local to their session.
- Keep public Rust sequence inspection from replacing live sequence metadata without its matching role catalog. Snapshot, name-list and state reads retain committed values, private definitions and temporary entries while preserving ordinary transaction data visibility and retained query catalogs.
- Retain sequence owners, ACL grantees and grantors by role OID and incarnation through creation, GRANT/REVOKE, owner transfer, snapshots and dependency checks. Initial open validates the complete sequence catalog before converting legacy names; failed later restoration rolls back conversion, while refresh and secondary sessions reject unconverted or corrupt authority. SQLite and Key/Value codecs distinguish identities from legacy names without fallback. Common record format 23 fences preceding sequence writers; native mapping 8 and catalog version 49 are unchanged.
- Keep live sequence definition and dependency refresh consistent with its role and membership catalog after peer commits. Reuse execution's complete snapshot read before installing state, preserving private role grants, temporary ownership, savepoint undo and autonomous sequence values.
- Keep sequence privilege inquiry on coherent role, membership and sequence ACL snapshots, retaining explicit role identities through name resolution. Validate privileges before target lookup and honor PUBLIC grants for `public` and absent role OIDs, matching PostgreSQL 18 error precedence.
- Honor owner self-revocation of ordinary table, column and sequence privileges while retaining implicit grant options and ownership authority. Sequence parameter inspection and materialized-view refresh require their ordinary privileges even for the owner; denied sequence operations preserve cached reservations.
- Retain explicit role identities in table and column privilege inquiries, apply PUBLIC grants to the `public` subject and absent role OIDs, and reject invalid privilege strings before missing-OID or invalid-attribute `NULL` results while preserving named-object error precedence. Sequence targets share one detached authority snapshot for OID binding and all requested privileges, including current role memberships and private or temporary ACLs. Explicit subjects see newly committed roles without replacing the transaction's ordinary data snapshot.
- Preserve quoted role names in object grants, grantor paths and owner transfers. A role named `"PUBLIC"` has independent privileges from the PUBLIC recipient, and quoted session-keyword names remain literal through rollback, refresh and reopen. Typed ACL and statement encodings retain legacy meanings; development record format 21 excludes incompatible writers. See the [0.4.0 role catalog upgrade notes](docs/manual/reference/10-upgrading.md#040-role-catalog-records).
- Merge independent native SQLite HNSW document writers through the common vector journal and resolver. Preserve serial node allocation, topology and compaction, retained snapshots, savepoint undo and atomic publication retries. Share IVF/HNSW native codecs, staging and document/lifecycle guards. Native mapping format 7 upgrades formats 1–6 atomically, retaining existing row encodings and family 49 guard histories; older writers reject the new marker.

- Merge independent SQLite Key/Value and redb HNSW document writers using the same input journal, conflict validation and receipt-safe resolution as IVF. Preserve exact serial graph topology through node allocation and compaction, retain document/lifecycle conflicts and reject incomplete persisted tensors. Bound graph reconstruction and JSON buffers; upgrade development MVCC formats 1/2/3 atomically to format 4 without rewriting histories or receipts.
- Bound native SQLite and Key/Value HNSW candidate preparation and explicit reconstruction under the caller’s memory and cancellation allowance. Preserve serial graph topology, node allocation, compaction and incremental deltas while keeping failed candidates separate from the source. Reject incomplete canonical tensor ordinals during reconstruction.
- Merge independent native SQLite, SQLite Key/Value and redb writers sharing one IVF index, preserving ordered training changes, tensors, snapshots and savepoint undo. Retain conflicts for overlapping documents and index lifecycle changes, reject mismatched canonical inputs, and resolve publication retries with the original receipt identity. Development MVCC record format 4 upgrades formats 1/2/3, and native mapping format 6 adds IVF guards with an atomic upgrade from mappings 1–5. Both preserve existing source histories and IVF row encodings.
- Preserve bound native SQLite IVF centroids and deletion counters through ordinary document changes, and retrain at the common storage threshold. Native and Key/Value IVF mutations now share bounded, cancellable candidate preparation. Reject missing or inconsistent native IVF generations instead of silently using exact search; explicit initialization rebuilds from canonical tensors.

- Merge independent native SQLite, SQLite Key/Value and redb occurrence writes sharing posting clusters and field totals. Preserve same-document and structural conflicts, invalidate late accelerator builds, and retain bounded preparation, savepoints and receipt-based retry without replaying analysis or scoring. Development MVCC record format 2 upgrades prior metadata atomically and rejects older writers afterward; native mapping format 5 adds document/structural guards with an atomic predecessor upgrade.
- Validate bound native SQLite catalogs on their retained committed/private view during Engine restoration. Reject inconsistent schema, relation, definition and index references without reading raw physical rows; B-tree validation uses bounded key pages without loading entry payloads.
- Keep bounded common occurrence cursors on their original snapshot across cluster pages, and aggregate cross-field term frequencies on one view. Deterministic interleavings verify that later replacements cannot mix new frequencies into an older result.
- Preserve common Key/Value occurrence snapshots and compound reads across later writes, rollback and independent commits. Posting data, source metadata and statistics share one read boundary; mutations retain their original write preconditions without replay. SQLite Key/Value and redb snapshots retain MVCC owners without copying the index corpus.
- Preserve common Key/Value vector generations across rollback and independent commits. IVF and HNSW share compound read/evaluation and cache identity handling; exact snapshots retain their original tensors and memory reservation instead of following the live session. All three snapshot kinds reject mutation after their original handles close. Failed operations preserve earlier private writes without replaying evaluation. SQLite Key/Value and redb share this implementation; custom vector providers must implement the [compound operation contract](docs/manual/reference/10-upgrading.md#040-rust-keyvalue-compound-operations).
- Prevent SQLite HNSW searches from reusing discarded graphs after rollback or same-revision index recreation. Cache selection, candidate evaluation and persistence now share one physical view; intervening rebuilds cannot publish an obsolete candidate.
- Preserve embedded NUL characters in metadata-derived cache names and atomically repair the known prior triggers. Native conversion and reopen now reject missing or changed catalog cache tracking.
- Preserve graph path-cache validity across overlapping development SQLite Key/Value and redb transactions: merge source invalidations, include late graph dependencies, reject stale builds and preserve caches reassigned to another graph. Savepoints and receipt-based commit retry retain these effects without repeating graph evaluation.
- Reject duplicate sequence incarnations during native SQLite record conversion and competing live definition generations at commit, including aliases created concurrently by independent sessions. Native sequence catalog and value operations now share the selected logical session.
- Preserve data when the SQLite catalog column-rename API receives the same source and destination name. Move or remove B-tree repair markers with their column so renamed and deleted fields do not leave stale repair requests; preserve an existing destination B-tree without mixing in discarded source postings, including when SQLite foreign-key cascades are disabled.
- Preserve and resolve uncertain logical commits through Engine without replaying SQL preparation or Rust callbacks. Typed storage outcomes retain their transaction identity across later failures; matching receipts complete session publication, and rollback cannot report success for already committed data. `Engine::pending_commit` exposes retained resolution state.
- Roll back a retained storage transaction before refreshing Engine caches after a failed commit. If rollback also fails, preserve the failed Engine frame and locks until storage cleanup succeeds instead of exposing private catalog or graph state as committed.
- Reject persistent catalog/backend pairs from different reported transaction contexts before Engine restoration or sibling attachment, including separate sessions over the same file. Native SQLite, SQLite Key/Value and redb expose the shared affinity contract; custom wrappers must forward it as described in the [Rust upgrade notes](docs/manual/reference/10-upgrading.md#040-rust-session-affinity).

### Security

- Protect spill/conflict/retry temporary data with per-file authenticated encryption or independent ephemeral SQLCipher keys, and unlink files when their last owner releases them. Interrupted block replacement preserves the prior ciphertext. Process death may leave ciphertext without its ephemeral key; automatic orphan scavenging is not included.

## [0.3.8] - 2026-09-20

See the [upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.3.8/docs/manual/reference/10-upgrading.md) for package updates and corrected JSON extraction behavior.

### Fixed

- Preserved JSON and JSONB input types through `->` and `#>` extraction, including comparisons on empty tables, bound parameters and generated columns. Text extraction with `->>` and `#>>` continues to return text, and JSON equality remains rejected.
- Distinguished present JSON null from missing keys and SQL NULL, preserved text object-key versus integer array-index overloads, and decoded path operands as PostgreSQL text arrays, including quoted keys and NULL path elements. SQL rendering retains all four extraction operators and their result types.

## [0.3.7] - 2026-09-18

See the [upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.3.7/docs/manual/reference/10-upgrading.md) for package updates, concurrent writer coordination and encrypted notification sidecar requirements.

### Fixed

- Prevented independent sessions and processes from assigning the same generated physical document identity and silently overwriting successful inserts with distinct TEXT or composite primary keys. Reserved candidates and committed-state rechecks preserve VALUES and INSERT ... SELECT rows through RETURNING, multi-row statements, fixed snapshots, rollback and database reopen.
- Encrypted cross-process notification channels, payloads and registry state with the database credential for encrypted SQLite and compressed-encrypted providers. Pooled registry connections preserve encryption and concurrent initialization; incompatible plaintext or differently keyed sidecars fail without replacing their history.
- Updated the Node.js build tool's `js-yaml` dependency to address unbounded CPU use while processing empty merge sources.

## [0.3.6] - 2026-09-15

See the [upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.3.6/docs/manual/reference/10-upgrading.md) for package updates and operator-tree optimizer compatibility.

### Fixed

- Preserved vector-threshold intersection scores, document support, and invalid-threshold errors by removing the operator-tree threshold merge. Identical and nearby query vectors retain their separate score contributions, including inside nested operators.

### Deprecated

- Deprecated `uqa_planner::TreeOptimizerConfig::enable_merge_vector_thresholds`. The field remains source-compatible but is ignored for either value; vector-threshold predicates always preserve their separate score and validation semantics.

## [0.3.5] - 2026-09-15

See the [upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.3.5/docs/manual/reference/10-upgrading.md) for Japanese analysis, package features and Rust analyzer configuration changes.

### Added

- Added native Lucene 10.5.1 Kuromoji analysis with NORMAL/SEARCH/EXTENDED tokenization, N-best paths, Japanese user dictionaries, six morphology attributes, base-form/POS/stopword/reading/number filters, small-kana conversion, completion and independent normalization. The protected `kuromoji` and `kuromoji_completion` analyzers are available through SQL and all bindings without a JVM or runtime dictionary download.
- Added the independent `uqa-kuromoji-data` package with the complete pinned Japanese dictionary and upstream notices. Rust applications select `nori` and `kuromoji` independently; the CLI and official Python, Node.js and Browser WASM distributions include both languages by default.
- Added dictionary-independent CJK width and Japanese iteration-mark character filters with corrected source offsets, memory limits and cancellation. Immutable Japanese revisions preserve diagnostics, phrase graphs, highlighting and independent index/search bindings through transactions, sibling sessions, backups and reopen.

### Changed

- Shared dictionary codecs, resources, Unicode profiles, bounded lattice traversal, numeric composition and source-coordinate ownership between Nori and Kuromoji in `uqa-analysis`, preserving Korean dictionary bytes and retained descriptor identities.
- Added optional explicit normalization to Rust `Analyzer` values and typed language profile selection to `SimpleLowercaseConfig`. Existing Rust struct literals need the changes described in the upgrade guide; existing generic/Nori serialized configurations and stored revisions remain compatible.

### Fixed

- Updated `rustls` to 0.23.45 and `rustls-webpki` to 0.103.15 for [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html), which corrects TLS 1.3 handshake encryption-level validation.
- Restored declared text argument inference for parameterized analyzer table functions, including `INSERT ... SELECT analysis FROM analyze_text($1, $2)`.
- Verified release-file digests across artifact transfers before registry publication and complete dictionary/notice inventories in native and WASM packages, including split WASM data segments.

## [0.3.0] - 2026-09-14

See the [upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.3.0/docs/manual/reference/10-upgrading.md) for Rust API and SQLite provider changes, feature selection, and persistent analyzer/index migration.

### Added

- Added native Korean Nori analysis with the embedded dictionary, user-rule compilation, decompound modes, morphological attributes, POS and reading filters, simple lowercase normalization, and optional exact-decimal Korean number composition. Rust exposes the optional `nori` feature; official Python, Node.js, and browser WASM packages include the dictionary and upstream notices without a JVM runtime dependency.
- Added lossless token graphs, canonical term keys, original source offsets, and independent normalization lengths across memory, SQLite, and redb indexes. Quoted full-text phrases follow connected graph paths; analyzer-aware highlighting renders matches at original source spans. SQLite schema 48 and legacy key-value positional indexes rebuild from original documents under restored analyzer revisions in the initial catalog transaction.
- Added rich token inspection and normalization across SQL and language bindings, with explicit resource limits and cancellation through analysis, graph matching, highlighting, and result materialization.
- Persisted exact named analyzer descriptors and independent index/search bindings, including resolved synonym files, owner validation, transactional source migration, and restoration across sessions and reopen.

- Added a versioned pre-commit hook that validates staged crate dependencies and transitive ownership boundaries, with the same policy enforced in CI.

- Added cost-based custom/generic prepared-plan selection and `plan_cache_mode`, with typed parameter specialization, five initial custom plans, planning-cost-aware reuse, and per-session usage counters.
- Added ordered prepared-parameter inference, preparation-time schema and expression validation, fixed result-descriptor checks during replanning, and session-local `pg_prepared_statements` metadata retaining the original SQL text.
- Added an unpublished PostgreSQL TCP server crate with independent authenticated-role sessions, explicit trust policy, Simple Query results, cancellation, notifications, and protocol 3.0/3.2 negotiation.
- Added data-modifying CTEs for INSERT, UPDATE, DELETE, and MERGE, with typed RETURNING results, statement snapshot sharing, and execution of unreferenced commands. PostgreSQL 18 differential fixtures cover command results, state changes, and diagnostics on memory and SQLite engines.
- Added `Engine::sql_simple_query` for ordered per-statement results and `SQLResult::command_tag` for PostgreSQL command completion. Complete-message parsing, implicit transaction segments, deferred commit errors, and callback failures preserve the transaction's actual outcome.
- Added domain declarations with defaults, named CHECK and NOT NULL constraints, nested domains, domain arrays, catalog identities, transactional rollback, and SQLite persistence. PostgreSQL differential cases verify conversion errors, assignment versus explicit-cast behavior, preservation of already typed values, and constraint-function effects.

### Fixed

- Preserved copy-on-write memory index snapshots and durable analyzer/occurrence state through transaction rollback, savepoints, concurrent sessions, backups, and reopen. Analysis and retrieval failures release their retained allowance and publish no partial index replacement.
- Preserved both analyzer revisions before rewriting documents during a column rename, preventing existing rows from being reindexed with the table default. Dropping the last explicit GIN analyzer owner now rebuilds with the default when another GIN retains the field.

- Moved Cypher default-label requirement analysis and diagnostics from Engine into graph, preserving catalog reads, vertex-before-edge errors, and the existing graph transaction boundaries.
- Move pure scored-input and persisted-relation-reference tests to their implementation crates, consolidate Engine state tests under one unit-test tree, and move public model-training scenarios to the existing integration harness; remove obsolete test-only source directories and the event type reexport module.
- Removed the planner dependency on the graph runtime by moving the shared RPQ AST, parser, and seven original parser tests into Core. Graph retains the same public syntax exports, and the commit hook forbids reintroducing the dependency.
- Moved creation-namespace selection, schema creation privileges, and index-target visibility out of Engine. Table, CTAS, view, sequence, domain, routine, foreign-table, index, and key-constraint consumers share native namespace inputs while preserving live guards, deferred writer retry, and error order.
- Moved table-to-training-data conversion and JSON/table training orchestration into execution, removed whole-training Engine callbacks, and relocated the original pure label and IVF-catalog validation tests to their owning crates. Engine retains generated-column reads and model persistence transactions.
- Implemented `CREATE SCHEMA AUTHORIZATION` with named and session-role owners, omitted schema names, PostgreSQL authorization and duplicate-schema checks, and durable transaction behavior. Schema owner transfer now checks database `CREATE` on the invoking role.
- Moved ordinary-table DROP dependency checks, inheritance/partition targets, and CASCADE scheduling into SQL and execution, retaining table generations, guard lifetimes, original transaction boundaries, and physical publication order.
- Moved sequence value diagnostics into SQL and value resolution, cache consumption, reservation, and publication scheduling into execution, retaining actual Engine session guards, transaction history, and physical session ownership.
- Moved initial-open sequence migrations, durable-row validation, and temporary-preserving restoration into execution, retaining allocator behavior, serialized metadata, read guards, and ordered registry publication.
- Moved sequence DROP execution into native dependency consumers and removed Engine command callbacks, retaining owner preflight, recursive cascade order, rollback, and stable-identity cache cleanup.
- Moved sequence expression and owner-dependency analysis out of Engine, preserving actual metadata guard lifetimes and ordered CHECK, default, generated-column, and provenance removal.
- Moved legacy sequence-owner inference to SQL and initial-open migration, loaded-owner validation, and attachment publication to execution. Stable owner identities now live in Core with existing storage imports and serialized fields preserved.
- Moved table-owner transfer into native execution, preserving sequence-before-table persistence, complete schema capture, ACL rewrites, rollback, and retained table generations. Removed Engine’s table-security implementation and whole-command owner callback.
- Moved table and foreign-table access checks, maintenance authorization, and foreign security persistence into native execution; view access rules now live in SQL while Engine retains actual catalog guards and table generations.

- Opened compressed SQLite lock sidecars with read-only access for read-only connections, preserving cross-process lock coordination without requiring write permissions or creating lock paths.
- Moved SQL models, static schemas, type and routine analysis, prepared parameter inference, catalog definitions, and query binding into `uqa-sql`, with narrow engine adapters and execution-owned row buffers. Low-level physical schema operations now use `uqa_execution::RowSchemaExecution`.

- Preserved `SET LOCAL` and `SET ... DEFAULT` through compilation and execution. Local values restore at transaction completion and follow savepoint rollback; default assignments retain the PostgreSQL SET command tag.
- Corrected rare-value selectivity using the probability left after common values and NULLs, without overriding known frequencies with an entropy floor. Domain parameters retain their identities and integer widths in custom and generic plans, and domain errors use catalog-visible type names.
- Released unwritten compressed SQLite readers before waiting for writer ownership or snapshot publication. Automatic statistics publication no longer deadlocks an application COMMIT, and catalog writer fences retain savepoint behavior while refreshing committed data.
- Removed unused rewrite-rule inputs before constant planning and preserved PostgreSQL completion counts from the last unconditional INSTEAD action of the original command kind. Rule RETURNING errors retain separate primary and hint fields over TCP.
- Preserved declared empty SQL schemas through column deletion, rollback, catalog refresh, and reopen while deferring native document and table-function fields until their runtime descriptors are available.
- Propagated immutable constant-expression errors from planning with their original SQLSTATEs and declared result types. Semantic analysis precedes folding, prepared arguments bind before body planning, and zero-parameter SQL EXECUTE ignores supplied argument expressions. Runtime COALESCE stops after its first non-NULL value; stored views and routines retain logical definitions until execution.
- Deferred prepared-body optimization until execution and resolved binary operators through PostgreSQL catalog signatures. Invalid typed NULL casts and operator combinations fail during preparation; quoted `"char"` retains its internal single-byte type identity.
- Fixed stored SQL-standard routine source aliases shifting or invalidating catalog refresh after an unread column is deleted. Table input columns, nested join aliases, and expanded projections retain their creation-time shape through added columns, old-name reuse, renames, rollback, refresh, and reopen. Sequence regclass constants in generated columns bind the original object, and sequence cascades remove dependent readers while preserving unrelated routines and aliases; legacy definitions migrate during initial open.
- Fixed column renames leaving SQL-standard function and procedure bodies bound to old column names and breaking subsequent SQLite catalog refresh. Renames now retain query and DML bindings, aliases, CTE and result names, unrelated parameters, and exact column identity after old-name reuse, including rollback, sibling-engine refresh, and reopen. Missing and duplicate rename targets report PostgreSQL column SQLSTATEs.
- Fixed column deletion ignoring SQL-standard routine dependencies and breaking SQLite catalog refresh. RESTRICT protects stored readers and CASCADE follows generated columns, views, owned sequences, routines, and domains while retaining unrelated rows and routines. Stored MERGE write-only targets keep their column identity after deletion, skip retired writes and their evaluation, retain expression and non-DEFAULT domain coercion dependencies, and never retarget recreated names; rollback, shared-catalog refresh, legacy definition migration, and reopen are covered.
- Fixed relation deletion leaving SQL-standard routines bound to removed tables, views, or sequences, including function/view cycles and dependencies reached through domains or generated columns. Cascades preserve unrelated objects and late-bound bodies; constant regclass references, typed routine arguments, and defaults retain exact relation identity through rename and reopen without treating integer OID conversions as stored dependencies. Fixed stored join-plan restoration reentering the transaction lock during rollback and preserved bound routine result types when converting regclass values to text.
- Implemented DROP DOMAIN with owner and schema-owner authorization, RESTRICT, multi-target atomicity, and cascades through derived domains, columns, indexes, views, and SQL-standard query and DML routines. Added schema ownership transfer with ACL preservation, excluded inaccessible namespaces from current schema/type lookup, and accepted domain casts in stored generated expressions. Domain deletion preserves unrelated data and participates in rollback, catalog refresh, and reopen.
- Implemented ordinary schema deletion with owner checks, PostgreSQL RESTRICT and missing-schema errors, atomic multi-target CASCADE, and dependencies across tables, views, sequences, routines, and domains. Cascades preserve unrelated columns and referencing tables; dropped public schemas stay absent after reopening. Zero-column tables accept DEFAULT VALUES, and INSERT validates RETURNING references and width errors.

- Preserved SQL current date/time expression types, precision, labels, and built-in identity. Statement and transaction clocks now remain stable through nested execution, Simple Query batches, savepoints, and rollback; `transaction_timestamp()` is available, and one-argument `age` subtracts its argument from the transaction's midnight in the correct direction. A 90-case PostgreSQL 18.4 fixture verifies result descriptors and clock relationships on memory, SQLite, and forced-spill execution.
- Corrected REAL input rounding, arithmetic, mixed numeric promotion, character output, and SUM accumulation across grouped, ordered, window, and spilled execution. Shared floating conversion and arithmetic now report PostgreSQL overflow, underflow, invalid-input, and division diagnostics while retaining NaN, infinity, subnormals, and signed zero. Storage predicates preserve declared column types, and cast projection labels retain their PostgreSQL names. A 133-case PostgreSQL 18.4 fixture verifies memory, SQLite, and forced-spill execution.
- Preserved SQL PREPARE parameter declarations through compilation and execution, including typed NULLs, assignment conversions, domain arrays, exact argument errors, and constant validation before volatile argument effects. Prepared definitions survive transaction and savepoint rollback; DISCARD PLANS invalidates plans without removing definitions. Corrected transactional DISCARD variants, nontransactional sequence-state discard, and domain-array regtype output.
- Connected the native PL/pgSQL parser to an immutable engine catalog snapshot so user-defined scalar declarations retain their initial values and declaration constraints. Updated the pinned PostgreSQL 18 parser chain with catalog type callbacks, resolved datum OIDs, and structured type-name preservation; quoted domains, domain arrays, and information-schema domains keep their identity in stored routines and anonymous blocks.
- Preserved declared types when binding PL/pgSQL variables and record fields, applied domain constraints at field assignment, and validated trigger return records by position without repeating domain checks. Domain and domain-array catalog rows retain their creating role as owner. Implicit domain coercions in routine parameters, local variables, and returns include constraint-function effects when selecting the statement transaction mode.
- Preserved time and timestamp precision and interval field restrictions through casts, assignments, arrays, function-source declarations, result descriptors, catalog metadata, and SQLite reopen. Rounding retains end-of-day time values and PostgreSQL's signed timestamp behavior; CASE result modifiers follow surviving constant branches without imposing declaration limits during common-type coercion.
- Corrected compressed SQLite writer reservations so existing and new readers remain available until a writer requests exclusive access; pending writers block new readers, and other processes can detect live rollback-journal reservations. This prevents spurious writer-promotion failures during concurrent background statistics and VACUUM FULL.
- Preserved MERGE CTE scope in automatic-view rewrites, privilege analysis, and stored SQL routines; ran statement-level BEFORE triggers before source evaluation while retaining the original statement snapshot. Invalid MERGE expressions are rejected before trigger effects.
- Kept quoted rewrite-rule column names case-sensitive and returned unqualified output labels for schema-qualified function calls.
- Matched PostgreSQL duplicate-key and foreign-key diagnostics, including schema-qualified and temporary relations, and preserved inline foreign-key deferrability when compiling column constraints.
- Updated bound SQL-standard routine bodies when a referenced relation is renamed, preserving execution, exact routine dependencies, and SQLite reopen behavior for data-modifying CTEs.

### Changed

- Low-level Rust lexical query terms now use `TokenTermKey`; existing string constructors remain available. Native lexical scoring belongs to `uqa-scoring`, and analysis, storage, planning, and execution retain their owning crate interfaces.
- Operator-tree planning accepts immutable index candidates and no longer retains storage index managers; low-level Rust callers migrate from `with_index_manager` to `with_index_candidates`.

- Moved concrete SQLite catalogs, connections, indexes, transactions, compressed storage, and graph persistence into `uqa-storage-sqlite`. Rust imports of `uqa_storage::SQLite*`, `uqa_storage::sqlite::*`, and `uqa_graph::SQLiteGraphStore` now use `uqa_storage_sqlite`; shared storage errors preserve the typed provider error through `StorageBackendError::Backend`. Database formats and engine SQL behavior are unchanged.
- Made `IndexManager::new()` independent of SQLite and moved block-max SQLite persistence to the provider's `SQLiteBlockMaxPersistence` extension trait. Provider conformance tests and SQLite persistence benchmarks also belong to the provider crate; the commit hook prevents common storage and graph crates from regaining provider dependencies in any dependency kind.
- `ScalarExpr::TypedLiteral` retains optional resolved type and parameter-origin metadata; `Statement::SetVariable` and `CommandPlan::SetVariable` retain local and default flags. Older serialized plans remain readable.
- Added structured `SQLError::Diagnostic` fields and persisted `TableConstraintSet::columns_declared` metadata for declared SQL schemas.
- Unified optimizer APIs now return `OptimizerResult` with `OptimizerError::Expression` and `OptimizerError::JoinGraph`, preserving the distinction between SQL expression failures and join-graph failures.
- Rust `Statement::Prepare` and `CommandPlan::Prepare` now retain `parameter_types`; existing serialized declarations without this field remain readable.
- `REGTYPE` values, including `pg_typeof`, now retain catalog OIDs in the integer carrier so comparisons and catalog lookups use type identity. Cast to text or use `sql::format_postgres_text` for the visible type name. `SQLResult::kind` distinguishes row descriptors from commands, including zero-column queries.
- Rust AST CTEs now expose `body: CteBody` instead of the SELECT-only `query` field, and planner CTEs expose `CtePlanBody`. Match the query or command variant when traversing WITH definitions. The SELECT-only serialized `query` representation remains readable in existing catalogs.

## [0.2.3] - 2026-09-09

See the [upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.2.3/docs/manual/reference/10-upgrading.md) for Rust graph API changes, custom storage requirements, automatic statistics, and persistent catalog migration.

### Fixed

- Rejected excessively nested Cypher expressions and deep operator chains with parse errors before parser recursion or expression-tree cleanup can exhaust the stack. Wide lists and independent expressions retain their existing behavior.
- Removed the complete resident graph and reachability-index replicas from persistent engines. Startup, new sessions, catalog refresh, and graph handle cloning now retain only metadata and session-bound storage handles; point, label, and adjacency reads use indexed durable records. Graph mutations use storage checkpoints, fixed transactions combine physical snapshots with changed identities, and cursors preserve their declaration-time graph view without hydrating a complete graph in RAM. SQLite catalog version 46 adds durable path-index data and invalidation; legacy key-value graph access indexes migrate atomically at initial open.
- Bounded sampled statistics by value size as well as row count. Oversized text and binary values no longer become full-payload histogram or MCV copies; non-null counts remain represented in distinctness estimates, row and NULL counts are preserved, and legacy oversized statistics are bounded during reopen and automatically replaced without requiring another write.
- Reused unchanged SQLite table definitions, physical handles, and decoded statistics across data-only and automatic-statistics commits. Transactional, per-table cache revisions preserve snapshot and rollback visibility, refresh only changed dependencies, and share decoded committed statistics across independent sessions without a database-wide load lock. SQLite catalog version 44 installs durable invalidation tracking atomically.
- Preserved staged document-ID reservations when deferred transaction snapshots refresh for key-lock rechecks or writer promotion after a concurrent statistics or catalog commit, preventing INSERT/COPY from reusing IDs and overwriting earlier rows in the same statement or transaction.
- Replaced synchronous full-table statistics collection during persistent query planning with automatic database-level background maintenance. Committed-change thresholds and dirty-age scheduling use durable counters, preserve existing estimates during refresh, survive restart, and respect rollback. Bounded projected samples avoid unrelated BLOB payloads; explicit full refresh persists through the ANALYZE transaction path and histogram construction avoids redundant payload copies.
- Shared immutable catalog registries and table definitions across statement snapshots and new sessions from a stable committed parent, while retaining independent physical storage handles, copy-on-write mutations, transaction isolation, and temporary-object visibility.
- Pinned external catalog refresh to one read snapshot so concurrent commits cannot mix catalog generations or deadlock recursive rule/trigger validation during reopen.

### Changed

- Rust graph callbacks now receive `GraphStoreHandle`, and `GraphStore` reads return owned, fallible results. Custom graph stores implement paged identity access, edge memberships, and atomic mutation checkpoints; custom catalogs implement direct graph access and durable path-index data. See the [graph API upgrade notes](https://github.com/cognica-io/uqa-engine/blob/v0.2.3/docs/manual/reference/10-upgrading.md#rust-graph-api-and-custom-catalogs) before updating an embedded application or storage provider.

## [0.2.2] - 2026-09-06

See the [upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.2.2/docs/manual/reference/10-upgrading.md) for package updates, SQL constraint behavior, Rust AST changes, and CHECK catalog migration.

### Added

- Added a checksum-pinned inventory and official-driver harness for all 354 PostgreSQL 18.4 core and isolation tests, complete ownership accounting, strict result and provenance checks, and the full PostgreSQL reference run in pre-merge CI. UQA execution of this corpus remains part of the compatibility work.

### Fixed

- Fixed recursive `ALTER TABLE` authorization before merging existing child columns and constraints, with PostgreSQL inheritance-edge traversal, unchanged-definition boundaries, and failure-atomic rollback.
- Preserved PostgreSQL 18 NO INHERIT metadata for `ONLY SET NOT NULL` on ordinary inheritance parents, rejected forbidden recursive and partition-parent changes with matching SQLSTATEs, retained local NOT NULL metadata when merging nullable inherited columns, projected NOT NULL inheritance counts from direct parent constraints, and preserved constraint identity, row validation, rollback, and durable reopen behavior.
- Recorded NOT NULL local origin independently from inheritance counts, preserving local declarations and inherited names through recursive changes, parent removal, partition attachment and detachment, validation, rollback, and reopen; prevented NO INHERIT constraints from being copied to new descendants.
- Merged equivalent inherited and local CHECK constraints, retained their independent local origin and direct-parent counts, and excluded NO INHERIT checks from descendants. Recursive additions, validation, removal, and renaming now follow PostgreSQL inheritance boundaries, preserve constraint identity, and roll back atomically; adding a column propagates its CHECK independently from existing child-column merges.

### Changed

- Added `ColumnDef.not_null_is_local` to the public Rust SQL AST. Applications constructing column definitions directly must initialize the field; older serialized definitions retain the previous local-origin projection without a NOT NULL metadata migration.
- Added `ColumnDef.check_is_local`, `ColumnDef.check_object_id`, `TableCheck.is_local`, and `TableCheck.object_id` to the Rust SQL AST. Initial catalog open assigns missing CHECK identities; legacy definitions retain their previous local-origin projection because their declaration history was not stored.
- The generated README files for `uqa`, `uqa-engine`, `uqa-client`, `uqa-api`, and `uqa-cli` link to `main` for development versions and the exact version tag for releases.

## [0.2.1] - 2026-09-06

See the [upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.2.1/docs/manual/reference/10-upgrading.md) for package updates and the Python CLI fix.

### Fixed

- Fixed the Python-installed `usql` entry point so interactive startup and SQL script execution use Python's command-line arguments instead of treating the console launcher or interpreter options as SQL input.

## [0.2.0] - 2026-09-05

See the [upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.2.0/docs/manual/reference/10-upgrading.md) for package versions, custom Rust storage API changes, and persistent database migration.

### Added

- Added cross-relation operator joins for text, vector, graph, hybrid, and cross-paradigm retrieval, with equivalent executed examples in Rust, Python, Node.js, and Browser WASM.
- Added PostgreSQL 18 ordinary-table ownership and relation ACLs, database and schema privilege inquiry and enforcement, creation and temporary-object authorization, and schema-qualified index identities.
- Expanded PL/pgSQL with array `FOREACH`, query and cursor `FOR` loops, assertions, dynamic cursor opening and movement, and result cursors for DML, `CALL`, and `MERGE`; aligned scroll-cursor volatile evaluation, first-fetch command execution, and `UNION ALL` traversal with PostgreSQL 18.
- Added bounded notification queue accounting, PostgreSQL `void` results for `pg_notify`, and committed notification delivery between native processes opening the same file-backed database, including polling, waiting, listener cursors, and process-liveness cleanup.
- Added PostgreSQL view definition reconstruction with `pg_get_viewdef`, stable relation and row-type identities across view and foreign-table renames, standalone unique-index enforcement, and index definition reconstruction with `pg_get_indexdef`.
- Made the Node.js `HttpEngine`, streaming reader, and SQL parameter helpers independent of native addons, with a pure JavaScript HTTP implementation, lazy embedded-engine loading, a dedicated `/http` package entry point, exact int64 and binary conversion, bounded CLI lookup and responses, stream cancellation, and offline package-installation tests on Node.js 16 and newer.
- Implemented immutable scalar B-tree expression keys with typed binding, composite unique enforcement, partial and expression `ON CONFLICT` inference through aliases and views, atomic build and mutation behavior, routine and column dependencies, creation-time index attribute names and types, `pg_get_indexdef` and `pg_index` expression metadata, and durable SQLite, SQLite key-value, and redb postings with rollback recovery. Added PostgreSQL `format_type(oid, integer)` for represented catalog types and checked-in PostgreSQL 18.4 oracle coverage.
- Extended PostgreSQL 18 routine dependency ownership to user-routine calls in trigger `WHEN` conditions, including exact overload binding at trigger publication, replacement dependency updates, routine rename and old-name recreation isolation, RESTRICT, trigger-granular CASCADE that retains the relation and trigger execution function, transactional initial-open migration, validation-only reloads, durable reopen, and checked-in PostgreSQL 18.4 oracle evidence.
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

### Changed

- Changed custom Rust `DocumentStore` implementations to persist typed `StoredDocument` records with separate `DocumentMetadata`, and changed `PersistentStorageBackend` B-tree methods to use `ValueIndexKey` so column accelerators and named SQL indexes have distinct physical identities. These storage trait changes require custom implementations to be updated for 0.2.0.
- Refactored the Rust workspace around explicit engine capabilities and subsystem ownership, with repository checks enforcing dependency boundaries, source-file limits, and consolidated integration-test harnesses.

### Fixed

- Made generated Rust crate README links point to the matching GitHub release tag, including release notes, upgrade guidance, licensing, and directory links.
- Kept native cross-process notification worker state out of Browser WASM builds while retaining the in-process notification queue.
- Deferred `CREATE TABLE IF NOT EXISTS` and `CREATE FOREIGN TABLE IF NOT EXISTS` definition analysis until after namespace authorization and shared relation-name collision checks, so existing targets now skip invalid or unsupported types, columns, constraints, hierarchy, typed-table sources, options, tablespaces, foreign servers, and implicit sequence creation with PostgreSQL 18.4 notice and error ordering.
- Retained each foreign table's complete SQL column and CHECK schema as its sole durable runtime definition instead of reconstructing it from the lossy FDW name/type projection, with stable table and column object identities, versioned initial-open migration, atomic validation and publication, catalog visibility, exact routine and sequence rename and DROP dependencies, owned `SERIAL` and identity sequences, PostgreSQL-style implicit sequence name clipping and collision suffixes, `pg_get_serial_sequence`, owner transfer, rollback and reopen coverage, relation-kind notices, and fail-closed unsupported key declarations.
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
