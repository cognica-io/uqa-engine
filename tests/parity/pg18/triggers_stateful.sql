-- Stateful PostgreSQL 18.4 trigger parity fixture.
-- The runner replaces __UQA_STATEFUL_SCHEMA__ and executes each delimited case in order.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- @case create_items ok
CREATE TABLE trigger_items (
  id integer PRIMARY KEY,
  value integer,
  generated_value integer GENERATED ALWAYS AS (value * 10) STORED
);
-- @end

-- @case create_log ok
CREATE TABLE trigger_log (
  id bigserial PRIMARY KEY,
  message text
);
-- @end

-- @case create_row_function ok
CREATE FUNCTION trigger_row_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$
DECLARE visible_rows integer;
BEGIN
  IF TG_WHEN = 'BEFORE' THEN
    INSERT INTO trigger_log(message) VALUES
      (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':g=' || coalesce(NEW.generated_value::text, 'NULL') || ':arg=' || TG_ARGV[0]);
  ELSE
    SELECT count(*) INTO visible_rows FROM trigger_items;
    INSERT INTO trigger_log(message) VALUES
      (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':rows=' || visible_rows::text);
  END IF;
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END
$probe$;
-- @end

-- @case create_statement_function ok
CREATE FUNCTION trigger_statement_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$
BEGIN
  INSERT INTO trigger_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP);
  RETURN NULL;
END
$probe$;
-- @end

-- @case create_visible_count_function ok
CREATE FUNCTION trigger_visible_count() RETURNS integer LANGUAGE sql VOLATILE AS $probe$
SELECT count(*)::integer FROM trigger_items
$probe$;
-- @end

-- @case reject_integer_when error
CREATE TRIGGER integer_when BEFORE INSERT ON trigger_items FOR EACH ROW WHEN (NEW.id + 1) EXECUTE FUNCTION trigger_row_probe();
-- @end

-- @case reject_invalid_text_when error
CREATE TRIGGER invalid_text_when BEFORE INSERT ON trigger_items FOR EACH ROW WHEN ('not-a-boolean') EXECUTE FUNCTION trigger_row_probe();
-- @end

-- @case create_a_before ok
CREATE TRIGGER a_before BEFORE INSERT ON trigger_items FOR EACH ROW EXECUTE FUNCTION trigger_row_probe('first');
-- @end

-- @case create_z_before ok
CREATE TRIGGER z_before BEFORE INSERT ON trigger_items FOR EACH ROW EXECUTE FUNCTION trigger_row_probe('last');
-- @end

-- @case create_after_insert ok
CREATE TRIGGER after_insert AFTER INSERT ON trigger_items FOR EACH ROW EXECUTE FUNCTION trigger_row_probe();
-- @end

-- @case create_after_when ok
CREATE TRIGGER after_when AFTER INSERT ON trigger_items FOR EACH ROW WHEN (trigger_visible_count() = NEW.id) EXECUTE FUNCTION trigger_row_probe();
-- @end

-- @case create_after_update_statement ok
CREATE TRIGGER after_update_statement AFTER UPDATE ON trigger_items FOR EACH STATEMENT EXECUTE FUNCTION trigger_statement_probe();
-- @end

-- @case insert_two_rows ok
INSERT INTO trigger_items(id, value) VALUES (1, 1), (2, 2);
-- @end

-- @case row_order_generated_and_after_visibility rows
SELECT message FROM trigger_log ORDER BY id;
-- @end

-- @case update_zero_rows ok
UPDATE trigger_items SET value = value + 1 WHERE id = 999;
-- @end

-- @case statement_trigger_on_zero_rows rows
SELECT message FROM trigger_log ORDER BY id DESC LIMIT 1;
-- @end

-- @case direct_trigger_call_rejected error
SELECT trigger_row_probe();
-- @end

-- @case trigger_catalog_shape rows
SELECT t.tgname, t.tgtype, t.tgenabled, t.tgisinternal, t.tgnargs
FROM pg_catalog.pg_trigger AS t
JOIN pg_catalog.pg_class AS c ON c.oid = t.tgrelid
WHERE c.relname = 'trigger_items'
ORDER BY tgname;
-- @end

-- @case disable_a_before ok
ALTER TABLE trigger_items DISABLE TRIGGER a_before;
-- @end

-- @case insert_with_disabled_trigger ok
INSERT INTO trigger_items(id, value) VALUES (3, 3);
-- @end

-- @case disabled_mode rows
SELECT t.tgenabled
FROM pg_catalog.pg_trigger AS t
JOIN pg_catalog.pg_class AS c ON c.oid = t.tgrelid
WHERE c.relname = 'trigger_items' AND t.tgname = 'a_before';
-- @end

-- @case remaining_trigger_order rows
SELECT message FROM trigger_log WHERE message LIKE '%:INSERT:%' ORDER BY id DESC LIMIT 2;
-- @end

-- @case rename_trigger ok
ALTER TRIGGER z_before ON trigger_items RENAME TO renamed_before;
-- @end

-- @case renamed_trigger_catalog rows
SELECT t.tgname
FROM pg_catalog.pg_trigger AS t
JOIN pg_catalog.pg_class AS c ON c.oid = t.tgrelid
WHERE c.relname = 'trigger_items' AND t.tgname = 'renamed_before';
-- @end

-- @case create_utility_items ok
CREATE TABLE trigger_utility_items (id integer PRIMARY KEY);
-- @end

-- @case create_replace_trigger_initial ok
CREATE TRIGGER utility_replace BEFORE INSERT ON trigger_utility_items FOR EACH STATEMENT EXECUTE FUNCTION trigger_statement_probe();
-- @end

-- @case replace_trigger ok
CREATE OR REPLACE TRIGGER utility_replace AFTER INSERT ON trigger_utility_items FOR EACH STATEMENT EXECUTE FUNCTION trigger_statement_probe();
-- @end

-- @case execute_replaced_trigger ok
INSERT INTO trigger_utility_items VALUES (1);
-- @end

-- @case replaced_trigger_execution rows
SELECT message FROM trigger_log ORDER BY id DESC LIMIT 1;
-- @end

-- @case create_truncate_trigger ok
CREATE TRIGGER utility_truncate BEFORE TRUNCATE ON trigger_utility_items FOR EACH STATEMENT EXECUTE FUNCTION trigger_statement_probe();
-- @end

-- @case execute_truncate_trigger ok
TRUNCATE trigger_utility_items;
-- @end

-- @case truncate_trigger_execution rows
SELECT message FROM trigger_log ORDER BY id DESC LIMIT 1;
-- @end

-- @case function_drop_restrict error
DROP FUNCTION trigger_row_probe();
-- @end

-- @case function_drop_cascade ok
DROP FUNCTION trigger_row_probe() CASCADE;
-- @end

-- @case cascade_removed_dependent_triggers rows
SELECT count(*)
FROM pg_catalog.pg_trigger AS t
JOIN pg_catalog.pg_class AS c ON c.oid = t.tgrelid
WHERE c.relname = 'trigger_items';
-- @end

-- @case create_constraint_trigger_fixture ok
CREATE TABLE constraint_trigger_items (id integer PRIMARY KEY, value integer); CREATE TABLE constraint_trigger_log (id bigserial PRIMARY KEY, message text); CREATE TABLE constraint_trigger_reference (id integer PRIMARY KEY); CREATE FUNCTION constraint_trigger_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO constraint_trigger_log(message) VALUES (TG_NAME || ':' || TG_OP || ':' || coalesce(OLD.value::text, 'NULL') || ':' || coalesce(NEW.value::text, 'NULL')); RETURN NULL; END $probe$;
-- @end

-- @case create_deferred_constraint_trigger ok
CREATE CONSTRAINT TRIGGER constraint_guard AFTER INSERT OR UPDATE OR DELETE ON constraint_trigger_items FROM constraint_trigger_reference DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION constraint_trigger_probe();
-- @end

-- @case constraint_trigger_catalog rows
SELECT c.contype, c.condeferrable, c.condeferred, c.connoinherit, t.tgtype, t.tgdeferrable, t.tginitdeferred, t.tgconstraint = c.oid, t.tgconstrrelid = 'constraint_trigger_reference'::regclass FROM pg_constraint c JOIN pg_trigger t ON t.tgconstraint = c.oid WHERE c.conname = 'constraint_guard';
-- @end

-- @case constraint_trigger_definition rows
SELECT pg_get_triggerdef(t.oid, true) FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid WHERE c.relname = 'constraint_trigger_items' AND t.tgname = 'constraint_guard';
-- @end

-- @case deferred_constraint_trigger_commit ok
BEGIN; INSERT INTO constraint_trigger_items VALUES (1, 10); COMMIT;
-- @end

-- @case deferred_constraint_trigger_commit_log rows
SELECT message FROM constraint_trigger_log ORDER BY id;
-- @end

-- @case deferred_constraint_trigger_savepoint_rollback ok
BEGIN; SAVEPOINT discard_constraint_event; INSERT INTO constraint_trigger_items VALUES (2, 20); ROLLBACK TO SAVEPOINT discard_constraint_event; COMMIT;
-- @end

-- @case deferred_constraint_trigger_savepoint_log_unchanged rows
SELECT message FROM constraint_trigger_log ORDER BY id;
-- @end

-- @case constraint_trigger_retroactive_immediate ok
BEGIN; INSERT INTO constraint_trigger_items VALUES (2, 20); SET CONSTRAINTS constraint_guard IMMEDIATE; UPDATE constraint_trigger_items SET value = 21 WHERE id = 2; DELETE FROM constraint_trigger_items WHERE id = 2; COMMIT;
-- @end

-- @case constraint_trigger_immediate_mode_log rows
SELECT message FROM constraint_trigger_log ORDER BY id;
-- @end

-- @case constraint_trigger_pending_trigger_rename ok
BEGIN; INSERT INTO constraint_trigger_items VALUES (3, 30); ALTER TRIGGER constraint_guard ON constraint_trigger_items RENAME TO renamed_constraint_trigger; SET CONSTRAINTS constraint_guard IMMEDIATE; COMMIT;
-- @end

-- @case constraint_trigger_rename_uses_new_tg_name rows
SELECT message FROM constraint_trigger_log ORDER BY id DESC LIMIT 1;
-- @end

-- @case rename_constraint_trigger_constraint ok
ALTER TABLE constraint_trigger_items RENAME CONSTRAINT constraint_guard TO renamed_constraint;
-- @end

-- @case independent_trigger_and_constraint_names rows
SELECT t.tgname, c.conname, t.tgconstraint = c.oid FROM pg_trigger t JOIN pg_constraint c ON c.oid = t.tgconstraint WHERE t.tgname = 'renamed_constraint_trigger';
-- @end

-- @case constraint_trigger_constraint_drop_rejected error
ALTER TABLE constraint_trigger_items DROP CONSTRAINT renamed_constraint CASCADE;
-- @end

-- @case create_partition_constraint_trigger_fixture ok
CREATE TABLE partition_constraint_items (id integer, value integer) PARTITION BY RANGE (id); CREATE TABLE partition_constraint_items_low PARTITION OF partition_constraint_items FOR VALUES FROM (0) TO (10); CREATE CONSTRAINT TRIGGER partition_constraint_guard AFTER INSERT ON partition_constraint_items DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION constraint_trigger_probe();
-- @end

-- @case partition_constraint_trigger_clone_catalog rows
SELECT c.relname, t.tgparentid <> 0, pc.contype, pc.conrelid = c.oid FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_constraint pc ON pc.oid = t.tgconstraint WHERE t.tgname = 'partition_constraint_guard' ORDER BY c.relname;
-- @end

-- @case partition_constraint_trigger_executes_on_leaf ok
BEGIN; INSERT INTO partition_constraint_items VALUES (1, 100); COMMIT;
-- @end

-- @case partition_constraint_trigger_leaf_context rows
SELECT message FROM constraint_trigger_log ORDER BY id DESC LIMIT 1;
-- @end

-- @case create_failing_constraint_trigger ok
CREATE TABLE failing_constraint_items (id integer PRIMARY KEY, value integer); CREATE FUNCTION reject_negative_constraint_trigger() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN IF NEW.value < 0 THEN RAISE EXCEPTION 'negative value'; END IF; RETURN NULL; END $probe$; CREATE CONSTRAINT TRIGGER reject_negative AFTER INSERT OR UPDATE ON failing_constraint_items DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION reject_negative_constraint_trigger();
-- @end

-- @case failing_constraint_trigger_aborts_commit error
BEGIN; INSERT INTO failing_constraint_items VALUES (1, -1); COMMIT;
-- @end

-- @case failing_constraint_trigger_rolls_back_row rows
SELECT count(*) FROM failing_constraint_items;
-- @end

-- @case drop_constraint_trigger_reference ok
DROP TABLE constraint_trigger_reference;
-- @end

-- @case referenced_constraint_trigger_removed rows
SELECT count(*) FROM pg_trigger WHERE tgname = 'renamed_constraint_trigger';
-- @end

-- @case create_pending_referenced_constraint_trigger_fixture ok
CREATE TABLE pending_constraint_trigger_target (id integer); CREATE TABLE pending_constraint_trigger_reference (id integer); CREATE TABLE pending_constraint_trigger_log (id integer); CREATE FUNCTION pending_referenced_constraint_trigger() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO pending_constraint_trigger_log VALUES (NEW.id); RETURN NULL; END $probe$; CREATE CONSTRAINT TRIGGER pending_referenced_guard AFTER INSERT ON pending_constraint_trigger_target FROM pending_constraint_trigger_reference DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pending_referenced_constraint_trigger();
-- @end

-- @case drop_reference_cancels_pending_constraint_trigger ok
BEGIN; INSERT INTO pending_constraint_trigger_target VALUES (7); DROP TABLE pending_constraint_trigger_reference; COMMIT;
-- @end

-- @case dropped_reference_cancels_event_and_trigger rows
SELECT (SELECT count(*) FROM pending_constraint_trigger_log) AS fired_events, (SELECT count(*) FROM pg_trigger WHERE tgname = 'pending_referenced_guard') AS remaining_triggers;
-- @end

-- @case create_transition_relation_fixture ok
CREATE TABLE transition_items (id integer PRIMARY KEY, value integer, generated_value integer GENERATED ALWAYS AS (value * 10) STORED); CREATE TABLE transition_log (id bigserial PRIMARY KEY, trigger_name text, operation text, old_count bigint, new_count bigint, old_sum bigint, new_sum bigint, old_generated_sum bigint, new_generated_sum bigint); CREATE FUNCTION transition_mutate_row() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN NEW.value := NEW.value + 1; RETURN NEW; END $probe$; CREATE FUNCTION transition_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE old_count bigint := 0; new_count bigint := 0; old_sum bigint := 0; new_sum bigint := 0; old_generated_sum bigint := 0; new_generated_sum bigint := 0; BEGIN IF TG_OP IN ('UPDATE', 'DELETE') THEN SELECT count(*), coalesce(sum(value), 0), coalesce(sum(generated_value), 0) INTO old_count, old_sum, old_generated_sum FROM old_rows; END IF; IF TG_OP IN ('INSERT', 'UPDATE') THEN SELECT count(*), coalesce(sum(value), 0), coalesce(sum(generated_value), 0) INTO new_count, new_sum, new_generated_sum FROM new_rows; END IF; INSERT INTO transition_log(trigger_name, operation, old_count, new_count, old_sum, new_sum, old_generated_sum, new_generated_sum) VALUES (TG_NAME, TG_OP, old_count, new_count, old_sum, new_sum, old_generated_sum, new_generated_sum); RETURN NULL; END $probe$; CREATE TRIGGER transition_mutate BEFORE INSERT OR UPDATE ON transition_items FOR EACH ROW EXECUTE FUNCTION transition_mutate_row();
-- @end

-- @case reject_transition_before_trigger error
CREATE TRIGGER transition_bad_before BEFORE INSERT ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case reject_transition_multiple_events error
CREATE TRIGGER transition_bad_events AFTER INSERT OR UPDATE ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case reject_transition_update_column_list error
CREATE TRIGGER transition_bad_columns AFTER UPDATE OF value ON transition_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case reject_transition_old_table_for_insert error
CREATE TRIGGER transition_bad_insert_old AFTER INSERT ON transition_items REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case reject_transition_new_table_for_delete error
CREATE TRIGGER transition_bad_delete_new AFTER DELETE ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case reject_transition_same_relation_name error
CREATE TRIGGER transition_bad_same_name AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS changed_rows NEW TABLE AS changed_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case reject_transition_duplicate_old_table error
CREATE TRIGGER transition_bad_old_twice AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS old_rows OLD TABLE AS older_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case reject_transition_row_variable_name error
CREATE TRIGGER transition_bad_row_name AFTER INSERT ON transition_items REFERENCING NEW ROW AS new_row FOR EACH ROW EXECUTE FUNCTION transition_probe();
-- @end

-- @case reject_transition_truncate error
CREATE TRIGGER transition_bad_truncate AFTER TRUNCATE ON transition_items REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case create_transition_insert_statement ok
CREATE TRIGGER transition_insert_statement AFTER INSERT ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case create_transition_insert_row ok
CREATE TRIGGER transition_insert_row AFTER INSERT ON transition_items REFERENCING NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe();
-- @end

-- @case create_transition_update_statement ok
CREATE TRIGGER transition_update_statement AFTER UPDATE ON transition_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case create_transition_delete_statement ok
CREATE TRIGGER transition_delete_statement AFTER DELETE ON transition_items REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
-- @end

-- @case transition_insert_multiple_rows ok
INSERT INTO transition_items(id, value) VALUES (1, 10), (2, 20), (3, 30);
-- @end

-- @case transition_update_multiple_rows ok
UPDATE transition_items SET value = value * 2 WHERE id <= 2;
-- @end

-- @case transition_update_zero_rows ok
UPDATE transition_items SET value = value * 2 WHERE id = 999;
-- @end

-- @case transition_delete_multiple_rows ok
DELETE FROM transition_items WHERE id IN (1, 3);
-- @end

-- @case transition_relation_execution rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum, old_generated_sum, new_generated_sum FROM transition_log ORDER BY id;
-- @end

-- @case transition_relation_catalog rows
SELECT t.tgname, t.tgoldtable, t.tgnewtable, pg_get_triggerdef(t.oid, true) FROM pg_trigger AS t JOIN pg_class AS c ON c.oid = t.tgrelid WHERE c.relname = 'transition_items' AND (t.tgoldtable IS NOT NULL OR t.tgnewtable IS NOT NULL) ORDER BY t.tgname;
-- @end

-- @case create_transition_insert_select_source ok
CREATE TABLE transition_insert_source (id integer PRIMARY KEY, value integer); INSERT INTO transition_insert_source VALUES (4, 40), (5, 50); DELETE FROM transition_log;
-- @end

-- @case transition_insert_select ok
INSERT INTO transition_items(id, value) SELECT id, value FROM transition_insert_source ORDER BY id;
-- @end

-- @case transition_insert_select_execution rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum FROM transition_log ORDER BY id;
-- @end

-- @case clear_transition_log_before_on_conflict ok
DELETE FROM transition_log;
-- @end

-- @case transition_on_conflict_update_and_insert ok
INSERT INTO transition_items(id, value) VALUES (2, 200), (6, 60) ON CONFLICT (id) DO UPDATE SET value = excluded.value;
-- @end

-- @case transition_on_conflict_execution rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum FROM transition_log ORDER BY id;
-- @end

-- @case create_transition_update_from_source ok
CREATE TABLE transition_adjustments (id integer PRIMARY KEY, delta integer); INSERT INTO transition_adjustments VALUES (4, 3), (5, 4); DELETE FROM transition_log;
-- @end

-- @case transition_update_from ok
UPDATE transition_items AS target SET value = target.value + adjustment.delta FROM transition_adjustments AS adjustment WHERE target.id = adjustment.id;
-- @end

-- @case transition_update_from_execution rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum FROM transition_log ORDER BY id;
-- @end

-- @case create_transition_merge_source ok
CREATE TABLE transition_merge_source (id integer PRIMARY KEY, value integer); INSERT INTO transition_merge_source VALUES (2, 300), (4, 400), (7, 70); DELETE FROM transition_log;
-- @end

-- @case transition_merge_mixed_actions ok
MERGE INTO transition_items AS target USING transition_merge_source AS source ON target.id = source.id WHEN MATCHED AND source.id = 2 THEN DELETE WHEN MATCHED THEN UPDATE SET value = source.value WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value);
-- @end

-- @case transition_merge_execution rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum FROM transition_log ORDER BY id;
-- @end

-- @case create_partition_transition_fixture ok
CREATE TABLE transition_partitioned (id integer, value integer, generated_value integer GENERATED ALWAYS AS (value * 10) STORED) PARTITION BY RANGE (id); CREATE TABLE transition_partitioned_low PARTITION OF transition_partitioned FOR VALUES FROM (0) TO (10); CREATE TABLE transition_partitioned_high PARTITION OF transition_partitioned FOR VALUES FROM (10) TO (20); CREATE TRIGGER transition_partition_mutate BEFORE INSERT OR UPDATE ON transition_partitioned FOR EACH ROW EXECUTE FUNCTION transition_mutate_row(); CREATE TRIGGER transition_partition_update AFTER UPDATE ON transition_partitioned REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); INSERT INTO transition_partitioned(id, value) VALUES (1, 10), (11, 20); DELETE FROM transition_log;
-- @end


-- @case reject_partitioned_row_transition error
CREATE TRIGGER transition_bad_partitioned_row AFTER UPDATE ON transition_partitioned REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe();
-- @end

-- @case reject_partition_row_transition error
CREATE TRIGGER transition_bad_partition_row AFTER UPDATE ON transition_partitioned_low REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe();
-- @end

-- @case transition_partition_update_across_leaves ok
UPDATE transition_partitioned SET value = value * 2;
-- @end

-- @case transition_partition_execution rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum, old_generated_sum, new_generated_sum FROM transition_log ORDER BY id;
-- @end

-- @case create_inheritance_transition_fixture ok
CREATE TABLE transition_inherited (id integer, value integer, generated_value integer GENERATED ALWAYS AS (value * 10) STORED); CREATE TABLE transition_inherited_child () INHERITS (transition_inherited); CREATE TRIGGER transition_inherited_update AFTER UPDATE ON transition_inherited REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); INSERT INTO transition_inherited(id, value) VALUES (1, 10); INSERT INTO transition_inherited_child(id, value) VALUES (2, 20); DELETE FROM transition_log;
-- @end


-- @case reject_inheritance_child_row_transition error
CREATE TRIGGER transition_bad_inheritance_child_row AFTER UPDATE ON transition_inherited_child REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe();
-- @end

-- @case transition_inheritance_update_parent_and_child ok
UPDATE transition_inherited SET value = value + 1;
-- @end

-- @case transition_inheritance_execution rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum, old_generated_sum, new_generated_sum FROM transition_log ORDER BY id;
-- @end

-- @case create_foreign_key_transition_fixture ok
CREATE TABLE transition_referenced (id integer PRIMARY KEY); CREATE TABLE transition_referencing (id integer PRIMARY KEY REFERENCES transition_referenced(id) ON UPDATE CASCADE ON DELETE CASCADE, value integer, generated_value integer GENERATED ALWAYS AS (value * 10) STORED); CREATE TRIGGER transition_cascade_update AFTER UPDATE ON transition_referencing REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_cascade_delete AFTER DELETE ON transition_referencing REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); INSERT INTO transition_referenced VALUES (1), (2); INSERT INTO transition_referencing(id, value) VALUES (1, 10), (2, 20); DELETE FROM transition_log;
-- @end

-- @case transition_foreign_key_update_cascade ok
UPDATE transition_referenced SET id = id + 10;
-- @end

-- @case transition_foreign_key_update_cascade_execution rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum FROM transition_log ORDER BY id;
-- @end

-- @case clear_transition_log_before_delete_cascade ok
DELETE FROM transition_log;
-- @end

-- @case transition_foreign_key_delete_cascade ok
DELETE FROM transition_referenced;
-- @end

-- @case transition_foreign_key_delete_cascade_execution rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum FROM transition_log ORDER BY id;
-- @end

-- @case create_multi_foreign_key_transition_fixture ok
CREATE TABLE transition_multi_parent (a integer UNIQUE, b integer UNIQUE); CREATE TABLE transition_multi_child (id integer PRIMARY KEY, a integer REFERENCES transition_multi_parent(a) ON UPDATE CASCADE, b integer REFERENCES transition_multi_parent(b) ON UPDATE CASCADE, value integer, generated_value integer GENERATED ALWAYS AS (value * 10) STORED); CREATE TABLE transition_multi_statement_log (id bigserial PRIMARY KEY, message text); CREATE FUNCTION transition_multi_statement_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO transition_multi_statement_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP); RETURN NULL; END $probe$; CREATE TRIGGER transition_multi_cascade_update AFTER UPDATE ON transition_multi_child REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_multi_aa_before_b BEFORE UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_before BEFORE UPDATE ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_before_a BEFORE UPDATE OF a ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_aa_after_b AFTER UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_after AFTER UPDATE ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_after_b AFTER UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); INSERT INTO transition_multi_parent VALUES (1, 10), (2, 20); INSERT INTO transition_multi_child(id, a, b, value) VALUES (1, 1, 10, 100), (2, 2, 20, 200); DELETE FROM transition_log;
-- @end

-- @case transition_multi_foreign_key_update_cascades ok
UPDATE transition_multi_parent SET a = a + 100, b = b + 1000;
-- @end

-- @case transition_multi_foreign_key_cascade_statement_boundaries rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum FROM transition_log ORDER BY id;
-- @end

-- @case transition_multi_foreign_key_statement_trigger_cardinality rows
SELECT message FROM transition_multi_statement_log ORDER BY id;
-- @end

-- @case transition_multi_foreign_key_cascade_final_rows rows
SELECT id, a, b, value FROM transition_multi_child ORDER BY id;
-- @end


-- @case create_transition_persistence_guard_fixture ok
CREATE TABLE transition_persistence_items (value integer); CREATE FUNCTION transition_create_materialized_view() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN CREATE MATERIALIZED VIEW transition_leaked_view AS SELECT * FROM inserted_rows; RETURN NEW; END $probe$; CREATE TRIGGER transition_persistence_guard AFTER INSERT ON transition_persistence_items REFERENCING NEW TABLE AS inserted_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_create_materialized_view();
-- @end

-- @case reject_persisting_transition_relation error
INSERT INTO transition_persistence_items VALUES (42);
-- @end

-- @case transition_persistence_failure_is_atomic rows
SELECT (SELECT count(*) FROM transition_persistence_items), (SELECT count(*) FROM pg_class WHERE relname = 'transition_leaked_view');
-- @end
