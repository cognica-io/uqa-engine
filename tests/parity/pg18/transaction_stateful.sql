-- Stateful PostgreSQL 18.4 transaction, cursor, and maintenance parity fixture.
-- The runner replaces __UQA_STATEFUL_SCHEMA__ and executes each delimited case in order.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- @case catalog_session_views rows
SELECT namespace.nspname = current_schema() AS namespace_matches,
       replace(setting.setting, ' ', '') = current_schema() || ',pg_catalog' AS search_path_matches
FROM pg_catalog.pg_namespace AS namespace
CROSS JOIN pg_catalog.pg_settings AS setting
WHERE namespace.nspname = current_schema() AND setting.name = 'search_path';
-- @end

-- @case schema_create_rollback ok
BEGIN;
CREATE SCHEMA __UQA_SCHEMA_PROBE__;
ROLLBACK;
-- @end

-- @case schema_create_rollback_result rows
SELECT count(*) FROM pg_catalog.pg_namespace WHERE nspname = '__UQA_SCHEMA_PROBE__';
-- @end

-- @case schema_create_commit ok
BEGIN;
CREATE SCHEMA __UQA_SCHEMA_PROBE__;
COMMIT;
-- @end

-- @case schema_create_commit_result rows
SELECT count(*) FROM pg_catalog.pg_namespace WHERE nspname = '__UQA_SCHEMA_PROBE__';
-- @end

-- @case schema_create_commit_cleanup ok
DROP SCHEMA __UQA_SCHEMA_PROBE__;
-- @end

-- @case create_cursor_fixture ok
CREATE TABLE cursor_source(id integer PRIMARY KEY);
INSERT INTO cursor_source VALUES (1);
CREATE TABLE cursor_divisors(divisor integer NOT NULL);
INSERT INTO cursor_divisors VALUES (0);
CREATE TABLE cursor_observations(name text PRIMARY KEY, observed bigint NOT NULL);
CREATE SEQUENCE incremental_cursor_sequence START WITH 1;
CREATE SEQUENCE snapshot_cursor_sequence START WITH 1;
CREATE SEQUENCE hold_cursor_sequence START WITH 1;
CREATE TABLE own_cursor_source(id integer PRIMARY KEY, value integer NOT NULL);
INSERT INTO own_cursor_source VALUES (1, 10);
CREATE TABLE held_own_cursor_source(id integer PRIMARY KEY, value integer NOT NULL);
INSERT INTO held_own_cursor_source VALUES (1, 10);
CREATE SEQUENCE own_cursor_sequence START WITH 1;
CREATE SEQUENCE held_own_cursor_sequence START WITH 1;
CREATE SEQUENCE cursor_rename_sequence START WITH 1;
CREATE SEQUENCE cursor_hierarchy_sequence START WITH 1;
CREATE SEQUENCE cursor_view_sequence START WITH 1;
CREATE SEQUENCE cursor_path_sequence START WITH 1;
CREATE SEQUENCE cursor_regclass_sequence START WITH 1;
CREATE SEQUENCE cursor_function_sequence START WITH 1;
CREATE SEQUENCE catalog_cursor_sequence START WITH 1;
CREATE TABLE catalog_cursor_a(id integer);
CREATE FUNCTION cursor_plan_function() RETURNS bigint LANGUAGE SQL VOLATILE AS 'SELECT nextval(''cursor_function_sequence'')';
CREATE TABLE cursor_rename_source(id integer PRIMARY KEY);
INSERT INTO cursor_rename_source VALUES (7);
CREATE TABLE cursor_parent(id integer);
CREATE TABLE cursor_child(id integer);
INSERT INTO cursor_child VALUES (9);
CREATE TABLE cursor_view_old_source(id integer PRIMARY KEY);
INSERT INTO cursor_view_old_source VALUES (11);
CREATE TABLE cursor_view_new_source(id integer PRIMARY KEY);
INSERT INTO cursor_view_new_source VALUES (22), (23);
CREATE VIEW cursor_inner_view AS SELECT id FROM cursor_view_old_source;
CREATE VIEW cursor_outer_view AS SELECT id FROM cursor_inner_view;
CREATE VIEW cursor_path_view AS SELECT id FROM cursor_view_old_source;
CREATE TABLE fixed_snapshot_source(id integer PRIMARY KEY);
INSERT INTO fixed_snapshot_source VALUES (1), (2);
CREATE TABLE fixed_rename_source(id integer PRIMARY KEY, value integer NOT NULL);
INSERT INTO fixed_rename_source VALUES (1, 10);
CREATE TABLE relation_oid_source(id integer);
CREATE TABLE relation_oid_observation(original oid NOT NULL);
INSERT INTO relation_oid_observation VALUES ('relation_oid_source'::regclass);
CREATE TABLE cursor_routine_rows(id integer PRIMARY KEY);
CREATE FUNCTION cursor_insert_and_count(value integer) RETURNS integer LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    observed integer;
BEGIN
    INSERT INTO cursor_routine_rows VALUES (value);
    SELECT count(*) INTO observed FROM cursor_routine_rows;
    INSERT INTO cursor_observations VALUES ('routine_' || value, observed);
    RETURN observed;
END
$$;
-- @end

-- DECLARE performs relation and column analysis without evaluating query rows.
-- @case cursor_declare_rejects_missing_relation error
BEGIN;
DECLARE missing_relation_cursor CURSOR FOR SELECT * FROM absent_cursor_relation;
-- @end

-- @case cursor_declare_rejects_missing_column error
BEGIN;
DECLARE missing_column_cursor CURSOR FOR SELECT absent_column FROM cursor_rename_source;
-- @end

-- A virtual catalog relation captures the same relation catalog that was visible to DECLARE, including tables that are not direct query dependencies.
-- @case catalog_cursor_keeps_declare_time_relations ok
INSERT INTO cursor_observations VALUES ('catalog_cursor_seed', nextval('catalog_cursor_sequence'));
BEGIN;
DECLARE catalog_relation_cursor CURSOR FOR SELECT nextval('catalog_cursor_sequence') AS value FROM pg_catalog.pg_class WHERE relname IN ('catalog_cursor_a', 'catalog_cursor_b') ORDER BY relname;
CREATE TABLE catalog_cursor_b(id integer);
DROP TABLE catalog_cursor_a;
MOVE FORWARD ALL FROM catalog_relation_cursor;
INSERT INTO cursor_observations VALUES ('catalog_cursor_rows', currval('catalog_cursor_sequence'));
COMMIT;
-- @end

-- @case catalog_cursor_declare_time_result rows
SELECT observed FROM cursor_observations WHERE name = 'catalog_cursor_rows';
-- @end

-- A cursor stays bound to the relation identity resolved by DECLARE when that table is renamed later in the transaction.
-- @case cursor_keeps_relation_binding_after_rename ok
INSERT INTO cursor_observations VALUES ('binding_rename_seed', nextval('cursor_rename_sequence'));
BEGIN;
DECLARE rename_binding_cursor CURSOR FOR SELECT nextval('cursor_rename_sequence') AS value FROM cursor_rename_source;
ALTER TABLE cursor_rename_source RENAME TO cursor_renamed_source;
MOVE FORWARD ALL FROM rename_binding_cursor;
INSERT INTO cursor_observations VALUES ('binding_rename_rows', currval('cursor_rename_sequence'));
COMMIT;
-- @end

-- The inherited descendant set is also frozen at DECLARE rather than recomputed at FETCH.
-- @case cursor_keeps_declare_time_hierarchy ok
INSERT INTO cursor_observations VALUES ('binding_hierarchy_seed', nextval('cursor_hierarchy_sequence'));
BEGIN;
DECLARE hierarchy_binding_cursor CURSOR FOR SELECT nextval('cursor_hierarchy_sequence') AS value FROM cursor_parent;
ALTER TABLE cursor_child INHERIT cursor_parent;
MOVE FORWARD ALL FROM hierarchy_binding_cursor;
INSERT INTO cursor_observations VALUES ('binding_hierarchy_rows', currval('cursor_hierarchy_sequence'));
COMMIT;
-- @end

-- @case cursor_relation_binding_results rows
SELECT name, observed FROM cursor_observations WHERE name IN ('binding_rename_rows', 'binding_hierarchy_rows') ORDER BY name;
-- @end

-- Nested view definitions are part of the DECLARE-time cursor plan and do not follow CREATE OR REPLACE VIEW before FETCH.
-- @case cursor_keeps_declare_time_view_plan ok
INSERT INTO cursor_observations VALUES ('binding_view_seed', nextval('cursor_view_sequence'));
BEGIN;
DECLARE view_binding_cursor CURSOR FOR SELECT nextval('cursor_view_sequence') AS value FROM cursor_outer_view;
CREATE OR REPLACE VIEW cursor_inner_view AS SELECT id FROM cursor_view_new_source;
CREATE OR REPLACE VIEW cursor_outer_view AS SELECT id FROM cursor_view_new_source;
MOVE FORWARD ALL FROM view_binding_cursor;
INSERT INTO cursor_observations VALUES ('binding_view_rows', currval('cursor_view_sequence'));
COMMIT;
-- @end

-- @case cursor_view_plan_result rows
SELECT observed FROM cursor_observations WHERE name = 'binding_view_rows';
-- @end

-- An unqualified view name is resolved by DECLARE and does not follow a later search_path change.
-- @case cursor_keeps_declare_time_view_name_binding ok
SET search_path = __UQA_STATEFUL_SCHEMA__, pg_catalog;
INSERT INTO cursor_observations VALUES ('binding_view_path_seed', nextval('cursor_path_sequence'));
BEGIN;
DECLARE view_path_cursor CURSOR FOR SELECT nextval('__UQA_STATEFUL_SCHEMA__.cursor_path_sequence') AS value FROM cursor_path_view;
SET search_path = pg_catalog;
MOVE FORWARD ALL FROM view_path_cursor;
SET search_path = __UQA_STATEFUL_SCHEMA__, pg_catalog;
INSERT INTO cursor_observations VALUES ('binding_view_path_rows', currval('cursor_path_sequence'));
COMMIT;
-- @end

-- @case cursor_view_name_binding_result rows
SELECT observed FROM cursor_observations WHERE name = 'binding_view_path_rows';
-- @end

-- Literal regclass sequence arguments are resolved at DECLARE rather than reinterpreted through the FETCH-time search_path.
-- @case cursor_keeps_declare_time_regclass_binding ok
SET search_path = __UQA_STATEFUL_SCHEMA__, pg_catalog;
INSERT INTO cursor_observations VALUES ('binding_regclass_seed', nextval('cursor_regclass_sequence'));
BEGIN;
DECLARE regclass_binding_cursor CURSOR FOR SELECT nextval('cursor_regclass_sequence') AS value;
SET search_path = pg_catalog;
MOVE FORWARD ALL FROM regclass_binding_cursor;
SET search_path = __UQA_STATEFUL_SCHEMA__, pg_catalog;
INSERT INTO cursor_observations VALUES ('binding_regclass_rows', currval('cursor_regclass_sequence'));
COMMIT;
-- @end

-- @case cursor_regclass_binding_result rows
SELECT observed FROM cursor_observations WHERE name = 'binding_regclass_rows';
-- @end

-- A cursor retains the routine definition selected at DECLARE even when CREATE OR REPLACE FUNCTION publishes a new body before FETCH.
-- @case cursor_keeps_declare_time_function_plan ok
INSERT INTO cursor_observations VALUES ('binding_function_seed', nextval('cursor_function_sequence'));
BEGIN;
DECLARE function_binding_cursor CURSOR FOR SELECT cursor_plan_function() AS value;
CREATE OR REPLACE FUNCTION cursor_plan_function() RETURNS bigint LANGUAGE SQL VOLATILE AS 'SELECT nextval(''cursor_function_sequence'') + nextval(''cursor_function_sequence'')';
MOVE FORWARD ALL FROM function_binding_cursor;
INSERT INTO cursor_observations VALUES ('binding_function_rows', currval('cursor_function_sequence'));
COMMIT;
-- @end

-- @case cursor_function_plan_result rows
SELECT observed FROM cursor_observations WHERE name = 'binding_function_rows';
-- @end

-- A fixed transaction snapshot follows the original relation through DDL, but a DROP plus same-name CREATE introduces a new relation lifetime rather than overlaying the old snapshot rows.
-- @case fixed_snapshot_uses_recreated_relation_lifetime ok
BEGIN ISOLATION LEVEL REPEATABLE READ;
INSERT INTO cursor_observations SELECT 'fixed_snapshot_before_recreate', count(*) FROM fixed_snapshot_source;
DROP TABLE fixed_snapshot_source;
CREATE TABLE fixed_snapshot_source(id integer PRIMARY KEY);
INSERT INTO fixed_snapshot_source VALUES (99);
INSERT INTO cursor_observations SELECT 'fixed_snapshot_after_recreate', sum(id) FROM fixed_snapshot_source;
COMMIT;
-- @end

-- @case fixed_snapshot_recreated_relation_result rows
SELECT name, observed FROM cursor_observations WHERE name LIKE 'fixed_snapshot_%' ORDER BY name;
-- @end

-- A transaction's own row changes remain attached to the same relation when ALTER TABLE renames it after the fixed snapshot is acquired.
-- @case fixed_snapshot_own_change_survives_rename ok
BEGIN ISOLATION LEVEL REPEATABLE READ;
INSERT INTO cursor_observations SELECT 'fixed_rename_snapshot', count(*) FROM fixed_rename_source;
UPDATE fixed_rename_source SET value = 20 WHERE id = 1;
ALTER TABLE fixed_rename_source RENAME TO fixed_renamed_source;
INSERT INTO cursor_observations SELECT 'fixed_rename_value', value FROM fixed_renamed_source;
COMMIT;
-- @end

-- @case fixed_snapshot_own_change_after_rename_result rows
SELECT observed FROM cursor_observations WHERE name = 'fixed_rename_value';
-- @end

-- A table keeps its catalog object identity across rename and TRUNCATE, while DROP plus CREATE allocates a new identity.
-- @case relation_oid_survives_rename_and_truncate ok
ALTER TABLE relation_oid_source RENAME TO relation_oid_renamed;
TRUNCATE relation_oid_renamed;
-- @end

-- @case relation_oid_after_rename_and_truncate_result rows
SELECT original = 'relation_oid_renamed'::regclass AS identity_preserved FROM relation_oid_observation;
-- @end

-- @case relation_oid_changes_after_recreate ok
DROP TABLE relation_oid_renamed;
CREATE TABLE relation_oid_renamed(id integer);
-- @end

-- @case relation_oid_after_recreate_result rows
SELECT original <> 'relation_oid_renamed'::regclass AS identity_replaced FROM relation_oid_observation;
-- @end

-- DECLARE does not execute the query, while the first movement exposes a deferred runtime error.
-- @case declare_cursor_is_lazy ok
BEGIN;
DECLARE lazy_cursor CURSOR FOR SELECT 1 / divisor AS value FROM cursor_divisors;
CLOSE lazy_cursor;
COMMIT;
-- @end

-- @case first_cursor_movement_executes_query error
BEGIN;
DECLARE lazy_cursor CURSOR FOR SELECT 1 / divisor AS value FROM cursor_divisors;
MOVE FORWARD 1 FROM lazy_cursor;
-- @end

-- A runtime error while COMMIT materializes a holdable cursor aborts and closes the transaction.
-- @case holdable_materialization_failure_ends_transaction error
BEGIN;
DECLARE failing_held_cursor CURSOR WITH HOLD FOR SELECT 1 / divisor AS value FROM cursor_divisors;
COMMIT;
-- @end

-- @case session_after_holdable_materialization_failure rows
SELECT 42 AS value;
-- @end

-- Zero movement must not evaluate a volatile projection, and moving one row must not drain the query.
-- @case cursor_execution_is_incremental ok
INSERT INTO cursor_observations VALUES ('incremental_seed', nextval('incremental_cursor_sequence'));
BEGIN;
DECLARE incremental_cursor CURSOR FOR SELECT nextval('incremental_cursor_sequence') AS value FROM generate_series(1, 3);
MOVE FORWARD 0 FROM incremental_cursor;
MOVE FORWARD 1 FROM incremental_cursor;
INSERT INTO cursor_observations VALUES ('incremental_after_one', currval('incremental_cursor_sequence'));
CLOSE incremental_cursor;
COMMIT;
-- @end

-- @case cursor_execution_is_incremental_result rows
SELECT name, observed FROM cursor_observations WHERE name LIKE 'incremental_%' ORDER BY name;
-- @end

-- A READ COMMITTED cursor keeps the snapshot captured by DECLARE even after the declaring transaction writes.
-- @case declare_captures_cursor_snapshot ok
BEGIN;
DECLARE snapshot_cursor CURSOR FOR SELECT nextval('snapshot_cursor_sequence') AS value FROM cursor_source ORDER BY id;
INSERT INTO cursor_source VALUES (2);
MOVE FORWARD ALL FROM snapshot_cursor;
INSERT INTO cursor_observations VALUES ('declare_snapshot_rows', currval('snapshot_cursor_sequence'));
CLOSE snapshot_cursor;
COMMIT;
-- @end

-- @case declare_captures_cursor_snapshot_result rows
SELECT observed FROM cursor_observations WHERE name = 'declare_snapshot_rows';
-- @end

-- A cursor snapshot includes this transaction's INSERT and UPDATE as of DECLARE, then excludes its later UPDATE, DELETE, and INSERT.
-- @case declare_snapshot_freezes_own_changes ok
INSERT INTO cursor_observations VALUES ('own_snapshot_seed', nextval('own_cursor_sequence'));
BEGIN;
UPDATE own_cursor_source SET value = 20 WHERE id = 1;
INSERT INTO own_cursor_source VALUES (2, 20);
DECLARE own_snapshot_cursor CURSOR FOR SELECT nextval('own_cursor_sequence') AS value FROM own_cursor_source WHERE value = 20 ORDER BY id;
UPDATE own_cursor_source SET value = 30 WHERE id = 1;
DELETE FROM own_cursor_source WHERE id = 2;
INSERT INTO own_cursor_source VALUES (3, 20);
MOVE FORWARD ALL FROM own_snapshot_cursor;
COMMIT;
INSERT INTO cursor_observations VALUES ('own_snapshot_rows', currval('own_cursor_sequence'));
-- @end

-- @case declare_snapshot_freezes_own_changes_result rows
SELECT observed FROM cursor_observations WHERE name = 'own_snapshot_rows';
-- @end

-- Volatile routines evaluated by a cursor observe writes made by earlier rows from that cursor execution.
-- @case cursor_volatile_routine_observes_earlier_writes ok
BEGIN;
DECLARE mutating_cursor CURSOR FOR SELECT cursor_insert_and_count(value) AS observed FROM generate_series(1, 2) AS values(value);
MOVE FORWARD ALL FROM mutating_cursor;
COMMIT;
-- @end

-- @case cursor_volatile_routine_write_visibility_result rows
SELECT name, observed FROM cursor_observations WHERE name LIKE 'routine_%' ORDER BY name;
-- @end

-- WITH HOLD reexecution at COMMIT uses the same DECLARE-time own-change image and restores the logical cursor position.
-- @case holdable_snapshot_freezes_own_changes ok
INSERT INTO cursor_observations VALUES ('held_own_snapshot_seed', nextval('held_own_cursor_sequence'));
BEGIN;
UPDATE held_own_cursor_source SET value = 20 WHERE id = 1;
INSERT INTO held_own_cursor_source VALUES (2, 20);
DECLARE held_own_snapshot_cursor CURSOR WITH HOLD FOR SELECT nextval('held_own_cursor_sequence') AS value FROM held_own_cursor_source WHERE value = 20 ORDER BY id;
MOVE FORWARD 1 FROM held_own_snapshot_cursor;
UPDATE held_own_cursor_source SET value = 30 WHERE id = 1;
DELETE FROM held_own_cursor_source WHERE id = 2;
INSERT INTO held_own_cursor_source VALUES (3, 20);
COMMIT;
MOVE FORWARD ALL FROM held_own_snapshot_cursor;
INSERT INTO cursor_observations VALUES ('held_own_snapshot_rows', currval('held_own_cursor_sequence'));
CLOSE held_own_snapshot_cursor;
-- @end

-- @case holdable_snapshot_freezes_own_changes_result rows
SELECT observed FROM cursor_observations WHERE name = 'held_own_snapshot_rows';
-- @end

-- Commit materializes the unread portion of a holdable cursor once and preserves its position afterward.
-- @case holdable_cursor_materializes_at_commit ok
BEGIN;
DECLARE held_cursor CURSOR WITH HOLD FOR SELECT nextval('hold_cursor_sequence') AS value FROM generate_series(1, 3);
MOVE FORWARD 1 FROM held_cursor;
COMMIT;
MOVE FORWARD 1 FROM held_cursor;
INSERT INTO cursor_observations VALUES ('hold_materialized_rows', currval('hold_cursor_sequence'));
CLOSE held_cursor;
-- @end

-- @case holdable_cursor_materializes_at_commit_result rows
SELECT observed FROM cursor_observations WHERE name = 'hold_materialized_rows';
-- @end

-- Deferred constraints are revalidated after commit materializes a holdable cursor with a mutating projection.
-- @case create_holdable_constraint_fixture ok
CREATE TABLE held_parent(id integer PRIMARY KEY);
CREATE TABLE held_child(id integer PRIMARY KEY, parent_id integer, CONSTRAINT held_child_parent_fk FOREIGN KEY (parent_id) REFERENCES held_parent(id) DEFERRABLE INITIALLY DEFERRED);
CREATE FUNCTION insert_invalid_held_child() RETURNS integer LANGUAGE SQL VOLATILE AS 'INSERT INTO held_child VALUES (1, 999) RETURNING id';
-- @end

-- @case holdable_cursor_revalidates_deferred_constraints error
BEGIN;
DECLARE mutating_held_cursor CURSOR WITH HOLD FOR SELECT insert_invalid_held_child() AS id;
COMMIT;
-- @end

-- @case holdable_cursor_constraint_failure_rolls_back rows
SELECT count(*) AS child_rows FROM held_child;
-- @end

-- Targeted VACUUM FULL rewrites only the named relation and leaves it readable and writable.
-- @case create_vacuum_fixture ok
CREATE TABLE vacuum_target(id integer PRIMARY KEY, value text);
INSERT INTO vacuum_target VALUES (1, 'one'), (2, 'two');
CREATE TABLE vacuum_other(id integer PRIMARY KEY, value text);
INSERT INTO vacuum_other VALUES (9, 'nine');
-- @end

-- @case targeted_vacuum_full ok
VACUUM (FULL, ANALYZE) __UQA_STATEFUL_SCHEMA__.vacuum_target;
-- @end

-- @case targeted_vacuum_full_remains_writable ok
UPDATE vacuum_target SET value = 'updated' WHERE id = 2;
-- @end

-- @case targeted_vacuum_full_result rows
SELECT 'target' AS relation, id, value FROM vacuum_target UNION ALL SELECT 'other', id, value FROM vacuum_other ORDER BY relation, id;
-- @end

-- ANALYZE is allowed in an explicit read-only transaction and does not turn it into a writer transaction.
-- @case analyze_in_read_only_transaction ok
BEGIN READ ONLY;
ANALYZE vacuum_target;
COMMIT;
-- @end

-- PREPARE acquires the transaction's first snapshot even when the prepared query is constant-only.
-- @case prepare_sets_transaction_snapshot error
BEGIN;
PREPARE prepared_snapshot_probe AS SELECT 1;
SET TRANSACTION ISOLATION LEVEL SERIALIZABLE;
-- @end

-- ANALYZE remains nontransactional after the transaction has already written and then becomes read-only.
-- @case analyze_after_write_in_read_only_transaction ok
BEGIN;
INSERT INTO vacuum_target VALUES (3, 'rolled back');
SET TRANSACTION READ ONLY;
ANALYZE vacuum_target;
ROLLBACK;
-- @end

-- @case analyze_after_write_rollback_result rows
SELECT count(*) AS rows_after_rollback FROM vacuum_target;
-- @end

-- PostgreSQL exposes a non-volatile ADD COLUMN default through pg_attribute.attmissingval, while a volatile sequence default rewrites each existing row independently.
-- @case create_attribute_missing_value_fixture ok
CREATE SEQUENCE attribute_default_sequence START WITH 10;
CREATE TABLE attribute_defaults(id integer PRIMARY KEY);
INSERT INTO attribute_defaults VALUES (1), (2);
ALTER TABLE attribute_defaults ADD COLUMN fast_default integer DEFAULT 7;
ALTER TABLE attribute_defaults ADD COLUMN volatile_default bigint DEFAULT nextval('attribute_default_sequence');
-- @end

-- @case attribute_missing_value_catalog_result rows
SELECT attname, atthasmissing, attmissingval::text FROM pg_catalog.pg_attribute WHERE attrelid = 'attribute_defaults'::regclass AND attname IN ('fast_default', 'volatile_default') ORDER BY attname;
-- @end

-- @case volatile_added_column_default_result rows
SELECT id, volatile_default FROM attribute_defaults ORDER BY id;
-- @end
