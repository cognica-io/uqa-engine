# PG18 differential probes

`run_diff.py` validates `manifest.json`, executes every probe in `probes.sql` against a real PostgreSQL 18 instance and against the `usql` release binary, then reports divergences in four categories:

- `engine-error`: PostgreSQL answers, the engine rejects (missing feature).
- `engine-accepts`: PostgreSQL rejects, the engine answers (missing guard, e.g. division by zero).
- `sqlstate-mismatch`: both engines reject, but with different SQLSTATE codes.
- `value-mismatch`: both answer, values differ after normalization (boolean display and numerically equivalent float formatting are normalized; JSON and JSONB output text is compared exactly).

## Prerequisites

- A PostgreSQL 18 container named `uqa-pg18` with user `postgres`, database `uqa`:

  ```sh
  docker run -d --name uqa-pg18 \
    -e POSTGRES_PASSWORD=uqa -e POSTGRES_DB=uqa \
    -p 15432:5432 postgres:18
  ```

- A release build of the CLI: `cargo build --release -p uqa-cli`.

## Run

```sh
python3 tests/parity/pg18/run_diff.py --validate-manifest
python3 tests/parity/pg18/run_diff.py
```

Manifest schema version 2 records the pinned parser chain, oracle provenance, milestone titles and exit gates, exact single ownership of every evidence item, positive evidence, and every currently tracked incomplete gate. The validator derives milestone states from owned item statuses, synchronizes the plan ledger and manual snapshot, rejects malformed ownership, stale wrapper revisions, duplicate or orphaned items, verified items with open issues, and any complete-compatibility claim made before M6 and every item are complete.

The differential summary line reports `total/match/diff`, and any difference makes the runner exit nonzero. Error rows match only when their SQLSTATE codes match; message text is not compared. At this revision `probes.sql` contains 797 probes. Update it freely: one probe per line, `--` comments skipped; probes must be side-effect-free single statements. Set `UQA_PG_CONTAINER`, `UQA_PG_DATABASE`, or `UQA_USQL` to override the defaults while keeping both systems under test in equivalent contexts.

## MERGE and RETURNING oracle

[`merge_returning_oracle.md`](merge_returning_oracle.md) records the pinned PostgreSQL 18.4 container provenance, full-join candidate results, clause-order and visibility SQLSTATEs, repeated-target cardinality behavior, all mutation row images, `DO NOTHING`, source-column NULLs, `merge_action()`, and source-before-target `RETURNING *` layout used by the focused compiler and engine tests.

## Stateful routine oracle

`run_routines_stateful.py` executes the 200 delimited cases in `routines_stateful.sql` against PostgreSQL 18.4 with Apache AGE and UQA, then compares both results with `routines_stateful.expected.json`. It covers polymorphic and variadic resolution, pseudo-type declaration validation, user `pg_proc` identity, ALTER lifecycle, persisted concrete bindings, routine ownership and security, PL/pgSQL array `FOREACH`, static-query, dynamic-query, and bound-cursor `FOR` traversal, assignment-style Boolean conditions, `ASSERT`, lazy messages, assertion handlers and settings, nontransactional sequence effects, expression evaluation, portal prefetch and cleanup, `FOUND`, validation order, session portals, transitive function and procedure `DROP CASCADE` effects, and exact SQLSTATEs.

`routine_rename_oracle.sql` records PostgreSQL 18.4 function, procedure, and routine rename behavior. The checked-in transcript verifies overload and kind selection, omitted-signature ambiguity, collisions, missing targets, rollback, stable `pg_proc` OIDs and `information_schema.routines.specific_name` suffixes, `CREATE OR REPLACE` identity preservation, bound SQL-standard bodies, scalar and table-function views, generated columns, rewrite rules, trigger targets, dependency rejection, old-name recreation isolation, and the intentionally dynamic behavior of SQL string and PL/pgSQL bodies.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/routine_rename_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/routine_rename_oracle.expected.txt -
```

The same runner's `--suite roles` mode executes the 182 cases in `roles_stateful.sql` and compares them with `roles_stateful.expected.json`. It covers PostgreSQL 18 role-membership defaults and option changes, independent grantors, ADMIN dependency `RESTRICT` and `CASCADE`, cycles, CREATE ROLE membership clauses, legacy ALTER GROUP, transactions and reopen, `pg_auth_members`, CREATEROLE attribute-delegation limits, transitive SET, INHERIT, and ADMIN behavior, all six name/OID `pg_has_role` overloads and privilege modes, scalar and array `regrole` OID storage, role-removal output, stored scalar-constant dependency rejection, CHECK, partition-key, and routine error precedence, and allowed runtime forms, stable-function generated-column rejection, membership-aware routine ownership and ACLs, unauthorized replacement and drop, SECURITY INVOKER role persistence, SECURITY DEFINER restrictions, role-drop dependencies, and exact SQLSTATEs.

The same runner's `--suite constraints` mode executes `constraints_stateful.sql` and compares it with `constraints_stateful.expected.json`. The 162-case transcript covers named CHECK, foreign-key, and `NOT NULL` `NOT VALID` state, validation and enforcement failure atomicity, supported ALTER forms, inferred primary-key references, exact referenced-key and physical-partition event identity, directional and temporal cross-type keys, initially-deferred outer-commit and savepoint behavior, `SET CONSTRAINTS` name resolution and trigger recreation, dependency-aware drops and pending-event precedence, multi-action rollback, catalog flags, and exact SQLSTATEs.

The `--suite type-temporal` mode executes `type_temporal_stateful.sql` and compares it with `type_temporal_stateful.expected.json`. It covers built-in range and multirange identity, canonical values and operators, polymorphic range routine resolution, failure-atomic type rewrites, `WITHOUT OVERLAPS`, aggregate `PERIOD` coverage, catalog persistence, and exact SQLSTATEs.

The `--suite triggers` mode executes the 609 cases in `triggers_stateful.sql` and compares them with `triggers_stateful.expected.json`. It covers trigger creation and executable replacement, row and statement execution, `WHEN` validation and timing, generated-row images, zero-row updates, `TRUNCATE`, constraint-trigger immediate and deferred execution, `SET CONSTRAINTS`, captured row images, savepoint and commit lifecycle, trigger-owned constraints, partition clones, catalogs, enable and independent rename lifecycle, `session_replication_role` mode selection and foreign-key suppression, dependency drops, queued-event cancellation, transition-relation definition validation, typed setwise execution for multi-row and zero-row mutations, `INSERT SELECT`, `ON CONFLICT`, `UPDATE FROM`, `MERGE`, partition-moving UPDATE, UPDATE FROM, and MERGE row-trigger and transition lifecycles including cancellation and destination mutation, partition and inheritance descendants, cumulative direct multi-foreign-key actions, statement-global event ordering across independently prepared multi-row conflict-update cascade trees, PostgreSQL's recursive chain and branching cascade waves with coalesced, split, and trailing empty sets, statement-start source and target snapshots across `BEFORE` statement-trigger writes, `INSTEAD OF` view-trigger definition validation, alphabetical row chaining, suppression, statement timing, row-image `RETURNING`, `UPDATE FROM`, `DELETE USING`, rename, drop, enable-mode rejection, catalog and reopen behavior, automatically updatable nested view `INSERT`, implicit leading-column values and queries, scalar, `EXISTS`, and `IN` subqueries in view projections and predicates, correlated and unqualified view-definition references, local-alias collision avoidance, statement snapshots, `OLD` and `NEW` computed row images, check options, rewrite-rule images, `ON CONFLICT`, `UPDATE FROM`, `DELETE USING`, `MERGE`, public-row-type name boundaries with fixed star expansion, correlated scalar-subquery binding across the complete containing DML namespace, including target and `excluded` rows, `FROM` or `USING` sources, and explicit `OLD` and `NEW` `RETURNING` aliases, ordinary source relations named `old` or `new` and source-only hidden-name resolution including unaliased derived sources, target-before-source `UPDATE FROM` and `DELETE USING` plus source-before-target `MERGE RETURNING *` layout, computed projections and physical partition `tableoid` row images, base-trigger routing, automatic `MERGE` user-rule rejection, nested-layer view `ALSO` and `INSTEAD` rule suppression and action ordering including nonautomatic rule-backed inner-view boundaries, qualification-before-action row projection, provider-level `RETURNING` subqueries, pre-routing NULL `tableoid`, suppressed `UPDATE` and `DELETE` zero command counts, final-action `INSERT` rule command counts, base-rule defaults, actual rule-provided `RETURNING` selection, direct rule-plus-trigger ordering, conditional-INSTEAD rejection, unconditional-rule input, source, assignment, and view-materialization laziness, omitted computed-column NULL rule images, computed-column action inputs, `CASE` short-circuiting, rule action event cardinality, view-rule `ON CONFLICT`, uniqueness-before-check-option error order, duplicate view mappings, rule-backed information-schema flags, original view-trigger suppression, and suppressed INSERT identity/default behavior, duplicate ordinary `id` persistence across reopen, defined-but-suppressed `INSTEAD OF` triggers, inner-to-outer `LOCAL` and `CASCADED` check options after `BEFORE` triggers including `MERGE`, failure atomicity, system and no-writable-column handling, materialized-view relation errors, information-schema flags, target-list SRF and CREATE or ALTER check-option rejection, non-updatable shape errors, canonical catalog deparsing, nested-routine scope isolation, persistence guards, and exact SQLSTATEs.

The trigger suite's view `MERGE` slice covers direct and nested automatic-to-trigger targets, INSERT, UPDATE, DELETE, and DO NOTHING actions, action-path selection and errors, statement-trigger order, current, OLD, and NEW row images, NULL suppression, repeated candidates, replication-mode suppression, user-rule rejection, final check options, hidden target rows, failure atomicity, and statement-start snapshots.

The trigger suite's DML row-image slice covers ordinary and partitioned INSERT, UPDATE, DELETE, ON CONFLICT, and MERGE, including BEFORE-trigger mutation and suppression, trigger assignments that do not replace the original OLD image, AFTER-trigger writes that do not retroactively change RETURNING, stored generated values, same-leaf and cross-leaf updates, and source and destination physical `tableoid` identities.

The `--suite rules` mode executes the 194 rewrite-rule cases in `rules_stateful.sql` and compares them with `rules_stateful.expected.json`. It covers `OLD` and `NEW` binding including nullable integer row images, collision-free and correlated LATERAL action sources, PostgreSQL CTE, set-operation member, conditional set-operation action, and `ON CONFLICT` reference-scope errors, qualified and unqualified conditions, alphabetical action ordering, `ALSO`, conditional and unconditional `INSTEAD`, `NOTHING`, set-oriented action and statement-trigger cardinality, INSERT SELECT, row-independent UPDATE and DELETE action qualification cardinality including no-predicate, false-predicate, empty-target, `UPDATE FROM`, and `DELETE USING` cases, positional DML `RETURNING` provider validation, lazy projection evaluation, action row images, aliases, UPDATE-provider `UPDATE FROM` source columns, DELETE-provider `DELETE USING` source columns, view-target action validation, canonical recursion detection, DML restrictions, `session_replication_role` mode selection, `pg_rewrite` and `pg_rules`, enable and rename lifecycle, persistence-safe replacement, token-safe column dependency rewrites, reserved `_RETURN` naming, view `_RETURN` replacement and protection, materialized-view rejection, and exact SQLSTATEs.

The `--suite transactions` mode executes the 275 cases in `transaction_stateful.sql` and compares them with `transaction_stateful.expected.json`. It covers schema catalog/session views, `CREATE SCHEMA` rollback, commit, and durable reopen, execution-free relation and column validation at `DECLARE`, lazy row evaluation, deferred execution errors and transaction cleanup, typed zero-movement and one-row incremental portal execution including target-list evaluation for `OFFSET` rows and stopping at `LIMIT`, directional volatile target-list and filter reevaluation during scroll-cursor revisits, native `UNION ALL` branch traversal and complete-Append materialization when a branch cannot scan backwards, declaration-time relation, virtual-catalog, view-name, nested-view-plan, literal-regclass sequence, routine-definition, and inheritance binding, `AccessShare` relation locking, `DECLARE`-time snapshots including this transaction's own changes, fixed-snapshot relation lifetimes and transaction-local writes across transactional DDL, volatile cursor routines observing earlier cursor writes, PL/pgSQL row-returning command cursors with first-use execution, error timing, output-producing `CALL`, `MERGE ... RETURNING`, and PostgreSQL command scrolling rules, stable relation OIDs across rename and `TRUNCATE` with replacement after drop and recreation, PostgreSQL holdable-cursor rewind and materialization timing, deferred-constraint revalidation after materialization, targeted `VACUUM FULL`, read-only and post-write nontransactional `ANALYZE`, snapshot acquisition by `PREPARE`, `pg_attribute` missing-value behavior for fast and volatile added-column defaults, sequence cache reservation, session-local consumption, invalidation, rollback behavior, name and schema lifecycle with stable OIDs and dependencies, logged-state transitions, and stable sequence ownership across renames, owner drops, transitive `RESTRICT` and `CASCADE`, `TRUNCATE`, transactions, detachment, serial and identity dependency kinds, and `pg_get_serial_sequence`.

`sequence_persistence_session_oracle.sql` uses `dblink` to change two cached sequences from a sibling PostgreSQL 18.4 backend. The checked-in transcript verifies that an actual logged-state change invalidates the first backend's reserved block, while a same-state change preserves it and both paths retain session-local `currval`.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/sequence_persistence_session_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/sequence_persistence_session_oracle.expected.txt -
```

`sequence_name_lifecycle_session_oracle.sql` uses `dblink` to rename and move cached sequences from a sibling PostgreSQL 18.4 backend. The checked-in transcript verifies that both operations retain the first backend's reserved values, `currval`, `lastval`, stable OIDs, and numeric `regclass` calls.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/sequence_name_lifecycle_session_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/sequence_name_lifecycle_session_oracle.expected.txt -
```

`database_privilege_oracle.sql` records PostgreSQL 18.4 database ownership, ACL, and privilege inquiry behavior. The checked-in transcript verifies default `PUBLIC` privileges, explicit grants and revokes, rooted grant-option chains, dependent `RESTRICT` and `CASCADE`, owner and inherited-owner privileges, all six name/OID `has_database_privilege` overloads, strict NULLs, missing objects, error precedence, exact `pg_database` ACL output, and exact `pg_proc` identities.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/database_privilege_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/database_privilege_oracle.expected.txt -
```

`database_privilege_enforcement_oracle.sql` records PostgreSQL 18.4 database `CREATE` and `TEMPORARY` enforcement. The checked-in transcript verifies schema and every implemented temporary relation or index creation form, exact precedence across existing, reserved, qualified, invalid-definition, and missing-source targets, inherited grants, immediate revocation after temporary-namespace allocation, and `DISCARD TEMP` behavior.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/database_privilege_enforcement_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/database_privilege_enforcement_oracle.expected.txt -
```

`schema_privilege_inquiry_oracle.sql` records PostgreSQL 18.4 schema privilege inquiry behavior. The checked-in transcript verifies all six name/OID `has_schema_privilege` overloads, owner and inherited-role privileges, ACL and grant-option paths, comma-any checks, system and current temporary namespaces, strict NULLs, missing OIDs, name and OID resolution precedence, and exact `pg_proc` identities.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/schema_privilege_inquiry_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/schema_privilege_inquiry_oracle.expected.txt -
```

`schema_create_enforcement_oracle.sql` records PostgreSQL 18.4 schema `CREATE` enforcement across supported schema object boundaries. The checked-in transcript verifies tables, CTAS, `SELECT INTO`, views, materialized views, sequences, foreign tables, functions, procedures, indexes, indexed key constraints, qualified and search-path targets, collision and definition-error precedence, inherited grants, immediate revocation, inferred temporary views, transactions, and savepoints.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/schema_create_enforcement_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/schema_create_enforcement_oracle.expected.txt -
```

`routine_schema_usage_oracle.sql` records PostgreSQL 18.4 schema `USAGE` enforcement at routine name-resolution boundaries. The checked-in transcript verifies qualified and effective-search-path function and procedure calls, argument column and type errors before namespace checks in projections and filters, missing-object and schema-before-`EXECUTE` precedence, ALTER, GRANT, REVOKE, and DROP targets, inherited grants, immediate prepared-plan invalidation after revocation, and the distinction between dynamically resolved invoker or definer bodies and exact bindings stored in views, generated expressions, and SQL-standard routine bodies.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/routine_schema_usage_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/routine_schema_usage_oracle.expected.txt -
```

`relation_schema_usage_oracle.sql` records PostgreSQL 18.4 schema `USAGE` enforcement at relation name-resolution boundaries. The checked-in transcript verifies qualified and effective-search-path lookup, missing-relation and column-error precedence, DML, TRUNCATE, ALTER, DROP, trigger and rule targets, constraint-trigger references, rule-action mutation targets, hierarchy parents, foreign-key references, hard and soft `regclass` input, inherited grants, immediate prepared-plan invalidation, security-context-aware string SQL, and namespace-check-free identities stored in views, materialized views, SQL-standard query bodies, declared cursors, and rule actions.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/relation_schema_usage_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/relation_schema_usage_oracle.expected.txt -
```

`index_namespace_oracle.sql` records PostgreSQL 18.4 index identity and shared relation-namespace behavior. The checked-in transcript verifies equal local index names in distinct schemas, distinct `regclass` OIDs, quoted component boundaries, default-name collision suffixes, table/index collisions, `IF NOT EXISTS`, effective-search-path `DROP INDEX`, wrong-kind precedence, missing schemas and indexes, and schema `USAGE` filtering.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/index_namespace_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/index_namespace_oracle.expected.txt -
```

`index_ownership_oracle.sql` records PostgreSQL 18.4 standalone-index ownership. The checked-in transcript verifies schema `USAGE`, table ownership, schema `CREATE`, and definition-error ordering for creation; inherited table-owner and containing-schema-owner drop authority; `IF EXISTS`; atomic multi-target preflight; table-owner transfer; transaction rollback; and exact `pg_class.relowner` projection.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/index_ownership_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/index_ownership_oracle.expected.txt -
```

`trigger_privilege_oracle.sql` records PostgreSQL 18.4 table and view trigger authorization. The checked-in transcript verifies target `TRIGGER` before function lookup, trigger-function `EXECUTE` before return-type and duplicate-name validation, inherited owner authority, CREATE OR REPLACE, relation-owner-derived DROP and ALTER checks, missing-trigger `IF EXISTS`, creation-time-only function authorization, table-owner transfer, and exact SQLSTATEs and primary messages.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/trigger_privilege_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/trigger_privilege_oracle.expected.txt -
```

`rule_privilege_oracle.sql` records PostgreSQL 18.4 table and regular-view rewrite-rule authorization and action security. The checked-in transcript verifies owner-only creation and replacement, inherited ownership, DROP, rename, enable, missing-rule ordering, live owner transfer, action target and source relation privileges, direct sequence relation scans, invoker-authorized routine and sequence-function calls including action-target defaults, `INSERT DEFAULT VALUES` cardinality, invoker-visible `current_user`, and exact SQLSTATEs and primary messages.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/rule_privilege_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/rule_privilege_oracle.expected.txt -
```

`rule_condition_subquery_oracle.sql` records PostgreSQL 18.4 rewrite-rule condition subqueries. The checked-in transcript verifies constant, correlated, scalar, `IN`, external-relation, local-name-shadowing, and correlated-CTE forms; INSERT action-time relation state; UPDATE and DELETE OLD/NEW rows; creation-time SQLSTATEs; scalar cardinality and atomicity; stored relation binding; target-column rename deparsing; rule-owner relation privileges; and invoker-authorized routine calls.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/rule_condition_subquery_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/rule_condition_subquery_oracle.expected.txt -
```

`rule_dependency_oracle.sql` records PostgreSQL 18.4 rewrite-rule object dependencies. The checked-in transcript verifies creation-bound action-target, action-source, and condition-source relations, exact routine overload identities, search-path and later-overload stability, DROP RESTRICT and CASCADE behavior, multi-target failure atomicity, transaction rollback, table and sequence rename, and durable catalog restore.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/rule_dependency_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/rule_dependency_oracle.expected.txt -
```

`rule_column_dependency_oracle.sql` records PostgreSQL 18.4 rewrite-rule column dependencies. The checked-in transcript verifies source projections and predicates, action target columns, positional INSERT targets, stable ownership of initially unqualified source columns, creation-bound projection stars, live source whole-row composites, one-sided and two-sided `JOIN USING` renames, one-sided `NATURAL JOIN` rename, catalog deparsing through range-table column aliases, unreferenced-column drops, rename execution, DROP COLUMN RESTRICT and CASCADE behavior, and rule removal.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/rule_column_dependency_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/rule_column_dependency_oracle.expected.txt -
```

`rule_row_expansion_oracle.sql` records PostgreSQL 18.4 rewrite-rule action-row expansion. The checked-in transcript verifies multi-row `OLD.*` and `NEW.*` expansion in `VALUES` rows, `SELECT` target lists, and `ROW` constructors; event-side SQLSTATEs; nested local-alias shadowing; CTE and set-operation scope rejection; creation-time stability across added columns; rename and drop dependencies; and cascade cleanup.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/rule_row_expansion_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/rule_row_expansion_oracle.expected.txt -
```

`rule_returning_event_row_oracle.sql` records PostgreSQL 18.4 rewrite-rule action `RETURNING` event-row behavior. The checked-in transcript verifies INSERT action-image versus UPDATE and DELETE event-row namespaces, explicit action-image aliases, `OLD.*` and `NEW.*` expansion, `RETURNING`-only set-oriented cardinality, event-side and column errors, target-alias ambiguity, inaccessible INSERT event rows, rename and drop dependencies, added-column `XX000` behavior, and statement atomicity.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/rule_returning_event_row_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/rule_returning_event_row_oracle.expected.txt -
```

`rule_whole_row_oracle.sql` records PostgreSQL 18.4 scalar rewrite-rule whole-row composites and action-target `RETURNING` stars. The checked-in transcript verifies live event-row shape across add, rename, and drop, whole-row conditions, exact invalid-side SQLSTATEs, local relation, column, derived-table, and CTE shadowing, command-specific event versus action row images, creation-bound target stars and namespaces, target-column dependencies, and exclusion of system attributes from composite row values while retaining their individual visibility.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/rule_whole_row_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/rule_whole_row_oracle.expected.txt -
```

`rule_notify_oracle.sql` records PostgreSQL 18.4 rewrite-rule `NOTIFY` actions and asynchronous transaction behavior. The checked-in transcript verifies zero-row and multi-row statement cardinality, unconditional `ALSO` and `INSTEAD`, durable deparsing, final subscription state, transaction-wide duplicate collapse, rollback and savepoint rollback, conditional-rule SQLSTATE `42P17`, channel and payload byte boundaries, NULL handling, notification inquiry functions, backend process identifiers, exact `pg_proc` identities, and the non-null `void` result boundary.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/rule_notify_oracle.sql 2>&1 | sed -E 's/PID [0-9]+/PID <pid>/' | diff -u tests/parity/pg18/rule_notify_oracle.expected.txt -
```

`foreign_table_trigger_oracle.sql` records PostgreSQL 18.4 foreign-table trigger definition and lifecycle behavior. The checked-in transcript verifies ordinary row and statement forms, replacement, `UPDATE OF`, `WHEN`, `TRUNCATE`, invalid constraint, transition, and `INSTEAD OF` forms, target and function authorization, `pg_trigger`, `relhastriggers`, enable, rename, drop, owner transfer, rollback, function dependencies, and relation-drop cleanup.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/foreign_table_trigger_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/foreign_table_trigger_oracle.expected.txt -
```

`table_ownership_oracle.sql` records PostgreSQL 18.4 ordinary-table owner creation, inherited owner authority, ALTER and DROP checks, containing-schema owner DROP authority, target-role existence, SET and schema-CREATE requirements, superuser transfer, serial-sequence propagation, transaction rollback, catalog projection, and dependent-role errors.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/table_ownership_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/table_ownership_oracle.expected.txt -
```

`view_ownership_oracle.sql` records PostgreSQL 18.4 regular-view and materialized-view owner creation, inherited owner authority, ALTER, replacement, refresh, and direct DROP checks including dependency-error precedence, containing-schema owner DROP authority, ownership-independent cascading removal, target-role existence, SET and schema-CREATE requirements, superuser transfer, transaction rollback, catalog projection, and dependent-role errors.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/view_ownership_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/view_ownership_oracle.expected.txt -
```

`foreign_table_ownership_oracle.sql` records PostgreSQL 18.4 foreign-table owner creation, inherited owner authority, direct and historical ALTER spellings, containing-schema owner DROP authority, target-role existence, SET and schema-CREATE requirements, target foreign-server privilege independence, superuser transfer, transaction rollback, catalog projection, and dependent-role errors.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/foreign_table_ownership_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/foreign_table_ownership_oracle.expected.txt -
```

`foreign_table_privilege_oracle.sql` records PostgreSQL 18.4 foreign-table relation and column ACL behavior. The checked-in transcript verifies NULL defaults, exact ACL text and privilege ordering, all-table and column grants, rooted grant-option dependencies, `RESTRICT` and `CASCADE`, `ALL TABLES IN SCHEMA`, owner-transfer rewriting, role dependencies, name/OID inquiry, direct SELECT enforcement, exact-column and `count(*)` authorization, and information-schema visibility and read-only metadata.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/foreign_table_privilege_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/foreign_table_privilege_oracle.expected.txt -
```

`view_privilege_oracle.sql` records PostgreSQL 18.4 regular-view and materialized-view ACL and enforcement behavior. The checked-in transcript verifies NULL defaults, exact relation and column ACL text, rooted grant-option revocation, all-in-schema targeting, name/OID privilege inquiry, information-schema visibility, regular-view definer and security-invoker authorization, automatically updatable DML, delegated materialized-view `MAINTAIN`, refresh owner context, unpopulated and DML error precedence, owner-transfer grantor rewriting, and role dependencies.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/view_privilege_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/view_privilege_oracle.expected.txt -
```

`table_privilege_oracle.sql` records PostgreSQL 18.4 ordinary-table ACL and enforcement behavior. The checked-in transcript verifies the NULL default ACL, `arwdDxtm` catalog order, target-read-sensitive DML checks, rooted grant-option revocation, implicit owner rights after self-revocation, explicit sequence targets, all-in-schema sequence exclusion, all six `has_table_privilege` overloads, information-schema visibility, exact error precedence, and ownership-transfer grantor rewriting.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/table_privilege_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/table_privilege_oracle.expected.txt -
```

`column_privilege_oracle.sql` records PostgreSQL 18.4 ordinary-table column ACL and enforcement behavior. The checked-in transcript verifies NULL and explicitly empty `attacl` state, exact `SELECT`, `INSERT`, `UPDATE`, and `REFERENCES` column requirements, table-to-column privilege implication, independent grant-option chains, system-column and attnum boundaries, sequence columns, all twelve `has_column_privilege` overloads, information-schema visibility, and ownership-transfer grantor rewriting.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/column_privilege_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/column_privilege_oracle.expected.txt -
```

`sequence_security_oracle.sql` records PostgreSQL 18.4 sequence role ownership and ACL behavior. The checked-in transcript verifies the default ACL and owner, an independent grant-option chain, all six `has_sequence_privilege` name/OID overloads, value-function access, grant-option `CASCADE`, implicit owner grant options after self-revocation, and ownership-transfer grantor rewriting.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/sequence_security_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/sequence_security_oracle.expected.txt -
```

`sequence_schema_security_oracle.sql` records PostgreSQL 18.4 schema ownership and ACL behavior at sequence boundaries. The checked-in transcript verifies `CREATE` and `USAGE` requirements, definition-error and missing-schema precedence, dependent grant-option `RESTRICT` and `CASCADE`, new-owner and target-schema `CREATE`, owned-sequence rejection and same-owner or same-schema no-op precedence, and `pg_namespace` owner and ACL output.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/sequence_schema_security_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/sequence_schema_security_oracle.expected.txt -
```

`sequence_information_schema_oracle.sql` records PostgreSQL 18.4 `information_schema.sequences` metadata and visibility. The checked-in transcript verifies current-session temporary namespace filtering in both sequence views, declared type and numeric precision, sequence options, exclusion of identity-owned internal sequences, explicit sequence privileges, and inherited-owner visibility.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/sequence_information_schema_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/sequence_information_schema_oracle.expected.txt -
```

`sequence_introspection_oracle.sql` records PostgreSQL 18.4 physical sequence tuples and built-in introspection behavior. The checked-in transcript verifies `pg_class.relnatts`, the three positive-numbered sequence attributes in `pg_attribute`, `last_value`, `log_cnt`, and `is_called`, bounded cached reservations, `pg_sequence_parameters`, `pg_get_sequence_data`, `pg_sequence_last_value`, each function's distinct `SELECT`, `UPDATE`, and `USAGE` visibility rule, `pg_sequences.last_value`, and exact `pg_proc` identities.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/sequence_introspection_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/sequence_introspection_oracle.expected.txt -
```

## Transaction catalog visibility oracle

`transaction_catalog_visibility_oracle.sql` uses `dblink` to commit catalog and storage changes from a sibling PostgreSQL 18.4 session after a `REPEATABLE READ` snapshot is established. The checked-in transcript verifies that the fixed snapshot uses current committed view and routine definitions, resolves a relation created after snapshot acquisition with no snapshot-visible rows, and treats a concurrently truncated relation as empty. The paired `pg18_fixed_snapshot_*` Rust integration tests exercise the same catalog identities and row-visibility boundaries in UQA, including transaction-local DDL and rollback.

```sh
docker exec -i uqa-pg18-age psql -U postgres -d postgres -X -qAt -f - < tests/parity/pg18/transaction_catalog_visibility_oracle.sql 2>/dev/null | diff -u tests/parity/pg18/transaction_catalog_visibility_oracle.expected.txt -
cargo test --locked -p uqa-engine --test integration pg18_fixed_snapshot
```

The PostgreSQL side keeps one generated schema across case-specific `psql` connections. The UQA side keeps one temporary database file and deliberately reopens it for every case, so the same comparison also verifies durable routine, view, generated-column, catalog, and ALTER state. Successful observation cases use COPY text rows; type-sensitive cases project `pg_typeof(...)`; expected failures compare SQLSTATE exactly.

Build the pinned PostgreSQL 18.4 and Apache AGE 1.8.0 oracle from AGE commit `b570cf7c1486863f77c14e9c0e07b0e9bfd01bf4`; `Dockerfile.pg18-age` also pins the PostgreSQL multi-platform image digest used for the checked-in transcript:

```sh
repo_root=$(git rev-parse --show-toplevel)
oracle_source=$(mktemp -d)
git -C "$oracle_source" init
git -C "$oracle_source" remote add origin https://github.com/apache/age.git
git -C "$oracle_source" fetch --depth=1 origin b570cf7c1486863f77c14e9c0e07b0e9bfd01bf4
git -C "$oracle_source" checkout --detach FETCH_HEAD
docker build --file "$repo_root/tests/parity/pg18/Dockerfile.pg18-age" --tag uqa-pg18-age:1.8.0 "$oracle_source"
docker run -d --name uqa-pg18-age -e POSTGRES_PASSWORD=uqa -e POSTGRES_DB=postgres uqa-pg18-age:1.8.0
```

Build the current CLI before running the oracle:

```sh
cargo build --release -p uqa-cli --bin usql
python3 tests/parity/pg18/run_routines_stateful.py
python3 tests/parity/pg18/run_routines_stateful.py --suite roles
python3 tests/parity/pg18/run_routines_stateful.py --suite constraints
python3 tests/parity/pg18/run_routines_stateful.py --suite type-temporal
python3 tests/parity/pg18/run_routines_stateful.py --suite triggers
python3 tests/parity/pg18/run_routines_stateful.py --suite rules
python3 tests/parity/pg18/run_routines_stateful.py --suite transactions
```

The runner executes PostgreSQL and UQA concurrently by default. `--backend postgres` and `--backend uqa` select one side for diagnosis. Canonical transcript updates require the PostgreSQL-only backend and use an atomic file replacement; regenerate only from the pinned PostgreSQL 18.4 + AGE oracle, then review the checked-in JSON diff:

```sh
python3 tests/parity/pg18/run_routines_stateful.py --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite roles --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite constraints --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite type-temporal --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite triggers --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite rules --backend postgres --update-expected
python3 tests/parity/pg18/run_routines_stateful.py --suite transactions --backend postgres --update-expected
```

Every fixture case starts with `-- @case <name> <ok|rows|error>` and ends with `-- @end`; this explicit framing allows routine bodies to contain semicolons without making the runner guess SQL statement boundaries. The runner replaces `__UQA_STATEFUL_SCHEMA__` and the role-suite placeholders with isolated generated names and rejects an expected transcript whose fixture SHA-256 or ordered case modes are stale.

## Routine security and cursor oracle

[`routine_security_cursor_oracle.md`](routine_security_cursor_oracle.md) records the PostgreSQL 18.4 with Apache AGE owner, EXECUTE ACL, `SECURITY DEFINER`, dynamic `current_user` versus stable `session_user`, routine configuration, planner-support metadata, `refcursor` type identity, and cross-call session-portal results used by the focused compiler and engine tests.

## Protocol client matrix

`clients/run.sh` builds pinned psycopg, pgx, and node-postgres images, provisions a password-authenticated role in a running PostgreSQL 18.4 container, checks the deterministic operation/version evidence from each driver against `clients/expected.json`, and reruns the same operations against the server fixture assembled from `uqa-pg-wire`. The matrix covers prepared reuse, binary formats, COPY in and out, failed-transaction rollback recovery, and one-connection pool reuse; it also runs the existing PostgreSQL 18 psql/libpq protocol 3.0/3.2 suite.

The default container name is `pg-parity`, the default published PostgreSQL port is `15432`, and the runner uses the Docker runtime's host-gateway alias. Override `UQA_PG18_WIRE_CONTAINER`, `UQA_PG18_ORACLE_PORT`, or `UQA_PG18_DOCKER_HOST` for another local runtime, then run:

```sh
bash tests/parity/pg18/clients/run.sh
```
