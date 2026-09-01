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

-- Procedural transaction control is nonatomic only for a standalone top-level CALL/DO and an uninterrupted nested CALL/DO chain.
-- @case create_procedural_transaction_fixture ok
SET search_path = __UQA_STATEFUL_SCHEMA__, pg_catalog;
CREATE TABLE procedural_transaction_log(entry text NOT NULL);
CREATE TABLE procedural_loop_source(value integer PRIMARY KEY);
INSERT INTO procedural_loop_source VALUES (1), (2), (3), (4), (5), (6), (7), (8), (9), (10), (11), (12);
CREATE TABLE procedural_command_target(value integer);
CREATE PROCEDURE procedural_commit_continue() LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('commit-before');
    COMMIT;
    INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('commit-after');
END
$$;
CREATE PROCEDURE procedural_rollback_continue() LANGUAGE plpgsql AS $$
DECLARE
    local_value integer := 10;
BEGIN
    local_value := local_value + 1;
    INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('discard-' || local_value);
    ROLLBACK;
    local_value := local_value + 1;
    INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('keep-' || local_value);
END
$$;
CREATE PROCEDURE procedural_error_after_commit() LANGUAGE plpgsql AS $$
DECLARE
    quotient integer;
BEGIN
    INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('segment-committed');
    COMMIT;
    INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('segment-discarded');
    quotient := 1 / 0;
END
$$;
CREATE PROCEDURE procedural_inner() LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('inner-before');
    COMMIT;
    INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('inner-after');
END
$$;
CREATE PROCEDURE procedural_outer_direct() LANGUAGE plpgsql AS $$
BEGIN
    CALL __UQA_STATEFUL_SCHEMA__.procedural_inner();
    INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('outer-after');
END
$$;
CREATE FUNCTION procedural_bridge() RETURNS integer LANGUAGE plpgsql AS $$
BEGIN
    CALL __UQA_STATEFUL_SCHEMA__.procedural_inner();
    RETURN 1;
END
$$;
CREATE PROCEDURE procedural_outer_bridge() LANGUAGE plpgsql AS $$
BEGIN
    PERFORM __UQA_STATEFUL_SCHEMA__.procedural_bridge();
END
$$;
CREATE PROCEDURE procedural_select_loop() LANGUAGE plpgsql AS $$
DECLARE
    row_value record;
BEGIN
    FOR row_value IN SELECT value FROM __UQA_STATEFUL_SCHEMA__.procedural_loop_source ORDER BY value LOOP
        INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_transaction_log VALUES ('loop-' || row_value.value);
        COMMIT;
    END LOOP;
END
$$;
CREATE PROCEDURE procedural_command_loop() LANGUAGE plpgsql AS $$
DECLARE
    row_value record;
BEGIN
    FOR row_value IN INSERT INTO __UQA_STATEFUL_SCHEMA__.procedural_command_target SELECT value FROM __UQA_STATEFUL_SCHEMA__.procedural_loop_source RETURNING value LOOP
        COMMIT;
    END LOOP;
END
$$;
CREATE PROCEDURE procedural_dynamic_commit() LANGUAGE plpgsql AS $$
BEGIN
    EXECUTE 'COMMIT';
END
$$;
-- @end

-- @case procedural_commit_continue ok
CALL __UQA_STATEFUL_SCHEMA__.procedural_commit_continue();
-- @end

-- @case procedural_commit_continue_result rows
SELECT entry FROM __UQA_STATEFUL_SCHEMA__.procedural_transaction_log ORDER BY entry;
-- @end

-- @case procedural_transaction_log_reset ok
TRUNCATE __UQA_STATEFUL_SCHEMA__.procedural_transaction_log;
-- @end

-- @case procedural_rollback_continue ok
CALL __UQA_STATEFUL_SCHEMA__.procedural_rollback_continue();
-- @end

-- @case procedural_rollback_continue_result rows
SELECT entry FROM __UQA_STATEFUL_SCHEMA__.procedural_transaction_log;
-- @end

-- @case procedural_transaction_log_reset_after_rollback ok
TRUNCATE __UQA_STATEFUL_SCHEMA__.procedural_transaction_log;
-- @end

-- @case procedural_error_after_commit error
CALL __UQA_STATEFUL_SCHEMA__.procedural_error_after_commit();
-- @end

-- @case procedural_error_after_commit_result rows
SELECT entry FROM __UQA_STATEFUL_SCHEMA__.procedural_transaction_log ORDER BY entry;
-- @end

-- @case procedural_transaction_log_reset_after_error ok
TRUNCATE __UQA_STATEFUL_SCHEMA__.procedural_transaction_log;
-- @end

-- @case procedural_direct_nested_call ok
CALL __UQA_STATEFUL_SCHEMA__.procedural_outer_direct();
-- @end

-- @case procedural_direct_nested_call_result rows
SELECT entry FROM __UQA_STATEFUL_SCHEMA__.procedural_transaction_log ORDER BY entry;
-- @end

-- @case procedural_transaction_log_reset_after_nested_call ok
TRUNCATE __UQA_STATEFUL_SCHEMA__.procedural_transaction_log;
-- @end

-- @case procedural_function_bridge_rejected error
CALL __UQA_STATEFUL_SCHEMA__.procedural_outer_bridge();
-- @end

-- @case procedural_function_bridge_result rows
SELECT count(*) FROM __UQA_STATEFUL_SCHEMA__.procedural_transaction_log;
-- @end

-- @case procedural_select_loop ok
CALL __UQA_STATEFUL_SCHEMA__.procedural_select_loop();
-- @end

-- @case procedural_select_loop_result rows
SELECT count(*) FROM __UQA_STATEFUL_SCHEMA__.procedural_transaction_log;
-- @end

-- @case procedural_transaction_log_reset_after_loop ok
TRUNCATE __UQA_STATEFUL_SCHEMA__.procedural_transaction_log;
-- @end

-- @case procedural_command_loop_rejected error
CALL __UQA_STATEFUL_SCHEMA__.procedural_command_loop();
-- @end

-- @case procedural_command_loop_result rows
SELECT count(*) FROM __UQA_STATEFUL_SCHEMA__.procedural_command_target;
-- @end

-- @case procedural_dynamic_commit_rejected error
CALL __UQA_STATEFUL_SCHEMA__.procedural_dynamic_commit();
-- @end

-- @case procedural_simple_query_batch_rejected error
CALL __UQA_STATEFUL_SCHEMA__.procedural_commit_continue(); SELECT 1;
-- @end

-- @case procedural_simple_query_batch_result rows
SELECT count(*) FROM __UQA_STATEFUL_SCHEMA__.procedural_transaction_log;
-- @end

-- @case create_cursor_fixture ok
CREATE TABLE cursor_source(id integer PRIMARY KEY);
INSERT INTO cursor_source VALUES (1);
CREATE TABLE cursor_divisors(divisor integer NOT NULL);
INSERT INTO cursor_divisors VALUES (0);
CREATE TABLE cursor_observations(name text PRIMARY KEY, observed bigint NOT NULL);
CREATE SEQUENCE incremental_cursor_sequence START WITH 1;
CREATE SEQUENCE offset_cursor_sequence START WITH 1;
CREATE SEQUENCE directional_cursor_sequence START WITH 1;
CREATE SEQUENCE directional_filter_sequence START WITH 1;
CREATE SEQUENCE directional_projection_sequence START WITH 1;
CREATE SEQUENCE directional_union_sequence START WITH 1;
CREATE SEQUENCE directional_union_cte_sequence START WITH 1;
CREATE SEQUENCE directional_mixed_union_sequence START WITH 1;
CREATE SEQUENCE directional_result_union_sequence START WITH 1;
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
CREATE TABLE plpgsql_command_cursor_source(id integer PRIMARY KEY, value integer NOT NULL);
INSERT INTO plpgsql_command_cursor_source VALUES (1, 10), (2, 20);
CREATE TABLE plpgsql_cursor_call_log(value integer);
CREATE PROCEDURE plpgsql_cursor_call_out(IN input integer, OUT output integer) LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO plpgsql_cursor_call_log VALUES (input);
    output := input + 1;
END
$$;
CREATE PROCEDURE plpgsql_cursor_call_no_out(IN input integer) LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO plpgsql_cursor_call_log VALUES (input);
END
$$;
CREATE TABLE plpgsql_cursor_merge_target(id integer PRIMARY KEY, value integer NOT NULL);
INSERT INTO plpgsql_cursor_merge_target VALUES (1, 10);
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
CREATE FUNCTION plpgsql_command_cursor(fetch_it boolean) RETURNS text LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    c refcursor;
    fetched integer;
BEGIN
    OPEN c FOR UPDATE plpgsql_command_cursor_source AS source SET value = source.value + 1 RETURNING source.value;
    IF fetch_it THEN
        FETCH c INTO fetched;
    END IF;
    CLOSE c;
    RETURN coalesce(fetched::text, 'closed');
END
$$;
CREATE FUNCTION plpgsql_command_cursor_failure(fetch_it boolean) RETURNS text LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    c refcursor;
    fetched integer;
BEGIN
    OPEN c FOR UPDATE plpgsql_command_cursor_source AS source SET value = 1 / (source.id - 2) RETURNING source.value;
    IF fetch_it THEN
        FETCH c INTO fetched;
    END IF;
    CLOSE c;
    RETURN 'ok';
EXCEPTION WHEN OTHERS THEN
    RETURN SQLSTATE || '|' || SQLERRM;
END
$$;
CREATE FUNCTION plpgsql_dynamic_command_cursor(query_text text) RETURNS text LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    c refcursor;
    fetched text;
BEGIN
    OPEN c FOR EXECUTE query_text;
    FETCH c INTO fetched;
    CLOSE c;
    RETURN coalesce(fetched, '<null>');
EXCEPTION WHEN OTHERS THEN
    RETURN SQLSTATE || '|' || SQLERRM;
END
$$;
CREATE FUNCTION plpgsql_call_cursor(query_text text, fetch_it boolean, scroll_it boolean) RETURNS text LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    c refcursor;
    fetched text;
    report text;
BEGIN
    IF scroll_it THEN
        OPEN c SCROLL FOR EXECUTE query_text;
    ELSE
        OPEN c FOR EXECUTE query_text;
    END IF;
    IF fetch_it THEN
        FETCH c INTO fetched;
        report := format('fetch=%s/%s;', fetched, FOUND);
        BEGIN
            FETCH PRIOR FROM c INTO fetched;
            report := report || format('prior=%s/%s', fetched, FOUND);
        EXCEPTION WHEN OTHERS THEN
            report := report || SQLSTATE || '|' || SQLERRM;
        END;
    ELSE
        report := 'open';
    END IF;
    CLOSE c;
    RETURN report;
EXCEPTION WHEN OTHERS THEN
    RETURN SQLSTATE || '|' || SQLERRM;
END
$$;
CREATE FUNCTION plpgsql_merge_cursor(fetch_it boolean, scroll_it boolean, returning_it boolean) RETURNS text LANGUAGE plpgsql VOLATILE AS $$
DECLARE
    c refcursor;
    fetched_id integer;
    fetched_value integer;
BEGIN
    IF returning_it THEN
        IF scroll_it THEN
            OPEN c SCROLL FOR EXECUTE 'MERGE INTO plpgsql_cursor_merge_target AS target USING (VALUES (1, 5), (2, 7)) AS source(id, delta) ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = target.value + source.delta WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.delta) RETURNING target.id, target.value';
        ELSE
            OPEN c FOR EXECUTE 'MERGE INTO plpgsql_cursor_merge_target AS target USING (VALUES (1, 5), (2, 7)) AS source(id, delta) ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = target.value + source.delta WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.delta) RETURNING target.id, target.value';
        END IF;
    ELSE
        OPEN c FOR EXECUTE 'MERGE INTO plpgsql_cursor_merge_target AS target USING (VALUES (1, 5)) AS source(id, delta) ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = target.value + source.delta';
    END IF;
    IF fetch_it THEN
        FETCH c INTO fetched_id, fetched_value;
        CLOSE c;
        RETURN format('%s/%s/%s', fetched_id, fetched_value, FOUND);
    END IF;
    CLOSE c;
    RETURN 'open';
EXCEPTION WHEN OTHERS THEN
    RETURN SQLSTATE || '|' || SQLERRM;
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

-- OFFSET evaluates discarded target rows, while LIMIT prevents evaluation beyond the bounded output.
-- @case cursor_offset_projection_timing ok
INSERT INTO cursor_observations VALUES ('offset_seed', nextval('offset_cursor_sequence'));
BEGIN;
DECLARE offset_cursor CURSOR FOR SELECT nextval('offset_cursor_sequence') AS value FROM generate_series(1, 5) OFFSET 2 LIMIT 2;
MOVE FORWARD 1 FROM offset_cursor;
INSERT INTO cursor_observations VALUES ('offset_after_one', currval('offset_cursor_sequence'));
MOVE FORWARD ALL FROM offset_cursor;
INSERT INTO cursor_observations VALUES ('offset_after_all', currval('offset_cursor_sequence'));
CLOSE offset_cursor;
COMMIT;
-- @end

-- @case cursor_offset_projection_timing_result rows
SELECT name, observed FROM cursor_observations WHERE name LIKE 'offset_%' ORDER BY name;
-- @end

-- A backwards-capable scan re-evaluates target expressions for every traversed row, while FETCH 0 backs up and advances and MOVE 0 does not execute the plan.
-- @case cursor_directional_projection_timing ok
BEGIN;
DO $$
DECLARE
    directional_cursor SCROLL CURSOR FOR SELECT value, nextval('directional_cursor_sequence') AS observed FROM generate_series(1, 4) AS values(value);
    fetched_value integer;
    fetched_observed bigint;
BEGIN
    OPEN directional_cursor;
    MOVE FORWARD 2 FROM directional_cursor;
    INSERT INTO cursor_observations VALUES ('directional_after_forward', currval('directional_cursor_sequence'));
    MOVE BACKWARD 1 FROM directional_cursor;
    INSERT INTO cursor_observations VALUES ('directional_after_backward', currval('directional_cursor_sequence'));
    MOVE FORWARD 1 FROM directional_cursor;
    INSERT INTO cursor_observations VALUES ('directional_after_revisit', currval('directional_cursor_sequence'));
    FETCH RELATIVE 0 FROM directional_cursor INTO fetched_value, fetched_observed;
    INSERT INTO cursor_observations VALUES ('directional_after_fetch_zero', currval('directional_cursor_sequence'));
    MOVE FORWARD 0 FROM directional_cursor;
    INSERT INTO cursor_observations VALUES ('directional_after_move_zero', currval('directional_cursor_sequence'));
    FETCH ABSOLUTE 1 FROM directional_cursor INTO fetched_value, fetched_observed;
    INSERT INTO cursor_observations VALUES ('directional_after_absolute', currval('directional_cursor_sequence'));
    FETCH RELATIVE 2 FROM directional_cursor INTO fetched_value, fetched_observed;
    INSERT INTO cursor_observations VALUES ('directional_after_relative', currval('directional_cursor_sequence'));
    CLOSE directional_cursor;
END
$$;
COMMIT;
-- @end

-- @case cursor_directional_projection_timing_result rows
SELECT name, observed FROM cursor_observations WHERE name LIKE 'directional_after_%' ORDER BY name;
-- @end

-- Volatile qualification is scanned again in the requested direction rather than replaying cached final rows.
-- @case cursor_directional_filter_timing ok
BEGIN;
DECLARE directional_filter_cursor SCROLL CURSOR FOR SELECT value, nextval('directional_projection_sequence') AS observed FROM generate_series(1, 5) AS values(value) WHERE nextval('directional_filter_sequence') % 2 = 0;
MOVE FORWARD 2 FROM directional_filter_cursor;
INSERT INTO cursor_observations VALUES ('directional_filter_after_forward', currval('directional_filter_sequence'));
INSERT INTO cursor_observations VALUES ('directional_projection_after_forward', currval('directional_projection_sequence'));
MOVE BACKWARD 1 FROM directional_filter_cursor;
INSERT INTO cursor_observations VALUES ('directional_filter_after_backward', currval('directional_filter_sequence'));
INSERT INTO cursor_observations VALUES ('directional_projection_after_backward', currval('directional_projection_sequence'));
MOVE FORWARD 1 FROM directional_filter_cursor;
INSERT INTO cursor_observations VALUES ('directional_filter_after_revisit', currval('directional_filter_sequence'));
INSERT INTO cursor_observations VALUES ('directional_projection_after_revisit', currval('directional_projection_sequence'));
CLOSE directional_filter_cursor;
COMMIT;
-- @end

-- @case cursor_directional_filter_timing_result rows
SELECT name, observed FROM cursor_observations WHERE name LIKE 'directional_filter_%' OR name LIKE 'directional_projection_%' ORDER BY name;
-- @end

-- UNION ALL follows backwards-capable branches in reverse order, while one unsupported branch materializes the complete Append output.
-- @case cursor_directional_union_timing ok
BEGIN;
DECLARE directional_union_cursor SCROLL CURSOR FOR SELECT value, nextval('directional_union_sequence') AS observed FROM generate_series(1, 2) AS values(value) UNION ALL SELECT value, nextval('directional_union_sequence') AS observed FROM generate_series(3, 4) AS values(value);
MOVE FORWARD 3 FROM directional_union_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_native_after_forward', currval('directional_union_sequence'));
MOVE BACKWARD 1 FROM directional_union_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_native_after_backward', currval('directional_union_sequence'));
MOVE FORWARD 1 FROM directional_union_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_native_after_revisit', currval('directional_union_sequence'));
CLOSE directional_union_cursor;
DECLARE directional_union_cte_cursor SCROLL CURSOR FOR SELECT value, nextval('directional_union_cte_sequence') AS observed FROM generate_series(1, 1) AS values(value) UNION ALL (WITH branch AS MATERIALIZED (SELECT nextval('directional_union_cte_sequence') AS observed) SELECT 2 AS value, observed FROM branch);
MOVE FORWARD 1 FROM directional_union_cte_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_cte_after_first', currval('directional_union_cte_sequence'));
MOVE FORWARD 1 FROM directional_union_cte_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_cte_after_second', currval('directional_union_cte_sequence'));
CLOSE directional_union_cte_cursor;
DECLARE directional_mixed_union_cursor SCROLL CURSOR FOR SELECT left_value AS value, nextval('directional_mixed_union_sequence') AS observed FROM generate_series(1, 1) AS left_values(left_value) CROSS JOIN generate_series(1, 1) AS right_values(right_value) UNION ALL SELECT value, nextval('directional_mixed_union_sequence') AS observed FROM generate_series(2, 2) AS values(value);
MOVE FORWARD 1 FROM directional_mixed_union_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_mixed_after_first', currval('directional_mixed_union_sequence'));
MOVE FORWARD 1 FROM directional_mixed_union_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_mixed_after_second', currval('directional_mixed_union_sequence'));
MOVE BACKWARD 1 FROM directional_mixed_union_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_mixed_after_backward', currval('directional_mixed_union_sequence'));
CLOSE directional_mixed_union_cursor;
DECLARE directional_result_union_cursor SCROLL CURSOR FOR SELECT 1 AS value, nextval('directional_result_union_sequence') AS observed UNION ALL SELECT value, nextval('directional_result_union_sequence') AS observed FROM generate_series(2, 2) AS values(value);
MOVE FORWARD 2 FROM directional_result_union_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_result_after_forward', currval('directional_result_union_sequence'));
MOVE BACKWARD 1 FROM directional_result_union_cursor;
INSERT INTO cursor_observations VALUES ('directional_union_result_after_backward', currval('directional_result_union_sequence'));
CLOSE directional_result_union_cursor;
COMMIT;
-- @end

-- @case cursor_directional_union_timing_result rows
SELECT name, observed FROM cursor_observations WHERE name LIKE 'directional_union_%' ORDER BY name;
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

-- PL/pgSQL command cursors defer mutation until the portal is first run and execute the complete DML command for one fetched RETURNING row.
-- @case plpgsql_command_cursor_open_is_execution_free rows
SELECT plpgsql_command_cursor(false) AS fetched;
-- @end

-- @case plpgsql_command_cursor_open_state rows
SELECT id, value FROM plpgsql_command_cursor_source ORDER BY id;
-- @end

-- @case plpgsql_command_cursor_fetch_executes rows
SELECT plpgsql_command_cursor(true) AS fetched;
-- @end

-- @case plpgsql_command_cursor_fetch_state rows
SELECT id, value FROM plpgsql_command_cursor_source ORDER BY id;
-- @end

-- A command execution error is raised by FETCH rather than OPEN, while closing an unread command cursor leaves the table unchanged.
-- @case plpgsql_command_cursor_error_is_deferred rows
SELECT plpgsql_command_cursor_failure(false) AS open_only;
-- @end

-- @case plpgsql_command_cursor_fetch_error rows
SELECT plpgsql_command_cursor_failure(true) AS fetched;
-- @end

-- Dynamic command cursors accept row-returning DML, SHOW, and EXPLAIN and reject a command without a tuple result before it mutates data.
-- @case plpgsql_dynamic_command_cursor_shapes rows
SELECT 'insert' AS shape, plpgsql_dynamic_command_cursor('INSERT INTO plpgsql_command_cursor_source VALUES (3, 30)') AS result
UNION ALL
SELECT 'show', CASE WHEN replace(plpgsql_dynamic_command_cursor('SHOW search_path'), ' ', '') = current_schema() || ',pg_catalog' THEN 'current,pg_catalog' ELSE 'unexpected' END
UNION ALL
SELECT 'explain', CASE WHEN plpgsql_dynamic_command_cursor('EXPLAIN SELECT * FROM plpgsql_command_cursor_source') LIKE '42P11|%' THEN 'rejected' ELSE 'row' END
ORDER BY shape;
-- @end

-- @case plpgsql_dynamic_returning_cursor rows
SELECT plpgsql_dynamic_command_cursor('UPDATE plpgsql_command_cursor_source SET value = value + 1 WHERE id = 1 RETURNING value') AS result;
-- @end

-- @case plpgsql_dynamic_command_cursor_state rows
SELECT id, value FROM plpgsql_command_cursor_source ORDER BY id;
-- @end

-- CALL with output parameters is a deferred row-returning command cursor, while a procedure without output parameters is rejected before execution.
-- @case plpgsql_call_cursor_open rows
SELECT plpgsql_call_cursor('CALL plpgsql_cursor_call_out(4, NULL)', false, false) AS result;
-- @end

-- @case plpgsql_call_cursor_fetch rows
SELECT plpgsql_call_cursor('CALL plpgsql_cursor_call_out(4, NULL)', true, false) AS result;
-- @end

-- @case plpgsql_call_cursor_scroll rows
SELECT plpgsql_call_cursor('CALL plpgsql_cursor_call_out(5, NULL)', true, true) AS result;
-- @end

-- @case plpgsql_call_cursor_without_output rows
SELECT plpgsql_call_cursor('CALL plpgsql_cursor_call_no_out(6)', false, false) AS result;
-- @end

-- @case plpgsql_call_cursor_state rows
SELECT value FROM plpgsql_cursor_call_log ORDER BY value;
-- @end

-- MERGE RETURNING is deferred until first FETCH; explicit SCROLL and a missing RETURNING list fail before mutation.
-- @case plpgsql_merge_cursor_open rows
SELECT plpgsql_merge_cursor(false, false, true) AS result;
-- @end

-- @case plpgsql_merge_cursor_open_state rows
SELECT id, value FROM plpgsql_cursor_merge_target ORDER BY id;
-- @end

-- @case plpgsql_merge_cursor_fetch rows
SELECT plpgsql_merge_cursor(true, false, true) AS result;
-- @end

-- @case plpgsql_merge_cursor_fetch_state rows
SELECT id, value FROM plpgsql_cursor_merge_target ORDER BY id;
-- @end

-- @case reset_plpgsql_merge_cursor_fixture ok
TRUNCATE plpgsql_cursor_merge_target;
INSERT INTO plpgsql_cursor_merge_target VALUES (1, 10);
-- @end

-- @case plpgsql_merge_cursor_scroll_error rows
SELECT plpgsql_merge_cursor(true, true, true) AS result;
-- @end

-- @case plpgsql_merge_cursor_without_returning rows
SELECT plpgsql_merge_cursor(false, false, false) AS result;
-- @end

-- @case plpgsql_merge_cursor_error_state rows
SELECT id, value FROM plpgsql_cursor_merge_target ORDER BY id;
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

-- Three-argument setval retains PostgreSQL's called bit, session currval, nontransactional rollback behavior, default bounds, and relation-kind errors.
-- @case create_setval_is_called_fixture ok
CREATE SEQUENCE setval_is_called_sequence START WITH 10;
CREATE SEQUENCE setval_descending_sequence INCREMENT BY -1;
CREATE TABLE setval_observations(name text PRIMARY KEY, value bigint, flag boolean);
CREATE TABLE setval_not_sequence(id integer);
-- @end

-- @case setval_false_session_semantics ok
INSERT INTO setval_observations VALUES ('initial_next', nextval('setval_is_called_sequence'), NULL);
INSERT INTO setval_observations VALUES ('false_return', setval('setval_is_called_sequence', 42, false), NULL);
INSERT INTO setval_observations VALUES ('currval_after_false', currval('setval_is_called_sequence'), NULL);
INSERT INTO setval_observations SELECT 'catalog_after_false', last_value, last_value IS NULL FROM pg_catalog.pg_sequences WHERE schemaname = current_schema() AND sequencename = 'setval_is_called_sequence';
INSERT INTO setval_observations VALUES ('first_next_after_false', nextval('setval_is_called_sequence'), NULL);
-- @end

-- @case setval_false_session_semantics_result rows
SELECT name, value, flag FROM setval_observations WHERE name IN ('initial_next', 'false_return', 'currval_after_false', 'catalog_after_false', 'first_next_after_false') ORDER BY name;
-- @end

-- @case setval_false_reopen_prepare ok
INSERT INTO setval_observations VALUES ('false_reopen_return', setval('setval_is_called_sequence', 70, false), NULL);
-- @end

-- @case setval_false_reopen_catalog rows
SELECT last_value, last_value IS NULL AS is_null FROM pg_catalog.pg_sequences WHERE schemaname = current_schema() AND sequencename = 'setval_is_called_sequence';
-- @end

-- @case setval_false_reopen_next rows
SELECT nextval('setval_is_called_sequence') AS value;
-- @end

-- @case setval_false_transaction_rollback ok
BEGIN;
INSERT INTO __UQA_STATEFUL_SCHEMA__.setval_observations VALUES ('rolled_back_seed', nextval('__UQA_STATEFUL_SCHEMA__.setval_is_called_sequence'), NULL);
INSERT INTO __UQA_STATEFUL_SCHEMA__.setval_observations VALUES ('rolled_back_setval', setval('__UQA_STATEFUL_SCHEMA__.setval_is_called_sequence', 80, false), NULL);
ROLLBACK;
INSERT INTO __UQA_STATEFUL_SCHEMA__.setval_observations VALUES ('currval_after_rollback', currval('__UQA_STATEFUL_SCHEMA__.setval_is_called_sequence'), NULL);
INSERT INTO __UQA_STATEFUL_SCHEMA__.setval_observations VALUES ('next_after_rollback', nextval('__UQA_STATEFUL_SCHEMA__.setval_is_called_sequence'), NULL);
-- @end

-- @case setval_false_transaction_rollback_result rows
SELECT name, value FROM setval_observations WHERE name IN ('rolled_back_seed', 'rolled_back_setval', 'currval_after_rollback', 'next_after_rollback') ORDER BY name;
-- @end

-- @case setval_false_savepoint_rollback ok
BEGIN;
INSERT INTO setval_observations VALUES ('savepoint_seed', nextval('setval_is_called_sequence'), NULL);
SAVEPOINT setval_point;
INSERT INTO setval_observations VALUES ('rolled_back_savepoint_setval', setval('setval_is_called_sequence', 90, false), NULL);
ROLLBACK TO SAVEPOINT setval_point;
INSERT INTO setval_observations VALUES ('currval_after_savepoint', currval('setval_is_called_sequence'), NULL);
INSERT INTO setval_observations VALUES ('next_after_savepoint', nextval('setval_is_called_sequence'), NULL);
COMMIT;
-- @end

-- @case setval_false_savepoint_rollback_result rows
SELECT name, value FROM setval_observations WHERE name IN ('savepoint_seed', 'rolled_back_savepoint_setval', 'currval_after_savepoint', 'next_after_savepoint') ORDER BY name;
-- @end

-- @case setval_false_exception_subtransaction ok
INSERT INTO setval_observations VALUES ('exception_seed', nextval('setval_is_called_sequence'), NULL);
DO $$
BEGIN
    BEGIN
        PERFORM setval('setval_is_called_sequence', 100, false);
        RAISE EXCEPTION 'caught setval failure';
    EXCEPTION WHEN OTHERS THEN
        NULL;
    END;
END
$$;
INSERT INTO setval_observations VALUES ('currval_after_exception', currval('setval_is_called_sequence'), NULL);
INSERT INTO setval_observations VALUES ('next_after_exception', nextval('setval_is_called_sequence'), NULL);
-- @end

-- @case setval_false_exception_subtransaction_result rows
SELECT name, value FROM setval_observations WHERE name IN ('exception_seed', 'currval_after_exception', 'next_after_exception') ORDER BY name;
-- @end

-- @case setval_false_failed_statement error
DO $$
BEGIN
    PERFORM setval('setval_is_called_sequence', 110, false);
    RAISE EXCEPTION 'uncaught setval failure';
END
$$;
-- @end

-- @case setval_false_failed_statement_catalog rows
SELECT last_value, last_value IS NULL AS is_null FROM pg_catalog.pg_sequences WHERE schemaname = current_schema() AND sequencename = 'setval_is_called_sequence';
-- @end

-- @case setval_false_failed_statement_next rows
SELECT nextval('setval_is_called_sequence') AS value;
-- @end

-- @case setval_null_arguments_are_strict rows
SELECT setval(NULL, 120, false), setval('setval_is_called_sequence', NULL, false), setval('setval_is_called_sequence', 120, NULL), (SELECT last_value FROM pg_catalog.pg_sequences WHERE schemaname = current_schema() AND sequencename = 'setval_is_called_sequence');
-- @end

-- @case setval_positive_default_bound error
SELECT setval('setval_is_called_sequence', 0, false);
-- @end

-- @case setval_bound_failure_preserves_state rows
SELECT nextval('setval_is_called_sequence') AS value;
-- @end

-- @case setval_descending_default_bound error
SELECT setval('setval_descending_sequence', 0, false);
-- @end

-- @case setval_descending_minimum_prepare ok
INSERT INTO setval_observations VALUES ('descending_minimum', setval('setval_descending_sequence', -9223372036854775808, false), NULL);
-- @end

-- @case setval_descending_minimum_next rows
SELECT nextval('setval_descending_sequence') AS value;
-- @end

-- @case setval_descending_exhaustion error
SELECT nextval('setval_descending_sequence');
-- @end

-- @case setval_wrong_relation_kind error
SELECT setval('setval_not_sequence', 1, false);
-- @end

-- @case setval_wrong_boolean_type error
SELECT setval('setval_is_called_sequence', 120, 1);
-- @end

-- @case setval_permanent_sequence_in_read_only_transaction error
BEGIN READ ONLY;
SELECT setval('setval_is_called_sequence', 120, false);
-- @end
