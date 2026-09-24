# History

All notable changes to `uqa-engine` are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Support overlapping logical write transactions through Engine SQL on native SQLite, SQLite Key/Value and redb. Independent sessions can commit unrelated writes while another write transaction remains open; shared MVCC merges evaluated document, full-text, vector, graph and catalog changes before short atomic provider publication. READ COMMITTED refreshes each command, while REPEATABLE READ and SERIALIZABLE retain their transaction views and conflict tracking. Rust, Python, Node.js, browser WASM and PostgreSQL TCP APIs use the same transaction contract.
- Add managed commit-receipt ownership, explicit terminal acknowledgement and bounded reclamation. Preserve live owners, unresolved outcomes and serializable dependencies; the default database-wide limit is 65,536 entries and exhaustion reports SQLSTATE `53400`. See the [receipt retention contract](docs/manual/reference/10-upgrading.md#unreleased-mvcc-writer-compatibility).
- Add explicit restoration of closed SQLite backups through `DatabaseRestore` and `ManagedConnection::open_restored`, including encrypted and compressed variants. Restoration preserves stored data addresses and allocation watermarks while replacing transaction-history identity; interrupted restoration resumes only the original persisted request. See the [backup restoration contract](docs/manual/reference/10-upgrading.md#unreleased-sqlite-backup-restoration).

### Changed

- Advance SQLite main record format to 46, redb main record format to 45 and native SQLite mapping format to 9; catalog format remains 49. Atomic upgrades preserve records, identities, histories and identifier allocations, while preserving existing receipt capacity, acknowledgement and ownership; predecessors without ownership retain their receipts as manually owned outcomes. Reopened and retained incompatible readers and writers are rejected. Restore a pre-upgrade backup to return to an earlier format. See the [writer compatibility contract](docs/manual/reference/10-upgrading.md#unreleased-mvcc-writer-compatibility).

- Keep produced and retained query values under their original memory and cancellation allowance through defaults, generated expressions, row/page construction and callback handoff. Selected analyzer/catalog generations, text-index nodes and vector collections retain their owners until the final reader releases them; failed production preserves previously admitted results and the original error.
- Share immutable memory document/text-index snapshots and retain versioned provider views without copying the entire corpus. Controlled identity and field readers keep bounded pages alive through consumers; custom providers must forward the [controlled read and compound operation contracts](docs/manual/reference/10-upgrading.md#unreleased-rust-keyvalue-compound-operations).
- Return `Budgeted<RetainedAnalyzedField>` from `analyze_index_field_budgeted`, with Core-owned term nodes. Ordinary `AnalyzedField` keeps its existing carrier. See the [Rust source upgrade](docs/manual/reference/10-upgrading.md#unreleased-retained-text-analysis-owners).
- Retain durable namespace OIDs and incarnations through ACL/owner changes, undo and reopen, while recreation gets a new identity. Coordinate schema creation names, lifetime binding, catalog tuple changes and relation-creation dependencies across native SQLite, SQLite Key/Value and redb; preserve PostgreSQL committed-update errors and rollback outcomes, including unchanged ACL commands. Schema security format 2 and common record format 31 fence incompatible predecessors.
- Preserve private schema creation, deletion, ACLs and ownership when direct catalog reads refresh a fixed transaction snapshot after a peer commit. Namespace authority overlays exact private records, including deletions, while unrelated committed schemas stay current.
- Persist ordinary relation and column ACL tuples independently across native SQLite, SQLite Key/Value and redb. GRANT/REVOKE now coordinate catalog tuple updates, preserve independent column commits and combine private ACL changes with fresh authority under fixed data snapshots. Record format 30 rejects writers that cannot read these tuples; definition lifecycle operations compact, move and remove the records atomically.
- Routed bound native SQLite occurrence indexes through common logical sessions, including retained cursors, source rebuilds and scorer-versioned block maxima. Mapping format 4 normalizes legacy skip/block-max tables; source and column changes invalidate derived accelerators and reject late competing builds.
- Routed bound native SQLite exact, IVF and HNSW vector APIs through common logical sessions. Canonical tensors and index metadata publish atomically, retained snapshots survive rollback and lifecycle changes, and HNSW caches distinguish committed and private graph generations. Existing IVF assignments and HNSW topology reopen without rebuilding.
- Routed native SQLite catalog cache generations through retained committed/private snapshots, including savepoint restoration, independent writers and bounded changed-key projections.
- Routed bound native SQLite graph catalog mutations, snapshot replacement/removal and path-index definitions/pairs through common logical transactions and shared cache validation. Mapping format 3 atomically adds versioned graph-to-path ownership while preserving predecessor histories and receipts.
- Added versioned native SQLite graph lookup entries for label, adjacency and membership reads, and routed native catalog graph reads and named-graph hydration through one retained logical snapshot. The development native adapter atomically upgrades mapping format 1 to 2 while preserving source history, original commit boundaries and receipts.
- Routed redb Key/Value, catalog and backend sessions through common logical transactions with pinned reads, private savepoints, bounded retention and durable commit receipts. Independent direct Key/Value writers can commit concurrently.
- Added an atomic, one-way redb record-format upgrade that rejects released 0.3.6 writers after migration. The default private session allowance is 64 MiB and can be configured with `RedbStorage::open_with_options`. See the [unreleased upgrade contract](docs/manual/reference/10-upgrading.md#unreleased-redb-record-format).
- Routed SQLite Key/Value sessions and their connection clones through the same logical transactions, including SQLCipher and compressed variants. Independent direct writers retain private changes without holding SQLite's physical writer. Legacy Key/Value files migrate atomically and reject released 0.3.6 writers. See the [SQLite upgrade contract](docs/manual/reference/10-upgrading.md#unreleased-sqlite-keyvalue-record-format).

### Fixed

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
- Wait through physical SQLite record-writer contention without replaying evaluated writes. Autonomous sequence reservations, document identity allocation and logical publication observe execution cancellation; a busy COMMIT retains its staged transaction while waiting. Rollback cleanup and ordinary sibling sessions retain independent cancellation. Custom versioned storage wrappers must forward the new [write cancellation contract](docs/manual/reference/10-upgrading.md#unreleased-storage-write-cancellation).
- Retain table, view, materialized-view and foreign-table owner/ACL role incarnations through catalog snapshots, refresh, undo and reopen. Initial restoration converts legacy names atomically; malformed current identities never rebind to replacement roles. Include column ACLs in table refresh fingerprints so unrelated catalog publication preserves private grants. Development record format 22 excludes prior writers.
- Read sequence definitions, ACLs, roles and memberships from one retained provider snapshot for value functions and introspection, preserving private and temporary state. Concurrent grants and membership revocations no longer mix new sequence metadata with stale authorization. Direct Rust value calls refresh command metadata outside active SQL callbacks. Ordinary transaction data snapshots remain fixed under REPEATABLE READ and SERIALIZABLE.
- Preserve private sequence creation, alteration, ownership, ACLs, rename and deletion when unrelated commits refresh a fixed transaction's catalog. Nested host queries can still resolve sequences created by their own transaction, and temporary entries stay local to their session.
- Keep public Rust sequence inspection from replacing live sequence metadata without its matching role catalog. Snapshot, name-list and state reads retain committed values, private definitions and temporary entries while preserving ordinary transaction data visibility and retained query catalogs.
- Retain sequence owners, ACL grantees and grantors by role OID and incarnation through creation, GRANT/REVOKE, owner transfer, snapshots and dependency checks. Initial open validates the complete sequence catalog before converting legacy names; failed later restoration rolls back conversion, while refresh and secondary sessions reject unconverted or corrupt authority. SQLite and Key/Value codecs distinguish identities from legacy names without fallback. Common record format 23 fences preceding sequence writers; native mapping 8 and catalog version 49 are unchanged.
- Keep live sequence definition and dependency refresh consistent with its role and membership catalog after peer commits. Reuse execution's complete snapshot read before installing state, preserving private role grants, temporary ownership, savepoint undo and autonomous sequence values.
- Keep sequence privilege inquiry on coherent role, membership and sequence ACL snapshots, retaining explicit role identities through name resolution. Validate privileges before target lookup and honor PUBLIC grants for `public` and absent role OIDs, matching PostgreSQL 18 error precedence.
- Honor owner self-revocation of ordinary table, column and sequence privileges while retaining implicit grant options and ownership authority. Sequence parameter inspection and materialized-view refresh require their ordinary privileges even for the owner; denied sequence operations preserve cached reservations.
- Retain explicit role identities in table and column privilege inquiries, apply PUBLIC grants to the `public` subject and absent role OIDs, and reject invalid privilege strings before missing-OID or invalid-attribute `NULL` results while preserving named-object error precedence. Sequence targets share one detached authority snapshot for OID binding and all requested privileges, including current role memberships and private or temporary ACLs. Explicit subjects see newly committed roles without replacing the transaction's ordinary data snapshot.
- Preserve quoted role names in object grants, grantor paths and owner transfers. A role named `"PUBLIC"` has independent privileges from the PUBLIC recipient, and quoted session-keyword names remain literal through rollback, refresh and reopen. Typed ACL and statement encodings retain legacy meanings; development record format 21 excludes incompatible writers. See the [unreleased role catalog upgrade notes](docs/manual/reference/10-upgrading.md#unreleased-role-catalog-records).
- Merge independent native SQLite HNSW document writers through the common vector journal and resolver. Preserve serial node allocation, topology and compaction, retained snapshots, savepoint undo and atomic publication retries. Share IVF/HNSW native codecs, staging and document/lifecycle guards. Native mapping format 7 upgrades formats 1–6 atomically, retaining existing row encodings and family 49 guard histories; older writers reject the new marker.

- Merge independent SQLite Key/Value and redb HNSW document writers using the same input journal, conflict validation and receipt-safe resolution as IVF. Preserve exact serial graph topology through node allocation and compaction, retain document/lifecycle conflicts and reject incomplete persisted tensors. Bound graph reconstruction and JSON buffers; upgrade development MVCC formats 1/2/3 atomically to format 4 without rewriting histories or receipts.
- Bound native SQLite and Key/Value HNSW candidate preparation and explicit reconstruction under the caller’s memory and cancellation allowance. Preserve serial graph topology, node allocation, compaction and incremental deltas while keeping failed candidates separate from the source. Reject incomplete canonical tensor ordinals during reconstruction.
- Merge independent native SQLite, SQLite Key/Value and redb writers sharing one IVF index, preserving ordered training changes, tensors, snapshots and savepoint undo. Retain conflicts for overlapping documents and index lifecycle changes, reject mismatched canonical inputs, and resolve publication retries with the original receipt identity. Development MVCC record format 4 upgrades formats 1/2/3, and native mapping format 6 adds IVF guards with an atomic upgrade from mappings 1–5. Both preserve existing source histories and IVF row encodings.
- Preserve bound native SQLite IVF centroids and deletion counters through ordinary document changes, and retrain at the common storage threshold. Native and Key/Value IVF mutations now share bounded, cancellable candidate preparation. Reject missing or inconsistent native IVF generations instead of silently using exact search; explicit initialization rebuilds from canonical tensors.

- Merge independent native SQLite, SQLite Key/Value and redb occurrence writes sharing posting clusters and field totals. Preserve same-document and structural conflicts, invalidate late accelerator builds, and retain bounded preparation, savepoints and receipt-based retry without replaying analysis or scoring. Development MVCC record format 2 upgrades prior metadata atomically and rejects older writers afterward; native mapping format 5 adds document/structural guards with an atomic predecessor upgrade.
- Validate bound native SQLite catalogs on their retained committed/private view during Engine restoration. Reject inconsistent schema, relation, definition and index references without reading raw physical rows; B-tree validation uses bounded key pages without loading entry payloads.
- Keep bounded common occurrence cursors on their original snapshot across cluster pages, and aggregate cross-field term frequencies on one view. Deterministic interleavings verify that later replacements cannot mix new frequencies into an older result.
- Preserve common Key/Value occurrence snapshots and compound reads across later writes, rollback and independent commits. Posting data, source metadata and statistics share one read boundary; mutations retain their original write preconditions without replay. SQLite Key/Value and redb snapshots retain MVCC owners without copying the index corpus.
- Preserve common Key/Value vector generations across rollback and independent commits. IVF and HNSW share compound read/evaluation and cache identity handling; exact snapshots retain their original tensors and memory reservation instead of following the live session. All three snapshot kinds reject mutation after their original handles close. Failed operations preserve earlier private writes without replaying evaluation. SQLite Key/Value and redb share this implementation; custom vector providers must implement the [compound operation contract](docs/manual/reference/10-upgrading.md#unreleased-rust-keyvalue-compound-operations).
- Prevent SQLite HNSW searches from reusing discarded graphs after rollback or same-revision index recreation. Cache selection, candidate evaluation and persistence now share one physical view; intervening rebuilds cannot publish an obsolete candidate.
- Preserve embedded NUL characters in metadata-derived cache names and atomically repair the known prior triggers. Native conversion and reopen now reject missing or changed catalog cache tracking.
- Preserve graph path-cache validity across overlapping development SQLite Key/Value and redb transactions: merge source invalidations, include late graph dependencies, reject stale builds and preserve caches reassigned to another graph. Savepoints and receipt-based commit retry retain these effects without repeating graph evaluation.
- Reject duplicate sequence incarnations during native SQLite record conversion and competing live definition generations at commit, including aliases created concurrently by independent sessions. Native sequence catalog and value operations now share the selected logical session.
- Preserve data when the SQLite catalog column-rename API receives the same source and destination name. Move or remove B-tree repair markers with their column so renamed and deleted fields do not leave stale repair requests; preserve an existing destination B-tree without mixing in discarded source postings, including when SQLite foreign-key cascades are disabled.
- Preserve and resolve uncertain logical commits through Engine without replaying SQL preparation or Rust callbacks. Typed storage outcomes retain their transaction identity across later failures; matching receipts complete session publication, and rollback cannot report success for already committed data. `Engine::pending_commit` exposes retained resolution state.
- Roll back a retained storage transaction before refreshing Engine caches after a failed commit. If rollback also fails, preserve the failed Engine frame and locks until storage cleanup succeeds instead of exposing private catalog or graph state as committed.
- Reject persistent catalog/backend pairs from different reported transaction contexts before Engine restoration or sibling attachment, including separate sessions over the same file. Native SQLite, SQLite Key/Value and redb expose the shared affinity contract; custom wrappers must forward it as described in the [Rust upgrade notes](docs/manual/reference/10-upgrading.md#unreleased-rust-session-affinity).

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
