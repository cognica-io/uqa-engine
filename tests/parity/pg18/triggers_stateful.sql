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
CREATE TRIGGER transition_update_statement AFTER UPDATE ON transition_items REFERENCING NEW TABLE AS new_rows OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe();
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
CREATE TABLE transition_multi_parent (a integer UNIQUE, b integer UNIQUE); CREATE TABLE transition_multi_child (id integer PRIMARY KEY, a integer REFERENCES transition_multi_parent(a) ON UPDATE CASCADE, b integer REFERENCES transition_multi_parent(b) ON UPDATE CASCADE, value integer, generated_value integer GENERATED ALWAYS AS (value * 10) STORED); CREATE TABLE transition_multi_statement_log (id bigserial PRIMARY KEY, message text); CREATE FUNCTION transition_multi_statement_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO transition_multi_statement_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP); RETURN NULL; END $probe$; CREATE TRIGGER transition_multi_cascade_row AFTER UPDATE ON transition_multi_child REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_multi_cascade_update AFTER UPDATE ON transition_multi_child REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_multi_aa_before_b BEFORE UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_before BEFORE UPDATE ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_before_a BEFORE UPDATE OF a ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_aa_after_b AFTER UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_after AFTER UPDATE ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); CREATE TRIGGER transition_multi_after_b AFTER UPDATE OF b ON transition_multi_child FOR EACH STATEMENT EXECUTE FUNCTION transition_multi_statement_probe(); INSERT INTO transition_multi_parent VALUES (1, 10), (2, 20); INSERT INTO transition_multi_child(id, a, b, value) VALUES (1, 1, 10, 100), (2, 2, 20, 200); DELETE FROM transition_log;
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

-- @case create_self_referential_cascade_transition_fixture ok
CREATE TABLE transition_self_ref (a integer PRIMARY KEY, b integer REFERENCES transition_self_ref(a) ON DELETE CASCADE); CREATE TABLE transition_self_ref_log (id bigserial PRIMARY KEY, trigger_name text, old_count bigint, old_a_sum bigint, old_b_sum bigint); CREATE FUNCTION transition_self_ref_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE row_count bigint; a_sum bigint; b_sum bigint; BEGIN SELECT count(*), coalesce(sum(a), 0), coalesce(sum(b), 0) INTO row_count, a_sum, b_sum FROM old_rows; INSERT INTO transition_self_ref_log(trigger_name, old_count, old_a_sum, old_b_sum) VALUES (TG_NAME, row_count, a_sum, b_sum); RETURN NULL; END $probe$; CREATE TRIGGER transition_self_ref_row AFTER DELETE ON transition_self_ref REFERENCING OLD TABLE AS old_rows FOR EACH ROW EXECUTE FUNCTION transition_self_ref_probe(); CREATE TRIGGER transition_self_ref_statement AFTER DELETE ON transition_self_ref REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_self_ref_probe(); INSERT INTO transition_self_ref VALUES (1, NULL), (2, 1), (3, 2);
-- @end

-- @case delete_self_referential_cascade_with_after_row_transition_trigger ok
DELETE FROM transition_self_ref WHERE a = 1;
-- @end

-- @case self_referential_cascade_splits_transition_sets rows
SELECT trigger_name, old_count, old_a_sum, old_b_sum FROM transition_self_ref_log ORDER BY id;
-- @end

-- @case delete_self_referential_cascade_without_after_row_transition_trigger ok
DELETE FROM transition_self_ref_log; DROP TRIGGER transition_self_ref_row ON transition_self_ref; INSERT INTO transition_self_ref VALUES (1, NULL), (2, 1), (3, 2), (4, 3); DELETE FROM transition_self_ref WHERE a = 1;
-- @end

-- @case self_referential_cascade_coalesces_without_after_row_transition_trigger rows
SELECT trigger_name, old_count, old_a_sum, old_b_sum FROM transition_self_ref_log ORDER BY id;
-- @end

-- @case create_branching_self_referential_cascade_transition_fixture ok
DELETE FROM transition_self_ref_log; CREATE TABLE transition_self_ref_branch (a integer PRIMARY KEY, b integer REFERENCES transition_self_ref_branch(a) ON DELETE CASCADE); CREATE TRIGGER transition_self_ref_branch_row AFTER DELETE ON transition_self_ref_branch REFERENCING OLD TABLE AS old_rows FOR EACH ROW EXECUTE FUNCTION transition_self_ref_probe(); CREATE TRIGGER transition_self_ref_branch_statement AFTER DELETE ON transition_self_ref_branch REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_self_ref_probe(); INSERT INTO transition_self_ref_branch VALUES (1, NULL), (2, 1), (3, 1), (4, 2), (5, 3);
-- @end

-- @case delete_branching_self_referential_cascade_with_after_row_transition_trigger ok
DELETE FROM transition_self_ref_branch WHERE a = 1;
-- @end

-- @case branching_self_referential_cascade_transition_sets rows
SELECT trigger_name, old_count, old_a_sum, old_b_sum FROM transition_self_ref_log ORDER BY id;
-- @end

-- @case create_deep_self_referential_cascade_transition_fixture ok
DELETE FROM transition_self_ref_log; CREATE TABLE transition_self_ref_deep (a integer PRIMARY KEY, b integer REFERENCES transition_self_ref_deep(a) ON DELETE CASCADE); CREATE TRIGGER transition_self_ref_deep_row AFTER DELETE ON transition_self_ref_deep REFERENCING OLD TABLE AS old_rows FOR EACH ROW EXECUTE FUNCTION transition_self_ref_probe(); CREATE TRIGGER transition_self_ref_deep_statement AFTER DELETE ON transition_self_ref_deep REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_self_ref_probe(); INSERT INTO transition_self_ref_deep VALUES (1, NULL), (2, 1), (3, 2), (4, 3);
-- @end

-- @case delete_deep_self_referential_cascade_with_after_row_transition_trigger ok
DELETE FROM transition_self_ref_deep WHERE a = 1;
-- @end

-- @case deep_self_referential_cascade_transition_sets rows
SELECT trigger_name, old_count, old_a_sum, old_b_sum FROM transition_self_ref_log ORDER BY id;
-- @end

-- @case create_multi_row_conflict_cascade_transition_fixture ok
DELETE FROM transition_log; CREATE TABLE transition_conflict_chain (a integer PRIMARY KEY, b integer UNIQUE, c integer, replacement integer, value integer, generated_value integer GENERATED ALWAYS AS (value * 10) STORED, FOREIGN KEY (b) REFERENCES transition_conflict_chain(a) ON UPDATE CASCADE, FOREIGN KEY (c) REFERENCES transition_conflict_chain(b) ON UPDATE CASCADE); CREATE TRIGGER transition_conflict_chain_row AFTER UPDATE ON transition_conflict_chain REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH ROW EXECUTE FUNCTION transition_probe(); CREATE TRIGGER transition_conflict_chain_statement AFTER UPDATE ON transition_conflict_chain REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_probe(); INSERT INTO transition_conflict_chain(a, b, c, value) VALUES (1, NULL, NULL, 10), (2, 1, NULL, 20), (3, NULL, 1, 30), (10, NULL, NULL, 100);
-- @end

-- @case multi_row_conflict_update_with_recursive_cascades ok
INSERT INTO transition_conflict_chain(a, b, c, replacement, value) VALUES (10, NULL, NULL, 110, 100), (1, NULL, NULL, 101, 10) ON CONFLICT (a) DO UPDATE SET a = excluded.replacement;
-- @end

-- @case multi_row_conflict_cascade_transition_sets rows
SELECT trigger_name, operation, old_count, new_count, old_sum, new_sum, old_generated_sum, new_generated_sum FROM transition_log ORDER BY id;
-- @end

-- @case multi_row_conflict_cascade_final_rows rows
SELECT a, b, c, value, generated_value FROM transition_conflict_chain ORDER BY a;
-- @end

-- @case create_nested_transition_scope_fixture ok
CREATE TABLE transition_scope_rows (value integer); INSERT INTO transition_scope_rows VALUES (1), (2), (3); CREATE TABLE transition_scope_outer (id integer); CREATE TABLE transition_scope_inner (id integer); CREATE TABLE transition_scope_log (id bigserial PRIMARY KEY, message text); CREATE FUNCTION transition_scope_helper() RETURNS bigint LANGUAGE plpgsql AS $probe$ DECLARE row_count bigint; BEGIN SELECT count(*) INTO row_count FROM transition_scope_rows; RETURN row_count; END $probe$; CREATE FUNCTION transition_scope_inner_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE row_count bigint; BEGIN SELECT count(*) INTO row_count FROM transition_scope_rows; INSERT INTO transition_scope_log(message) VALUES ('inner:' || row_count::text); RETURN NEW; END $probe$; CREATE TRIGGER transition_scope_inner_trigger AFTER INSERT ON transition_scope_inner FOR EACH ROW EXECUTE FUNCTION transition_scope_inner_probe(); CREATE FUNCTION transition_scope_outer_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE before_count bigint; helper_count bigint; after_count bigint; BEGIN SELECT count(*) INTO before_count FROM transition_scope_rows; helper_count := transition_scope_helper(); INSERT INTO transition_scope_inner VALUES (1); SELECT count(*) INTO after_count FROM transition_scope_rows; INSERT INTO transition_scope_log(message) VALUES ('outer:' || before_count::text || ':' || helper_count::text || ':' || after_count::text); RETURN NULL; END $probe$; CREATE TRIGGER transition_scope_outer_trigger AFTER INSERT ON transition_scope_outer REFERENCING NEW TABLE AS transition_scope_rows FOR EACH STATEMENT EXECUTE FUNCTION transition_scope_outer_probe();
-- @end

-- @case execute_nested_transition_scope ok
INSERT INTO transition_scope_outer VALUES (1), (2);
-- @end

-- @case nested_transition_scope_isolation rows
SELECT message FROM transition_scope_log ORDER BY id;
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

-- @case create_partition_move_trigger_fixture ok
CREATE TABLE partition_move_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, message text NOT NULL); CREATE TABLE partition_move_items (id integer, bucket integer, value text) PARTITION BY RANGE (bucket); CREATE TABLE partition_move_items_low PARTITION OF partition_move_items FOR VALUES FROM (0) TO (10); CREATE TABLE partition_move_items_high PARTITION OF partition_move_items FOR VALUES FROM (10) TO (20); CREATE FUNCTION partition_move_row_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN IF TG_LEVEL = 'STATEMENT' THEN INSERT INTO partition_move_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL); RETURN NULL; ELSIF TG_OP = 'DELETE' THEN INSERT INTO partition_move_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL || ':old=' || OLD.id::text || '/' || OLD.bucket::text || ':new=NULL'); RETURN OLD; ELSIF TG_OP = 'INSERT' THEN INSERT INTO partition_move_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL || ':old=NULL:new=' || NEW.id::text || '/' || NEW.bucket::text); RETURN NEW; ELSE INSERT INTO partition_move_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL || ':old=' || OLD.id::text || '/' || OLD.bucket::text || ':new=' || NEW.id::text || '/' || NEW.bucket::text); RETURN NEW; END IF; END $probe$; CREATE FUNCTION partition_move_transition_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE old_count bigint; new_count bigint; old_bucket_sum bigint; new_bucket_sum bigint; BEGIN SELECT count(*), coalesce(sum(bucket), 0) INTO old_count, old_bucket_sum FROM old_rows; SELECT count(*), coalesce(sum(bucket), 0) INTO new_count, new_bucket_sum FROM new_rows; INSERT INTO partition_move_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME || ':' || TG_LEVEL || ':old=' || old_count::text || '/' || old_bucket_sum::text || ':new=' || new_count::text || '/' || new_bucket_sum::text); RETURN NULL; END $probe$; CREATE TRIGGER partition_move_parent_before_statement BEFORE UPDATE ON partition_move_items FOR EACH STATEMENT EXECUTE FUNCTION partition_move_row_probe(); CREATE TRIGGER partition_move_parent_before_update BEFORE UPDATE ON partition_move_items FOR EACH ROW EXECUTE FUNCTION partition_move_row_probe(); CREATE TRIGGER partition_move_parent_after_update AFTER UPDATE ON partition_move_items FOR EACH ROW EXECUTE FUNCTION partition_move_row_probe(); CREATE TRIGGER partition_move_parent_after_statement AFTER UPDATE ON partition_move_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION partition_move_transition_probe(); CREATE TRIGGER partition_move_low_before_update BEFORE UPDATE ON partition_move_items_low FOR EACH ROW EXECUTE FUNCTION partition_move_row_probe(); CREATE TRIGGER partition_move_low_after_update AFTER UPDATE ON partition_move_items_low FOR EACH ROW EXECUTE FUNCTION partition_move_row_probe(); CREATE TRIGGER partition_move_low_before_delete BEFORE DELETE ON partition_move_items_low FOR EACH ROW EXECUTE FUNCTION partition_move_row_probe(); CREATE TRIGGER partition_move_low_after_delete AFTER DELETE ON partition_move_items_low FOR EACH ROW EXECUTE FUNCTION partition_move_row_probe(); CREATE TRIGGER partition_move_high_before_insert BEFORE INSERT ON partition_move_items_high FOR EACH ROW EXECUTE FUNCTION partition_move_row_probe(); CREATE TRIGGER partition_move_high_after_insert AFTER INSERT ON partition_move_items_high FOR EACH ROW EXECUTE FUNCTION partition_move_row_probe(); INSERT INTO partition_move_items VALUES (1, 1, 'before'); DELETE FROM partition_move_log;
-- @end

-- @case partition_move_update_returning rows
UPDATE partition_move_items SET bucket = 11, value = 'after' WHERE id = 1 RETURNING old.id, old.bucket, old.value, new.id, new.bucket, new.value;
-- @end

-- @case partition_move_trigger_order_and_transition_rows rows
SELECT message FROM partition_move_log ORDER BY seq;
-- @end

-- @case partition_move_final_leaf rows
SELECT 'low', id, bucket, value FROM partition_move_items_low UNION ALL SELECT 'high', id, bucket, value FROM partition_move_items_high ORDER BY 1, 2;
-- @end

-- @case create_partition_move_cancellation_fixture ok
CREATE TABLE partition_move_cancel_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, message text NOT NULL); CREATE TABLE partition_move_cancel_items (id integer, bucket integer, value text) PARTITION BY RANGE (bucket); CREATE TABLE partition_move_cancel_items_low PARTITION OF partition_move_cancel_items FOR VALUES FROM (0) TO (10); CREATE TABLE partition_move_cancel_items_high PARTITION OF partition_move_cancel_items FOR VALUES FROM (10) TO (20); CREATE FUNCTION partition_move_cancel_log_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO partition_move_cancel_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME); IF TG_OP = 'DELETE' THEN RETURN OLD; END IF; RETURN NEW; END $probe$; CREATE FUNCTION partition_move_cancel_row() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO partition_move_cancel_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME); RETURN NULL; END $probe$; CREATE FUNCTION partition_move_cancel_transition_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE old_count bigint; new_count bigint; old_bucket_sum bigint; new_bucket_sum bigint; BEGIN SELECT count(*), coalesce(sum(bucket), 0) INTO old_count, old_bucket_sum FROM old_rows; SELECT count(*), coalesce(sum(bucket), 0) INTO new_count, new_bucket_sum FROM new_rows; INSERT INTO partition_move_cancel_log(message) VALUES ('transition:' || old_count::text || '/' || old_bucket_sum::text || ':' || new_count::text || '/' || new_bucket_sum::text); RETURN NULL; END $probe$; CREATE TRIGGER source_cancel_delete BEFORE DELETE ON partition_move_cancel_items_low FOR EACH ROW EXECUTE FUNCTION partition_move_cancel_row(); CREATE TRIGGER source_after_delete AFTER DELETE ON partition_move_cancel_items_low FOR EACH ROW EXECUTE FUNCTION partition_move_cancel_log_probe(); CREATE TRIGGER destination_before_insert BEFORE INSERT ON partition_move_cancel_items_high FOR EACH ROW EXECUTE FUNCTION partition_move_cancel_log_probe(); CREATE TRIGGER destination_after_insert AFTER INSERT ON partition_move_cancel_items_high FOR EACH ROW EXECUTE FUNCTION partition_move_cancel_log_probe(); CREATE TRIGGER partition_move_cancel_transition AFTER UPDATE ON partition_move_cancel_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION partition_move_cancel_transition_probe(); INSERT INTO partition_move_cancel_items VALUES (1, 1, 'before');
-- @end

-- @case partition_move_before_delete_cancel_returning rows
UPDATE partition_move_cancel_items SET bucket = 11 WHERE id = 1 RETURNING old.id, old.bucket, new.id, new.bucket;
-- @end

-- @case partition_move_before_delete_cancel_state rows
SELECT seq, message, NULL::integer, NULL::integer FROM partition_move_cancel_log UNION ALL SELECT 9223372036854775807, 'row', id, bucket FROM partition_move_cancel_items ORDER BY 1;
-- @end

-- @case configure_partition_move_before_insert_cancel ok
DROP TRIGGER source_cancel_delete ON partition_move_cancel_items_low; CREATE TRIGGER source_before_delete BEFORE DELETE ON partition_move_cancel_items_low FOR EACH ROW EXECUTE FUNCTION partition_move_cancel_log_probe(); CREATE TRIGGER destination_cancel_insert BEFORE INSERT ON partition_move_cancel_items_high FOR EACH ROW EXECUTE FUNCTION partition_move_cancel_row(); DELETE FROM partition_move_cancel_log;
-- @end

-- @case partition_move_before_insert_cancel_returning rows
UPDATE partition_move_cancel_items SET bucket = 11 WHERE id = 1 RETURNING old.id, old.bucket, new.id, new.bucket;
-- @end

-- @case partition_move_before_insert_cancel_state rows
SELECT seq, message FROM partition_move_cancel_log UNION ALL SELECT 9223372036854775807, 'remaining:' || count(*)::text FROM partition_move_cancel_items ORDER BY 1;
-- @end

-- @case create_partition_move_cancel_foreign_key_fixture ok
CREATE TABLE partition_move_cancel_parent (id integer, bucket integer, PRIMARY KEY (id, bucket)) PARTITION BY RANGE (bucket); CREATE TABLE partition_move_cancel_parent_low PARTITION OF partition_move_cancel_parent FOR VALUES FROM (0) TO (10); CREATE TABLE partition_move_cancel_parent_high PARTITION OF partition_move_cancel_parent FOR VALUES FROM (10) TO (20); CREATE TABLE partition_move_cancel_child (id integer, bucket integer, FOREIGN KEY (id, bucket) REFERENCES partition_move_cancel_parent(id, bucket) ON UPDATE CASCADE ON DELETE CASCADE); CREATE FUNCTION partition_move_cancel_destination_insert() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN RETURN NULL; END $probe$; CREATE TRIGGER cancel_destination_insert BEFORE INSERT ON partition_move_cancel_parent_high FOR EACH ROW EXECUTE FUNCTION partition_move_cancel_destination_insert(); INSERT INTO partition_move_cancel_parent VALUES (1, 1); INSERT INTO partition_move_cancel_child VALUES (1, 1);
-- @end

-- @case partition_move_cancel_foreign_key_returning rows
UPDATE partition_move_cancel_parent SET bucket = 11 WHERE id = 1 RETURNING old.id, old.bucket, new.id, new.bucket;
-- @end

-- @case partition_move_cancel_foreign_key_state rows
SELECT 'parent', id, bucket FROM partition_move_cancel_parent UNION ALL SELECT 'child', id, bucket FROM partition_move_cancel_child ORDER BY 1, 2, 3;
-- @end

-- @case create_merge_partition_move_trigger_fixture ok
CREATE TABLE merge_partition_move_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, message text NOT NULL); CREATE TABLE merge_partition_move_items (id integer, bucket integer, value text) PARTITION BY RANGE (bucket); CREATE TABLE merge_partition_move_items_low PARTITION OF merge_partition_move_items FOR VALUES FROM (0) TO (10); CREATE TABLE merge_partition_move_items_high PARTITION OF merge_partition_move_items FOR VALUES FROM (10) TO (20); CREATE TABLE merge_partition_move_source (id integer PRIMARY KEY, bucket integer); CREATE FUNCTION merge_partition_move_log_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO merge_partition_move_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME); IF TG_OP = 'DELETE' THEN RETURN OLD; END IF; RETURN NEW; END $probe$; CREATE FUNCTION merge_partition_move_cancel_insert() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO merge_partition_move_log(message) VALUES (TG_NAME || ':' || TG_WHEN || ':' || TG_OP || ':' || TG_TABLE_NAME); RETURN NULL; END $probe$; CREATE FUNCTION merge_partition_move_transition_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ DECLARE old_count bigint; new_count bigint; BEGIN SELECT count(*) INTO old_count FROM old_rows; SELECT count(*) INTO new_count FROM new_rows; INSERT INTO merge_partition_move_log(message) VALUES ('transition:' || old_count::text || ':' || new_count::text); RETURN NULL; END $probe$; CREATE TRIGGER source_after_delete AFTER DELETE ON merge_partition_move_items_low FOR EACH ROW EXECUTE FUNCTION merge_partition_move_log_probe(); CREATE TRIGGER destination_before_insert BEFORE INSERT ON merge_partition_move_items_high FOR EACH ROW EXECUTE FUNCTION merge_partition_move_log_probe(); CREATE TRIGGER destination_after_insert AFTER INSERT ON merge_partition_move_items_high FOR EACH ROW EXECUTE FUNCTION merge_partition_move_log_probe(); CREATE TRIGGER merge_partition_move_transition AFTER UPDATE ON merge_partition_move_items REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION merge_partition_move_transition_probe(); INSERT INTO merge_partition_move_items VALUES (1, 1, 'success'); INSERT INTO merge_partition_move_source VALUES (1, 11);
-- @end

-- @case merge_partition_move_returning rows
MERGE INTO merge_partition_move_items AS target USING merge_partition_move_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET bucket = source.bucket RETURNING merge_action(), old.id, old.bucket, new.id, new.bucket;
-- @end

-- @case merge_partition_move_trigger_and_transition_state rows
SELECT seq, message, NULL::integer FROM merge_partition_move_log UNION ALL SELECT 9223372036854775807, 'high', bucket FROM merge_partition_move_items_high ORDER BY 1;
-- @end

-- @case configure_merge_partition_move_cancel ok
CREATE TRIGGER destination_cancel_insert BEFORE INSERT ON merge_partition_move_items_high FOR EACH ROW EXECUTE FUNCTION merge_partition_move_cancel_insert(); INSERT INTO merge_partition_move_items VALUES (2, 2, 'cancel'); INSERT INTO merge_partition_move_source VALUES (2, 12); DELETE FROM merge_partition_move_log;
-- @end

-- @case merge_partition_move_cancel_returning rows
MERGE INTO merge_partition_move_items AS target USING merge_partition_move_source AS source ON target.id = source.id WHEN MATCHED AND target.id = 2 THEN UPDATE SET bucket = source.bucket RETURNING merge_action(), old.id, old.bucket, new.id, new.bucket;
-- @end

-- @case merge_partition_move_cancel_trigger_and_transition_state rows
SELECT seq, message FROM merge_partition_move_log UNION ALL SELECT 9223372036854775807, 'remaining:' || count(*)::text FROM merge_partition_move_items WHERE id = 2 ORDER BY 1;
-- @end

-- @case configure_update_from_partition_move ok
CREATE TABLE partition_move_update_source (id integer PRIMARY KEY, bucket integer, value text); INSERT INTO partition_move_items VALUES (2, 2, 'before-from'); INSERT INTO partition_move_update_source VALUES (2, 12, 'after-from'); DELETE FROM partition_move_log;
-- @end

-- @case update_from_partition_move_returning rows
UPDATE partition_move_items AS target SET bucket = source.bucket, value = source.value FROM partition_move_update_source AS source WHERE target.id = source.id RETURNING old.id, old.bucket, old.value, new.id, new.bucket, new.value;
-- @end

-- @case update_from_partition_move_trigger_and_transition_state rows
SELECT seq, message FROM partition_move_log UNION ALL SELECT 9223372036854775807, 'high:' || id::text || '/' || bucket::text || '/' || value FROM partition_move_items_high WHERE id = 2 ORDER BY 1;
-- @end

-- @case create_partition_move_destination_mutation_fixture ok
CREATE TABLE partition_move_mutation_items (id integer, bucket integer, value text) PARTITION BY RANGE (bucket); CREATE TABLE partition_move_mutation_items_low PARTITION OF partition_move_mutation_items FOR VALUES FROM (0) TO (10); CREATE TABLE partition_move_mutation_items_high PARTITION OF partition_move_mutation_items FOR VALUES FROM (10) TO (20); CREATE TABLE partition_move_mutation_items_other PARTITION OF partition_move_mutation_items FOR VALUES FROM (20) TO (30); CREATE FUNCTION partition_move_mutate_destination() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN IF NEW.id = 1 THEN NEW.value := 'changed-by-trigger'; ELSIF NEW.id = 2 THEN NEW.bucket := 21; END IF; RETURN NEW; END $probe$; CREATE TRIGGER mutate_destination BEFORE INSERT ON partition_move_mutation_items_high FOR EACH ROW EXECUTE FUNCTION partition_move_mutate_destination(); INSERT INTO partition_move_mutation_items VALUES (1, 1, 'before'), (2, 2, 'reroute');
-- @end

-- @case partition_move_destination_value_mutation_returning rows
UPDATE partition_move_mutation_items SET bucket = 11 WHERE id = 1 RETURNING old.id, old.bucket, old.value, new.id, new.bucket, new.value;
-- @end

-- @case partition_move_destination_value_mutation_state rows
SELECT 'low', id, bucket, value FROM partition_move_mutation_items_low UNION ALL SELECT 'high', id, bucket, value FROM partition_move_mutation_items_high UNION ALL SELECT 'other', id, bucket, value FROM partition_move_mutation_items_other ORDER BY 1, 2;
-- @end

-- @case reject_partition_move_destination_reroute error
UPDATE partition_move_mutation_items SET bucket = 12 WHERE id = 2;
-- @end

-- @case partition_move_destination_reroute_is_atomic rows
SELECT 'low', id, bucket, value FROM partition_move_mutation_items_low UNION ALL SELECT 'high', id, bucket, value FROM partition_move_mutation_items_high UNION ALL SELECT 'other', id, bucket, value FROM partition_move_mutation_items_other ORDER BY 1, 2;
-- @end

-- @case create_session_replication_trigger_fixture ok
CREATE TABLE replication_trigger_items (id integer PRIMARY KEY); CREATE TABLE replication_trigger_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, message text NOT NULL); CREATE FUNCTION replication_trigger_probe() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO replication_trigger_log(message) VALUES (TG_NAME || ':' || NEW.id::text); RETURN NEW; END $probe$; CREATE TRIGGER trigger_origin AFTER INSERT ON replication_trigger_items FOR EACH ROW EXECUTE FUNCTION replication_trigger_probe(); CREATE TRIGGER trigger_replica AFTER INSERT ON replication_trigger_items FOR EACH ROW EXECUTE FUNCTION replication_trigger_probe(); CREATE TRIGGER trigger_always AFTER INSERT ON replication_trigger_items FOR EACH ROW EXECUTE FUNCTION replication_trigger_probe(); CREATE TRIGGER trigger_disabled AFTER INSERT ON replication_trigger_items FOR EACH ROW EXECUTE FUNCTION replication_trigger_probe(); ALTER TABLE replication_trigger_items ENABLE REPLICA TRIGGER trigger_replica; ALTER TABLE replication_trigger_items ENABLE ALWAYS TRIGGER trigger_always; ALTER TABLE replication_trigger_items DISABLE TRIGGER trigger_disabled; CREATE TABLE replication_fk_parent (id integer PRIMARY KEY); CREATE TABLE replication_fk_child (id integer PRIMARY KEY, parent_id integer REFERENCES replication_fk_parent(id) ON DELETE CASCADE); INSERT INTO replication_fk_parent VALUES (1); INSERT INTO replication_fk_child VALUES (1, 1);
-- @end

-- @case session_replication_origin_trigger_execution ok
SET session_replication_role = origin; INSERT INTO replication_trigger_items VALUES (1); RESET session_replication_role;
-- @end

-- @case session_replication_local_trigger_execution ok
SET session_replication_role = local; INSERT INTO replication_trigger_items VALUES (2); RESET session_replication_role;
-- @end

-- @case session_replication_replica_trigger_execution ok
SET session_replication_role = replica; INSERT INTO replication_trigger_items VALUES (3); RESET session_replication_role;
-- @end

-- @case session_replication_trigger_mode_rows rows
SELECT message FROM replication_trigger_log ORDER BY seq;
-- @end

-- @case session_replication_replica_disables_foreign_key_triggers ok
SET session_replication_role = replica; INSERT INTO replication_fk_child VALUES (2, 999); DELETE FROM replication_fk_parent WHERE id = 1; RESET session_replication_role;
-- @end

-- @case session_replication_replica_foreign_key_rows rows
SELECT id, parent_id FROM replication_fk_child ORDER BY id;
-- @end

-- @case reject_invalid_session_replication_role error
SET session_replication_role = rep;
-- @end

-- @case session_replication_role_transaction_rollback ok
BEGIN; SET session_replication_role = replica; ROLLBACK;
-- @end

-- @case session_replication_role_after_rollback rows
SELECT setting FROM pg_catalog.pg_settings WHERE name = 'session_replication_role';
-- @end

-- @case session_replication_role_pg_settings rows
SELECT setting, context, vartype, enumvals, boot_val, reset_val FROM pg_catalog.pg_settings WHERE name = 'session_replication_role';
-- @end

-- @case create_insert_select_statement_snapshot_fixture ok
CREATE TABLE trigger_snapshot_insert_target (id integer PRIMARY KEY, value text NOT NULL); CREATE TABLE trigger_snapshot_mutation_target (id integer PRIMARY KEY, value text NOT NULL); CREATE TABLE trigger_snapshot_source (id integer PRIMARY KEY, value text NOT NULL); CREATE FUNCTION trigger_seed_snapshot_source() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO trigger_snapshot_source VALUES (1, 'seeded'); RETURN NULL; END $probe$; CREATE FUNCTION trigger_seed_snapshot_update() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO trigger_snapshot_mutation_target VALUES (2, 'update-seeded'); RETURN NULL; END $probe$; CREATE FUNCTION trigger_seed_snapshot_delete() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO trigger_snapshot_mutation_target VALUES (3, 'delete-seeded'); RETURN NULL; END $probe$; CREATE TRIGGER seed_snapshot_source BEFORE INSERT ON trigger_snapshot_insert_target FOR EACH STATEMENT EXECUTE FUNCTION trigger_seed_snapshot_source(); CREATE TRIGGER seed_snapshot_update BEFORE UPDATE ON trigger_snapshot_mutation_target FOR EACH STATEMENT EXECUTE FUNCTION trigger_seed_snapshot_update(); CREATE TRIGGER seed_snapshot_delete BEFORE DELETE ON trigger_snapshot_mutation_target FOR EACH STATEMENT EXECUTE FUNCTION trigger_seed_snapshot_delete();
-- @end

-- @case insert_select_keeps_statement_snapshot rows
INSERT INTO trigger_snapshot_insert_target SELECT id, value FROM trigger_snapshot_source RETURNING id, value;
-- @end

-- @case insert_select_statement_snapshot_state rows
SELECT (SELECT count(*) FROM trigger_snapshot_source) AS source_rows, (SELECT count(*) FROM trigger_snapshot_insert_target) AS inserted_rows;
-- @end

-- @case update_keeps_statement_snapshot rows
UPDATE trigger_snapshot_mutation_target SET value = 'updated' RETURNING id, value;
-- @end

-- @case update_statement_snapshot_state rows
SELECT id, value FROM trigger_snapshot_mutation_target ORDER BY id;
-- @end

-- @case delete_keeps_statement_snapshot rows
DELETE FROM trigger_snapshot_mutation_target RETURNING id, value;
-- @end

-- @case delete_statement_snapshot_state rows
SELECT id, value FROM trigger_snapshot_mutation_target ORDER BY id;
-- @end

-- @case create_instead_of_view_trigger_fixture ok
CREATE TABLE instead_view_base (id integer PRIMARY KEY, value text NOT NULL); INSERT INTO instead_view_base VALUES (1, 'one'), (2, 'two'); CREATE VIEW instead_item_view AS SELECT id, value FROM instead_view_base WHERE id > 0; CREATE TABLE instead_view_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, entry text NOT NULL); CREATE FUNCTION instead_view_statement_log() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO instead_view_log(entry) VALUES (TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP); RETURN NULL; END $probe$; CREATE FUNCTION instead_view_transform() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO instead_view_log(entry) VALUES ('a:' || TG_OP || ':' || coalesce(OLD.id::text, '-') || ':' || coalesce(NEW.id::text, '-')); IF TG_OP <> 'DELETE' AND NEW.value = 'suppress' THEN RETURN NULL; END IF; IF TG_OP <> 'DELETE' THEN NEW.value := NEW.value || ':a'; RETURN NEW; END IF; OLD.value := OLD.value || ':a'; RETURN OLD; END $probe$; CREATE FUNCTION instead_view_apply() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO instead_view_log(entry) VALUES ('b:' || TG_OP || ':' || coalesce(OLD.value, '-') || ':' || coalesce(NEW.value, '-')); IF TG_OP = 'INSERT' THEN INSERT INTO instead_view_base VALUES (NEW.id, NEW.value || ':stored'); NEW.value := NEW.value || ':returned'; RETURN NEW; ELSIF TG_OP = 'UPDATE' THEN UPDATE instead_view_base SET id = NEW.id, value = NEW.value || ':stored' WHERE id = OLD.id; NEW.value := NEW.value || ':returned'; RETURN NEW; ELSE DELETE FROM instead_view_base WHERE id = OLD.id; OLD.value := OLD.value || ':returned'; RETURN OLD; END IF; END $probe$;
-- @end

-- @case reject_instead_of_trigger_on_table error
CREATE TRIGGER invalid_instead INSTEAD OF INSERT ON instead_view_base FOR EACH ROW EXECUTE FUNCTION instead_view_transform();
-- @end

-- @case reject_instead_of_statement_trigger error
CREATE TRIGGER invalid_instead INSTEAD OF INSERT ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_transform();
-- @end

-- @case reject_instead_of_when_condition error
CREATE TRIGGER invalid_instead INSTEAD OF INSERT ON instead_item_view FOR EACH ROW WHEN (NEW.id > 0) EXECUTE FUNCTION instead_view_transform();
-- @end

-- @case reject_instead_of_update_column_list error
CREATE TRIGGER invalid_instead INSTEAD OF UPDATE OF value ON instead_item_view FOR EACH ROW EXECUTE FUNCTION instead_view_transform();
-- @end

-- @case reject_instead_of_truncate_on_view error
CREATE TRIGGER invalid_instead INSTEAD OF TRUNCATE ON instead_item_view FOR EACH ROW EXECUTE FUNCTION instead_view_transform();
-- @end

-- @case reject_instead_of_transition_table error
CREATE TRIGGER invalid_instead INSTEAD OF INSERT ON instead_item_view REFERENCING NEW TABLE AS changed_rows FOR EACH ROW EXECUTE FUNCTION instead_view_transform();
-- @end

-- @case reject_before_row_trigger_on_view error
CREATE TRIGGER invalid_view_row BEFORE INSERT ON instead_item_view FOR EACH ROW EXECUTE FUNCTION instead_view_transform();
-- @end

-- @case create_instead_of_view_triggers ok
CREATE TRIGGER instead_before_insert BEFORE INSERT ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_statement_log(); CREATE TRIGGER instead_before_update BEFORE UPDATE ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_statement_log(); CREATE TRIGGER instead_before_delete BEFORE DELETE ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_statement_log(); CREATE TRIGGER a_instead_transform INSTEAD OF INSERT OR UPDATE OR DELETE ON instead_item_view FOR EACH ROW EXECUTE FUNCTION instead_view_transform(); CREATE TRIGGER b_instead_apply INSTEAD OF INSERT OR UPDATE OR DELETE ON instead_item_view FOR EACH ROW EXECUTE FUNCTION instead_view_apply(); CREATE TRIGGER instead_after_insert AFTER INSERT ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_statement_log(); CREATE TRIGGER instead_after_update AFTER UPDATE ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_statement_log(); CREATE TRIGGER instead_after_delete AFTER DELETE ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_statement_log();
-- @end

-- @case instead_of_view_trigger_catalog rows
SELECT t.tgname, t.tgtype, t.tgenabled, c.relhastriggers, c.relhasrules, pg_get_triggerdef(t.oid, true) FROM pg_catalog.pg_trigger AS t JOIN pg_catalog.pg_class AS c ON c.oid = t.tgrelid WHERE c.relname = 'instead_item_view' AND t.tgname IN ('a_instead_transform', 'b_instead_apply') ORDER BY t.tgname;
-- @end

-- @case instead_of_insert_chain_suppression_returning rows
INSERT INTO instead_item_view VALUES (3, 'three'), (4, 'suppress') RETURNING WITH (OLD AS o, NEW AS n) o.id AS old_id, n.id AS new_id, id, value;
-- @end

-- @case instead_of_update_returning_old_and_final_new rows
UPDATE instead_item_view SET id = id + 10, value = value || ':updated' WHERE id IN (1, 2) RETURNING WITH (OLD AS o, NEW AS n) o.id AS old_id, o.value AS old_value, n.id AS new_id, n.value AS new_value, id, value;
-- @end

-- @case instead_of_delete_returns_original_old rows
DELETE FROM instead_item_view WHERE id = 11 RETURNING WITH (OLD AS o, NEW AS n) o.value AS old_value, n.value AS new_value, id, value;
-- @end

-- @case instead_of_trigger_chain_and_base_state rows
SELECT seq, entry FROM instead_view_log UNION ALL SELECT 9223372036854775807, 'base:' || id::text || ':' || value FROM instead_view_base ORDER BY 1;
-- @end

-- @case clear_instead_of_log_before_zero_row ok
DELETE FROM instead_view_log;
-- @end

-- @case instead_of_zero_row_update ok
UPDATE instead_item_view SET value = value WHERE id = 999;
-- @end

-- @case instead_of_zero_row_statement_triggers rows
SELECT entry FROM instead_view_log ORDER BY seq;
-- @end

-- @case create_instead_of_source_context_fixture ok
CREATE TABLE instead_view_source (id integer PRIMARY KEY, next_value text NOT NULL); INSERT INTO instead_view_source VALUES (3, 'changed'), (12, 'remove'); DELETE FROM instead_view_log;
-- @end

-- @case instead_of_update_from_source_returning rows
UPDATE instead_item_view AS target SET value = source.next_value FROM instead_view_source AS source WHERE target.id = source.id AND source.id = 3 RETURNING target.id, source.next_value AS source_value, value;
-- @end

-- @case instead_of_delete_using_source_returning rows
DELETE FROM instead_item_view AS target USING instead_view_source AS source WHERE target.id = source.id AND source.id = 12 RETURNING target.id, source.id AS source_id, value;
-- @end

-- @case instead_of_source_context_final_state rows
SELECT 'base', id, value FROM instead_view_base UNION ALL SELECT 'source', id, next_value FROM instead_view_source ORDER BY 1, 2;
-- @end

-- @case instead_of_insert_select_returning rows
INSERT INTO instead_item_view SELECT id + 20, next_value || ':selected' FROM instead_view_source WHERE id = 3 RETURNING id, value;
-- @end

-- @case instead_of_insert_select_base_state rows
SELECT id, value FROM instead_view_base WHERE id = 23;
-- @end

-- @case create_instead_of_insert_select_snapshot_fixture ok
CREATE TABLE instead_view_snapshot_source (id integer PRIMARY KEY, value text NOT NULL); CREATE FUNCTION instead_view_seed_snapshot_source() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO instead_view_snapshot_source VALUES (30, 'seeded'); RETURN NULL; END $probe$; CREATE FUNCTION instead_view_seed_snapshot_update() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO instead_view_base VALUES (30, 'update-seeded'); RETURN NULL; END $probe$; CREATE FUNCTION instead_view_seed_snapshot_delete() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO instead_view_base VALUES (31, 'delete-seeded'); RETURN NULL; END $probe$; CREATE TRIGGER a_seed_snapshot_source BEFORE INSERT ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_seed_snapshot_source(); CREATE TRIGGER a_seed_snapshot_update BEFORE UPDATE ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_seed_snapshot_update(); CREATE TRIGGER a_seed_snapshot_delete BEFORE DELETE ON instead_item_view FOR EACH STATEMENT EXECUTE FUNCTION instead_view_seed_snapshot_delete();
-- @end

-- @case instead_of_insert_select_keeps_statement_snapshot rows
INSERT INTO instead_item_view SELECT id, value FROM instead_view_snapshot_source RETURNING id, value;
-- @end

-- @case instead_of_insert_select_snapshot_state rows
SELECT (SELECT count(*) FROM instead_view_snapshot_source) AS source_rows, (SELECT count(*) FROM instead_view_base WHERE id = 30) AS inserted_rows;
-- @end

-- @case instead_of_update_keeps_statement_snapshot rows
UPDATE instead_item_view SET value = 'updated' WHERE id = 30 RETURNING id, value;
-- @end

-- @case instead_of_update_statement_snapshot_state rows
SELECT id, value FROM instead_view_base WHERE id = 30;
-- @end

-- @case instead_of_delete_keeps_statement_snapshot rows
DELETE FROM instead_item_view WHERE id IN (30, 31) RETURNING id, value;
-- @end

-- @case instead_of_delete_statement_snapshot_state rows
SELECT id, value FROM instead_view_base WHERE id IN (30, 31) ORDER BY id;
-- @end

-- @case reject_instead_of_view_trigger_disable error
ALTER TABLE instead_item_view DISABLE TRIGGER a_instead_transform;
-- @end

-- @case rename_instead_of_view_trigger ok
ALTER TRIGGER a_instead_transform ON instead_item_view RENAME TO renamed_instead_transform;
-- @end

-- @case renamed_instead_of_view_trigger_catalog rows
SELECT tgname, tgenabled FROM pg_trigger WHERE tgrelid = 'instead_item_view'::regclass AND tgname = 'renamed_instead_transform';
-- @end

-- @case drop_renamed_instead_of_view_trigger ok
DROP TRIGGER renamed_instead_transform ON instead_item_view;
-- @end

-- @case dropped_instead_of_view_trigger_catalog rows
SELECT count(*) FROM pg_trigger WHERE tgrelid = 'instead_item_view'::regclass AND tgname = 'renamed_instead_transform';
-- @end
