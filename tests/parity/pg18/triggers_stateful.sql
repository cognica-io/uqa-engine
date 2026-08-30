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

-- @case create_automatic_view_fixture ok
CREATE TABLE automatic_view_base (id integer PRIMARY KEY, value text NOT NULL, visible boolean NOT NULL DEFAULT true, quantity integer NOT NULL DEFAULT 7); CREATE TABLE automatic_view_source (id integer PRIMARY KEY, next_value text NOT NULL); INSERT INTO automatic_view_source VALUES (3, 'three'), (4, 'four'); CREATE TABLE automatic_view_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, entry text NOT NULL); CREATE FUNCTION automatic_view_log_trigger() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN INSERT INTO automatic_view_log(entry) VALUES (TG_TABLE_NAME || ':' || TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP); IF TG_OP = 'DELETE' THEN RETURN OLD; END IF; RETURN NEW; END $probe$; CREATE VIEW automatic_item_view (item_id, label, visible, doubled) AS SELECT id, value, visible, quantity * 2 FROM automatic_view_base WHERE visible; CREATE TRIGGER automatic_view_before_insert BEFORE INSERT ON automatic_item_view FOR EACH STATEMENT EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_view_after_insert AFTER INSERT ON automatic_item_view FOR EACH STATEMENT EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_before_insert BEFORE INSERT ON automatic_view_base FOR EACH STATEMENT EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_before_insert_row BEFORE INSERT ON automatic_view_base FOR EACH ROW EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_after_insert_row AFTER INSERT ON automatic_view_base FOR EACH ROW EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_after_insert AFTER INSERT ON automatic_view_base FOR EACH STATEMENT EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_before_update BEFORE UPDATE ON automatic_view_base FOR EACH STATEMENT EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_before_update_row BEFORE UPDATE ON automatic_view_base FOR EACH ROW EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_after_update_row AFTER UPDATE ON automatic_view_base FOR EACH ROW EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_after_update AFTER UPDATE ON automatic_view_base FOR EACH STATEMENT EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_before_delete BEFORE DELETE ON automatic_view_base FOR EACH STATEMENT EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_before_delete_row BEFORE DELETE ON automatic_view_base FOR EACH ROW EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_after_delete_row AFTER DELETE ON automatic_view_base FOR EACH ROW EXECUTE FUNCTION automatic_view_log_trigger(); CREATE TRIGGER automatic_base_after_delete AFTER DELETE ON automatic_view_base FOR EACH STATEMENT EXECUTE FUNCTION automatic_view_log_trigger();
-- @end

-- @case automatic_view_insert_defaults_computed_returning rows
INSERT INTO automatic_item_view (item_id, label) VALUES (1, 'one'), (2, 'two') RETURNING item_id, label, visible, doubled;
-- @end

-- @case automatic_view_insert_select_returning rows
INSERT INTO automatic_item_view (item_id, label) SELECT id, next_value FROM automatic_view_source WHERE id = 3 RETURNING item_id, label, doubled;
-- @end

-- @case automatic_view_implicit_values_use_leading_columns rows
INSERT INTO automatic_item_view VALUES (5, 'five') RETURNING item_id, label, visible, doubled;
-- @end

-- @case automatic_view_implicit_select_uses_leading_columns rows
INSERT INTO automatic_item_view SELECT id, next_value FROM automatic_view_source WHERE id = 4 RETURNING item_id, label, visible, doubled;
-- @end

-- @case automatic_view_reject_computed_insert error
INSERT INTO automatic_item_view (item_id, label, doubled) VALUES (4, 'four', 8);
-- @end

-- @case automatic_view_upsert_returning rows
INSERT INTO automatic_item_view (item_id, label) VALUES (1, 'one:conflict') ON CONFLICT (item_id) DO UPDATE SET label = excluded.label RETURNING item_id, label, doubled;
-- @end

-- @case automatic_view_upsert_rejects_ambiguous_target_column error
INSERT INTO automatic_item_view (item_id, label) VALUES (1, 'ambiguous') ON CONFLICT (item_id) DO UPDATE SET label = label;
-- @end

-- @case automatic_view_update_returning_computed rows
UPDATE automatic_item_view SET label = label || ':updated' WHERE item_id = 1 RETURNING item_id, label, doubled;
-- @end

-- @case create_automatic_view_ambiguity_fixture ok
CREATE TABLE automatic_ambiguous_source (item_id integer PRIMARY KEY, label text NOT NULL); INSERT INTO automatic_ambiguous_source VALUES (2, 'ambiguous');
-- @end

-- @case automatic_view_update_from_rejects_ambiguous_column error
UPDATE automatic_item_view SET label = 'wrong' FROM automatic_ambiguous_source AS source WHERE item_id = source.item_id;
-- @end

-- @case automatic_view_delete_using_rejects_ambiguous_column error
DELETE FROM automatic_item_view USING automatic_ambiguous_source AS source WHERE item_id = source.item_id;
-- @end

-- @case automatic_view_ambiguity_failures_are_atomic rows
SELECT item_id, label FROM automatic_item_view WHERE item_id = 2;
-- @end

-- @case automatic_view_update_from_returning rows
UPDATE automatic_item_view AS target SET label = source.next_value || ':from' FROM automatic_view_source AS source WHERE target.item_id = source.id AND source.id = 3 RETURNING target.item_id, source.next_value, label, doubled;
-- @end

-- @case automatic_view_reject_computed_update error
UPDATE automatic_item_view SET doubled = 100 WHERE item_id = 1;
-- @end

-- @case automatic_view_without_check_option_can_leave_view rows
UPDATE automatic_item_view SET visible = false WHERE item_id = 2 RETURNING item_id, label, visible, doubled;
-- @end

-- @case automatic_view_delete_using_returning rows
DELETE FROM automatic_item_view AS target USING automatic_view_source AS source WHERE target.item_id = source.id AND source.id = 3 RETURNING target.item_id, source.next_value, label, doubled;
-- @end

-- @case automatic_view_base_state rows
SELECT id, value, visible, quantity FROM automatic_view_base ORDER BY id;
-- @end

-- @case automatic_view_only_base_triggers_fire rows
SELECT entry FROM automatic_view_log ORDER BY seq;
-- @end

-- @case create_automatic_replica_view_trigger_fixture ok
CREATE TABLE automatic_replica_capture (flag text NOT NULL); CREATE FUNCTION automatic_noop_view_trigger() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN RETURN NEW; END $probe$; CREATE TRIGGER automatic_instead_insert INSTEAD OF INSERT ON automatic_item_view FOR EACH ROW EXECUTE FUNCTION automatic_noop_view_trigger();
-- @end

-- @case automatic_replica_suppresses_defined_instead_trigger ok
SET session_replication_role = replica; INSERT INTO automatic_replica_capture SELECT is_trigger_insertable_into FROM information_schema.views WHERE table_schema = current_schema() AND table_name = 'automatic_item_view'; INSERT INTO automatic_item_view (item_id, label) VALUES (99, 'suppressed'); RESET session_replication_role;
-- @end

-- @case automatic_replica_preserves_catalog_flag_without_base_write rows
SELECT (SELECT flag FROM automatic_replica_capture) AS trigger_insertable, count(*) AS base_rows FROM automatic_view_base WHERE id = 99;
-- @end

-- @case create_automatic_check_option_views ok
CREATE VIEW automatic_local_view AS SELECT id, value, visible, quantity FROM automatic_view_base WHERE visible WITH LOCAL CHECK OPTION; CREATE TABLE automatic_nested_base (id integer PRIMARY KEY, inner_ok boolean NOT NULL DEFAULT true, outer_ok boolean NOT NULL DEFAULT true, note text NOT NULL DEFAULT 'defaulted'); CREATE VIEW automatic_inner_open AS SELECT id, inner_ok, outer_ok, note FROM automatic_nested_base WHERE inner_ok; CREATE VIEW automatic_outer_local AS SELECT id, inner_ok, outer_ok, note FROM automatic_inner_open WHERE outer_ok WITH LOCAL CHECK OPTION; CREATE VIEW automatic_outer_cascaded AS SELECT id, inner_ok, outer_ok, note FROM automatic_inner_open WHERE outer_ok WITH CASCADED CHECK OPTION; CREATE VIEW automatic_inner_checked AS SELECT id, inner_ok, outer_ok, note FROM automatic_nested_base WHERE inner_ok WITH LOCAL CHECK OPTION; CREATE VIEW automatic_outer_over_checked_local AS SELECT id, inner_ok, outer_ok, note FROM automatic_inner_checked WHERE outer_ok WITH LOCAL CHECK OPTION; CREATE VIEW automatic_nested_alias (renamed_id, renamed_note) AS SELECT id, note FROM automatic_outer_local;
-- @end

-- @case automatic_local_check_insert_rejects error
INSERT INTO automatic_local_view (id, value, visible) VALUES (10, 'ten', false);
-- @end

-- @case automatic_local_check_insert_failure_is_atomic rows
SELECT count(*) FROM automatic_view_base WHERE id = 10;
-- @end

-- @case automatic_local_check_update_fixture ok
INSERT INTO automatic_local_view (id, value) VALUES (10, 'ten'); INSERT INTO automatic_view_source VALUES (10, 'ten');
-- @end

-- @case automatic_local_check_update_rejects error
UPDATE automatic_local_view SET visible = false WHERE id = 10;
-- @end

-- @case automatic_local_check_update_failure_is_atomic rows
SELECT id, value, visible, quantity FROM automatic_view_base WHERE id = 10;
-- @end

-- @case automatic_local_check_update_from_rejects error
UPDATE automatic_local_view AS target SET visible = false FROM automatic_view_source AS source WHERE target.id = source.id AND source.id = 10;
-- @end

-- @case automatic_local_check_update_from_failure_is_atomic rows
SELECT id, value, visible, quantity FROM automatic_view_base WHERE id = 10;
-- @end

-- @case create_automatic_check_option_before_row_trigger ok
CREATE FUNCTION automatic_hide_checked_row() RETURNS trigger LANGUAGE plpgsql AS $probe$ BEGIN IF NEW.value LIKE 'hide%' THEN NEW.visible := false; END IF; RETURN NEW; END $probe$; CREATE TRIGGER automatic_hide_checked_row BEFORE INSERT OR UPDATE ON automatic_view_base FOR EACH ROW EXECUTE FUNCTION automatic_hide_checked_row();
-- @end

-- @case automatic_check_option_sees_before_insert_result error
INSERT INTO automatic_local_view (id, value) VALUES (11, 'hide-insert');
-- @end

-- @case automatic_check_option_before_insert_failure_is_atomic rows
SELECT count(*) FROM automatic_view_base WHERE id = 11;
-- @end

-- @case automatic_check_option_multirow_failure_is_atomic error
INSERT INTO automatic_local_view (id, value, visible) VALUES (11, 'eleven', true), (12, 'twelve', false);
-- @end

-- @case automatic_check_option_multirow_state rows
SELECT count(*) FROM automatic_view_base WHERE id IN (11, 12);
-- @end

-- @case automatic_check_option_before_update_fixture ok
INSERT INTO automatic_local_view (id, value) VALUES (11, 'eleven');
-- @end

-- @case automatic_check_option_sees_before_update_result error
UPDATE automatic_local_view SET value = 'hide-update' WHERE id = 11;
-- @end

-- @case automatic_check_option_sees_conflict_update_result error
INSERT INTO automatic_local_view (id, value) VALUES (11, 'hide-conflict') ON CONFLICT (id) DO UPDATE SET value = excluded.value;
-- @end

-- @case automatic_check_option_trigger_failure_state rows
SELECT id, value, visible, quantity FROM automatic_view_base WHERE id = 11;
-- @end

-- @case automatic_nested_local_ignores_unchecked_inner rows
INSERT INTO automatic_outer_local (id, inner_ok, outer_ok) VALUES (20, false, true) RETURNING id, inner_ok, outer_ok, note;
-- @end

-- @case automatic_nested_cascaded_checks_inner error
INSERT INTO automatic_outer_cascaded (id, inner_ok, outer_ok) VALUES (21, false, true);
-- @end

-- @case automatic_nested_local_checks_self error
INSERT INTO automatic_outer_local (id, inner_ok, outer_ok) VALUES (22, true, false);
-- @end

-- @case automatic_nested_local_preserves_inner_check error
INSERT INTO automatic_outer_over_checked_local (id, inner_ok, outer_ok) VALUES (23, false, true);
-- @end

-- @case automatic_nested_alias_insert_defaults_returning rows
INSERT INTO automatic_nested_alias VALUES (24, 'twenty-four') RETURNING renamed_id, renamed_note;
-- @end

-- @case automatic_nested_alias_update_returning rows
UPDATE automatic_nested_alias SET renamed_note = renamed_note || ':updated' WHERE renamed_id = 24 RETURNING renamed_id, renamed_note;
-- @end

-- @case automatic_nested_alias_delete_returning rows
DELETE FROM automatic_nested_alias WHERE renamed_id = 24 RETURNING renamed_id, renamed_note;
-- @end

-- @case automatic_nested_base_state rows
SELECT id, inner_ok, outer_ok, note FROM automatic_nested_base ORDER BY id;
-- @end

-- @case create_automatic_view_name_boundary_fixture ok
CREATE TABLE automatic_name_boundary_base (id integer PRIMARY KEY, shown text NOT NULL, secret text NOT NULL); INSERT INTO automatic_name_boundary_base VALUES (1, 'shown', 'secret'), (2, 'two', 'hidden'); CREATE VIEW automatic_name_boundary_view AS SELECT id, shown FROM automatic_name_boundary_base; CREATE TABLE automatic_alias_source (id integer PRIMARY KEY, label text NOT NULL); INSERT INTO automatic_alias_source VALUES (1, 'new-source'), (2, 'old-source');
-- @end

-- @case automatic_view_hidden_assignment_rejected error
UPDATE automatic_name_boundary_view SET shown = secret WHERE id = 1;
-- @end

-- @case automatic_view_hidden_predicate_rejected error
UPDATE automatic_name_boundary_view SET shown = 'leaked' WHERE secret = 'secret';
-- @end

-- @case automatic_view_hidden_returning_rejected error
UPDATE automatic_name_boundary_view SET shown = 'leaked' WHERE id = 1 RETURNING secret;
-- @end

-- @case automatic_view_hidden_insert_returning_rejected error
INSERT INTO automatic_name_boundary_view (id, shown) VALUES (3, 'three') RETURNING secret;
-- @end

-- @case automatic_view_hidden_column_failures_are_atomic rows
SELECT id, shown, secret FROM automatic_name_boundary_base ORDER BY id;
-- @end

-- @case automatic_view_source_alias_named_new rows
UPDATE automatic_name_boundary_view AS target SET shown = new.label FROM automatic_alias_source AS new WHERE target.id = new.id AND new.id = 1 RETURNING target.id, new.label AS source_label, target.shown;
-- @end

-- @case automatic_view_source_alias_named_old rows
DELETE FROM automatic_name_boundary_view AS target USING automatic_alias_source AS old WHERE target.id = old.id AND old.id = 2 RETURNING target.id, old.label AS source_label;
-- @end

-- @case automatic_view_returning_star_fixture ok
INSERT INTO automatic_view_source VALUES (6, 'six-star'); INSERT INTO automatic_item_view (item_id, label) VALUES (6, 'six');
-- @end

-- @case automatic_view_update_from_returning_star rows
UPDATE automatic_item_view AS target SET label = source.next_value FROM automatic_view_source AS source WHERE target.item_id = source.id AND source.id = 6 RETURNING *;
-- @end

-- @case automatic_view_delete_using_returning_star rows
DELETE FROM automatic_item_view AS target USING automatic_view_source AS source WHERE target.item_id = source.id AND source.id = 6 RETURNING *;
-- @end

-- @case create_automatic_ordered_check_fixture ok
CREATE VIEW automatic_ordered_inner AS SELECT * FROM automatic_nested_base WHERE inner_ok; CREATE VIEW automatic_ordered_outer AS SELECT * FROM automatic_ordered_inner WHERE 10 / id > 0 WITH CASCADED CHECK OPTION;
-- @end

-- @case automatic_nested_checks_run_inner_before_outer error
INSERT INTO automatic_ordered_outer (id, inner_ok, outer_ok) VALUES (0, false, true);
-- @end

-- @case create_non_updatable_views ok
CREATE VIEW automatic_distinct_view AS SELECT DISTINCT visible FROM automatic_view_base; CREATE VIEW automatic_aggregate_view AS SELECT visible, count(*) AS total FROM automatic_view_base GROUP BY visible; CREATE VIEW automatic_join_view AS SELECT base.id, base.value FROM automatic_view_base AS base JOIN automatic_view_source AS source ON source.id = base.id; CREATE TABLE automatic_readonly_base (id integer PRIMARY KEY); INSERT INTO automatic_readonly_base VALUES (1); CREATE VIEW automatic_constant_view AS SELECT id + 1 AS computed_id FROM automatic_readonly_base; CREATE VIEW automatic_xmin_view AS SELECT xmin FROM automatic_readonly_base; CREATE MATERIALIZED VIEW automatic_materialized_view AS SELECT id, value FROM automatic_view_base;
-- @end

-- @case automatic_distinct_view_insert_rejected error
INSERT INTO automatic_distinct_view VALUES (true);
-- @end

-- @case automatic_join_view_update_rejected error
UPDATE automatic_join_view SET value = 'changed';
-- @end

-- @case automatic_aggregate_view_delete_rejected error
DELETE FROM automatic_aggregate_view;
-- @end

-- @case automatic_constant_view_insert_rejected error
INSERT INTO automatic_constant_view VALUES (2);
-- @end

-- @case automatic_constant_view_update_rejected error
UPDATE automatic_constant_view SET computed_id = 3;
-- @end

-- @case automatic_xmin_view_insert_rejected error
INSERT INTO automatic_xmin_view VALUES ('1');
-- @end

-- @case automatic_xmin_view_update_rejected error
UPDATE automatic_xmin_view SET xmin = '1';
-- @end

-- @case automatic_constant_view_unknown_insert_column error
INSERT INTO automatic_constant_view (no_such_column) VALUES (2);
-- @end

-- @case automatic_constant_view_unknown_update_column error
UPDATE automatic_constant_view SET no_such_column = 3;
-- @end

-- @case create_automatic_duplicate_and_tableoid_views ok
CREATE VIEW automatic_duplicate_mapping (first_id, second_id) AS SELECT id, id FROM automatic_readonly_base; CREATE VIEW automatic_tableoid_view AS SELECT id, tableoid AS source_tableoid FROM automatic_readonly_base;
-- @end

-- @case automatic_duplicate_mapped_assignments_rejected error
UPDATE automatic_duplicate_mapping SET first_id = 2, second_id = 3;
-- @end

-- @case automatic_tableoid_projection_returning rows
UPDATE automatic_tableoid_view SET id = id WHERE id = 1 RETURNING source_tableoid = 'automatic_readonly_base'::regclass AS matches_base;
-- @end

-- @case automatic_constant_view_remains_deletable rows
DELETE FROM automatic_constant_view WHERE computed_id = 2 RETURNING computed_id;
-- @end

-- @case automatic_materialized_view_insert_rejected error
INSERT INTO automatic_materialized_view VALUES (1, 'one');
-- @end

-- @case automatic_materialized_view_update_rejected error
UPDATE automatic_materialized_view SET value = 'changed';
-- @end

-- @case automatic_materialized_view_delete_rejected error
DELETE FROM automatic_materialized_view;
-- @end

-- @case automatic_view_catalog_updatability rows
SELECT table_name, is_updatable, is_insertable_into, check_option FROM information_schema.views WHERE table_schema = current_schema() AND table_name IN ('automatic_item_view', 'automatic_local_view', 'automatic_distinct_view', 'automatic_aggregate_view', 'automatic_join_view', 'automatic_constant_view', 'automatic_xmin_view') ORDER BY table_name;
-- @end

-- @case automatic_view_column_updatability rows
SELECT column_name, is_updatable FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'automatic_item_view' ORDER BY ordinal_position;
-- @end

-- @case automatic_readonly_view_column_updatability rows
SELECT table_name, column_name, is_updatable FROM information_schema.columns WHERE table_schema = current_schema() AND table_name IN ('automatic_constant_view', 'automatic_xmin_view') ORDER BY table_name, ordinal_position;
-- @end

-- @case create_automatic_view_rule_fixture ok
CREATE TABLE automatic_rule_base (id integer PRIMARY KEY, value integer NOT NULL); CREATE TABLE automatic_rule_log (event text NOT NULL, id integer, value integer); CREATE VIEW automatic_rule_instead AS SELECT id, value FROM automatic_rule_base; CREATE RULE automatic_insert_instead AS ON INSERT TO automatic_rule_instead DO INSTEAD INSERT INTO automatic_rule_log VALUES ('insert-instead', NEW.id, NEW.value); CREATE VIEW automatic_rule_also AS SELECT id, value FROM automatic_rule_base; CREATE RULE automatic_insert_also AS ON INSERT TO automatic_rule_also DO ALSO INSERT INTO automatic_rule_log VALUES ('insert-also', NEW.id, NEW.value);
-- @end

-- @case automatic_view_insert_instead_rule ok
INSERT INTO automatic_rule_instead VALUES (1, 10);
-- @end

-- @case automatic_view_insert_instead_rule_state rows
SELECT (SELECT count(*) FROM automatic_rule_base WHERE id = 1) AS base_rows, event, id, value FROM automatic_rule_log WHERE event = 'insert-instead';
-- @end

-- @case automatic_view_insert_also_rule ok
INSERT INTO automatic_rule_also VALUES (2, 20);
-- @end

-- @case automatic_view_insert_also_rule_state rows
SELECT base.value AS base_value, log.value AS logged_value FROM automatic_rule_base AS base JOIN automatic_rule_log AS log ON log.id = base.id WHERE base.id = 2 AND log.event = 'insert-also';
-- @end

-- @case create_automatic_view_update_delete_rules ok
INSERT INTO automatic_rule_base VALUES (3, 30); CREATE RULE automatic_update_instead AS ON UPDATE TO automatic_rule_instead DO INSTEAD INSERT INTO automatic_rule_log VALUES ('update-instead', OLD.id, NEW.value); CREATE RULE automatic_delete_instead AS ON DELETE TO automatic_rule_instead DO INSTEAD INSERT INTO automatic_rule_log VALUES ('delete-instead', OLD.id, OLD.value); CREATE RULE automatic_update_also AS ON UPDATE TO automatic_rule_also DO ALSO INSERT INTO automatic_rule_log VALUES ('update-also', OLD.id, NEW.value); CREATE RULE automatic_delete_also AS ON DELETE TO automatic_rule_also DO ALSO INSERT INTO automatic_rule_log VALUES ('delete-also', OLD.id, OLD.value);
-- @end

-- @case automatic_view_update_instead_rule ok
UPDATE automatic_rule_instead SET value = 31 WHERE id = 3;
-- @end

-- @case automatic_view_update_instead_rule_state rows
SELECT base.value AS base_value, log.value AS logged_value FROM automatic_rule_base AS base JOIN automatic_rule_log AS log ON log.id = base.id WHERE base.id = 3 AND log.event = 'update-instead';
-- @end

-- @case automatic_view_delete_instead_rule ok
DELETE FROM automatic_rule_instead WHERE id = 3;
-- @end

-- @case automatic_view_delete_instead_rule_state rows
SELECT (SELECT count(*) FROM automatic_rule_base WHERE id = 3) AS base_rows, value AS logged_value FROM automatic_rule_log WHERE event = 'delete-instead';
-- @end

-- @case automatic_view_update_also_rule ok
UPDATE automatic_rule_also SET value = 21 WHERE id = 2;
-- @end

-- @case automatic_view_delete_also_rule ok
DELETE FROM automatic_rule_also WHERE id = 2;
-- @end

-- @case automatic_view_also_rules_state rows
SELECT event, value FROM automatic_rule_log WHERE event = 'update-also' UNION ALL SELECT event, value FROM automatic_rule_log WHERE event = 'delete-also' ORDER BY event;
-- @end

-- @case create_automatic_view_returning_rule ok
CREATE TABLE automatic_rule_return_action (id integer, value integer); CREATE VIEW automatic_rule_returning AS SELECT id, value FROM automatic_rule_base; CREATE RULE automatic_insert_returning AS ON INSERT TO automatic_rule_returning DO INSTEAD INSERT INTO automatic_rule_return_action VALUES (NEW.id, NEW.value) RETURNING id, value;
-- @end

-- @case automatic_view_rule_provides_returning rows
INSERT INTO automatic_rule_returning VALUES (4, 40) RETURNING id, value;
-- @end

-- @case automatic_view_rule_returning_state rows
SELECT (SELECT count(*) FROM automatic_rule_base WHERE id = 4) AS base_rows, id, value FROM automatic_rule_return_action WHERE id = 4;
-- @end

-- @case create_automatic_source_only_name_fixture ok
INSERT INTO automatic_name_boundary_base VALUES (7, 'seven', 'base-seven'), (8, 'eight', 'base-eight'); CREATE TABLE automatic_source_only_names (id integer PRIMARY KEY, secret text NOT NULL); INSERT INTO automatic_source_only_names VALUES (7, 'source-seven'), (8, 'source-eight');
-- @end

-- @case automatic_view_source_only_update_column rows
UPDATE automatic_name_boundary_view SET shown = secret FROM automatic_source_only_names AS source WHERE automatic_name_boundary_view.id = source.id AND source.id = 7 RETURNING shown, secret AS source_secret;
-- @end

-- @case automatic_view_source_only_delete_column rows
DELETE FROM automatic_name_boundary_view USING automatic_source_only_names AS source WHERE automatic_name_boundary_view.id = source.id AND secret = 'source-eight' RETURNING automatic_name_boundary_view.id, secret AS source_secret;
-- @end

-- @case create_nested_automatic_view_rule_fixture ok
CREATE TABLE automatic_nested_rule_base (id integer PRIMARY KEY, value integer NOT NULL); CREATE TABLE automatic_nested_rule_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, event text NOT NULL); CREATE VIEW automatic_nested_rule_inner AS SELECT id, value FROM automatic_nested_rule_base; CREATE VIEW automatic_nested_rule_outer AS SELECT id, value FROM automatic_nested_rule_inner; CREATE RULE automatic_nested_inner_insert AS ON INSERT TO automatic_nested_rule_inner DO ALSO INSERT INTO automatic_nested_rule_log(event) VALUES ('inner-insert'); CREATE RULE automatic_nested_outer_insert AS ON INSERT TO automatic_nested_rule_outer DO ALSO INSERT INTO automatic_nested_rule_log(event) VALUES ('outer-insert'); CREATE RULE automatic_nested_inner_update AS ON UPDATE TO automatic_nested_rule_inner DO ALSO INSERT INTO automatic_nested_rule_log(event) VALUES ('inner-update'); CREATE RULE automatic_nested_outer_update AS ON UPDATE TO automatic_nested_rule_outer DO ALSO INSERT INTO automatic_nested_rule_log(event) VALUES ('outer-update'); CREATE RULE automatic_nested_inner_delete AS ON DELETE TO automatic_nested_rule_inner DO ALSO INSERT INTO automatic_nested_rule_log(event) VALUES ('inner-delete'); CREATE RULE automatic_nested_outer_delete AS ON DELETE TO automatic_nested_rule_outer DO ALSO INSERT INTO automatic_nested_rule_log(event) VALUES ('outer-delete');
-- @end

-- @case nested_automatic_view_runs_layer_rules ok
INSERT INTO automatic_nested_rule_outer VALUES (1, 10); UPDATE automatic_nested_rule_outer SET value = 11 WHERE id = 1; DELETE FROM automatic_nested_rule_outer WHERE id = 1;
-- @end

-- @case nested_automatic_view_rule_state rows
SELECT event FROM automatic_nested_rule_log ORDER BY seq;
-- @end

-- @case create_conditional_automatic_view_rule_fixture ok
CREATE TABLE automatic_conditional_rule_base (id integer PRIMARY KEY, value integer NOT NULL); INSERT INTO automatic_conditional_rule_base VALUES (1, 10); CREATE VIEW automatic_conditional_insert_view AS SELECT id, value FROM automatic_conditional_rule_base; CREATE VIEW automatic_conditional_update_view AS SELECT id, value FROM automatic_conditional_rule_base; CREATE VIEW automatic_conditional_delete_view AS SELECT id, value FROM automatic_conditional_rule_base; CREATE RULE automatic_conditional_insert AS ON INSERT TO automatic_conditional_insert_view WHERE NEW.id > 0 DO INSTEAD NOTHING; CREATE RULE automatic_conditional_update AS ON UPDATE TO automatic_conditional_update_view WHERE OLD.id > 0 DO INSTEAD NOTHING; CREATE RULE automatic_conditional_delete AS ON DELETE TO automatic_conditional_delete_view WHERE OLD.id > 0 DO INSTEAD NOTHING;
-- @end

-- @case conditional_instead_rule_does_not_enable_insert error
INSERT INTO automatic_conditional_insert_view VALUES (2, 20);
-- @end

-- @case conditional_instead_rule_does_not_enable_update error
UPDATE automatic_conditional_update_view SET value = 11 WHERE id = 1;
-- @end

-- @case conditional_instead_rule_does_not_enable_delete error
DELETE FROM automatic_conditional_delete_view WHERE id = 1;
-- @end

-- @case conditional_instead_rule_failures_are_atomic rows
SELECT id, value FROM automatic_conditional_rule_base ORDER BY id;
-- @end

-- @case create_automatic_partition_tableoid_fixture ok
CREATE TABLE automatic_partition_oid_base (id integer, value text NOT NULL) PARTITION BY RANGE (id); CREATE TABLE automatic_partition_oid_low PARTITION OF automatic_partition_oid_base FOR VALUES FROM (0) TO (10); CREATE TABLE automatic_partition_oid_high PARTITION OF automatic_partition_oid_base FOR VALUES FROM (10) TO (20); CREATE VIEW automatic_partition_oid_view AS SELECT id, value, tableoid AS physical_oid FROM automatic_partition_oid_base; CREATE TABLE automatic_partition_oid_log (was_low boolean NOT NULL); INSERT INTO automatic_partition_oid_base VALUES (1, 'one');
-- @end

-- @case automatic_partition_tableoid_predicate_and_returning rows
UPDATE automatic_partition_oid_view SET value = 'updated' WHERE physical_oid = 'automatic_partition_oid_low'::regclass RETURNING physical_oid = 'automatic_partition_oid_low'::regclass AS is_low;
-- @end

-- @case automatic_partition_rule_tracks_physical_tableoid ok
CREATE RULE automatic_partition_oid_rule AS ON UPDATE TO automatic_partition_oid_view DO ALSO INSERT INTO automatic_partition_oid_log VALUES (OLD.physical_oid = 'automatic_partition_oid_low'::regclass); UPDATE automatic_partition_oid_view SET value = 'ruled' WHERE id = 1;
-- @end

-- @case automatic_partition_rule_uses_physical_tableoid rows
SELECT was_low FROM automatic_partition_oid_log;
-- @end

-- @case drop_automatic_partition_tableoid_rule ok
DROP RULE automatic_partition_oid_rule ON automatic_partition_oid_view;
-- @end

-- @case automatic_partition_move_returning_row_images rows
UPDATE automatic_partition_oid_view SET id = 11 WHERE id = 1 RETURNING WITH (OLD AS before, NEW AS after) before.physical_oid = 'automatic_partition_oid_low'::regclass AS old_is_low, after.physical_oid = 'automatic_partition_oid_high'::regclass AS new_is_high, physical_oid = 'automatic_partition_oid_high'::regclass AS current_is_high;
-- @end

-- @case automatic_partition_delete_uses_physical_tableoid rows
DELETE FROM automatic_partition_oid_view WHERE physical_oid = 'automatic_partition_oid_high'::regclass RETURNING physical_oid = 'automatic_partition_oid_high'::regclass AS was_high;
-- @end

-- @case create_fixed_star_automatic_view_fixture ok
CREATE TABLE automatic_fixed_star_base (id integer PRIMARY KEY, value text NOT NULL); CREATE VIEW automatic_fixed_star_view AS SELECT *, value || ':computed' AS computed FROM automatic_fixed_star_base; ALTER TABLE automatic_fixed_star_base ADD COLUMN added_later integer NOT NULL DEFAULT 7;
-- @end

-- @case fixed_star_automatic_view_insert_returning rows
INSERT INTO automatic_fixed_star_view (id, value) VALUES (1, 'one') RETURNING *;
-- @end

-- @case fixed_star_automatic_view_row_type_state rows
SELECT view_row.*, base.added_later FROM automatic_fixed_star_view AS view_row JOIN automatic_fixed_star_base AS base USING (id);
-- @end

-- @case create_set_returning_view_fixture ok
CREATE VIEW automatic_set_returning_view AS SELECT id, generate_series(1, 2) AS generated FROM automatic_readonly_base;
-- @end

-- @case set_returning_view_update_rejected error
UPDATE automatic_set_returning_view SET id = 2;
-- @end

-- @case set_returning_view_catalog_is_not_updatable rows
SELECT is_updatable, is_insertable_into FROM information_schema.views WHERE table_schema = current_schema() AND table_name = 'automatic_set_returning_view';
-- @end

-- @case non_updatable_view_check_option_rejected error
CREATE VIEW automatic_invalid_check_option AS SELECT DISTINCT visible FROM automatic_view_base WITH CHECK OPTION;
-- @end

-- @case non_updatable_join_unknown_insert_column error
INSERT INTO automatic_join_view (no_such_column) VALUES (2);
-- @end

-- @case non_updatable_join_unknown_update_column error
UPDATE automatic_join_view SET no_such_column = 3;
-- @end

-- @case create_correlated_automatic_view_fixture ok
CREATE TABLE automatic_correlated_base (id integer PRIMARY KEY, visible text NOT NULL, secret text NOT NULL); INSERT INTO automatic_correlated_base VALUES (1, 'before', 'hidden'); CREATE VIEW automatic_correlated_view (item_id, visible) AS SELECT id, visible FROM automatic_correlated_base;
-- @end

-- @case correlated_hidden_view_column_rejected error
UPDATE automatic_correlated_view AS target SET visible = 'leaked' WHERE EXISTS (SELECT 1 WHERE target.secret = 'hidden');
-- @end

-- @case correlated_visible_view_column_updates rows
UPDATE automatic_correlated_view AS target SET visible = 'after' WHERE EXISTS (SELECT 1 WHERE target.item_id = 1) RETURNING item_id, visible;
-- @end

-- @case create_view_rule_trigger_order_fixture ok
CREATE TABLE automatic_rule_trigger_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, event text NOT NULL); CREATE TABLE automatic_rule_trigger_base (id integer PRIMARY KEY); CREATE VIEW automatic_rule_trigger_also AS SELECT id FROM automatic_rule_trigger_base; CREATE FUNCTION automatic_rule_trigger_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO automatic_rule_trigger_log(event) VALUES ('trigger'); RETURN NEW; END $$; CREATE TRIGGER automatic_rule_trigger INSTEAD OF INSERT ON automatic_rule_trigger_also FOR EACH ROW EXECUTE FUNCTION automatic_rule_trigger_fn(); CREATE RULE automatic_rule_trigger_also_rule AS ON INSERT TO automatic_rule_trigger_also DO ALSO INSERT INTO automatic_rule_trigger_log(event) VALUES ('rule');
-- @end

-- @case view_also_rule_and_instead_trigger_both_run ok
INSERT INTO automatic_rule_trigger_also VALUES (1);
-- @end

-- @case view_also_rule_runs_after_instead_trigger rows
SELECT event FROM automatic_rule_trigger_log ORDER BY seq;
-- @end

-- @case create_view_instead_rule_suppresses_trigger_fixture ok
TRUNCATE automatic_rule_trigger_log; CREATE VIEW automatic_rule_trigger_instead AS SELECT id FROM automatic_rule_trigger_base; CREATE TRIGGER automatic_rule_trigger_suppressed INSTEAD OF INSERT ON automatic_rule_trigger_instead FOR EACH ROW EXECUTE FUNCTION automatic_rule_trigger_fn(); CREATE RULE automatic_rule_trigger_instead_rule AS ON INSERT TO automatic_rule_trigger_instead DO INSTEAD INSERT INTO automatic_rule_trigger_log(event) VALUES ('rule');
-- @end

-- @case view_instead_rule_suppresses_instead_trigger ok
INSERT INTO automatic_rule_trigger_instead VALUES (2);
-- @end

-- @case view_instead_rule_suppressed_trigger_state rows
SELECT event FROM automatic_rule_trigger_log ORDER BY seq;
-- @end

-- @case create_nested_insert_suppression_fixture ok
CREATE TABLE automatic_nested_suppression_base (id integer PRIMARY KEY); CREATE TABLE automatic_nested_suppression_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, event text NOT NULL); CREATE VIEW automatic_nested_suppression_inner AS SELECT id FROM automatic_nested_suppression_base; CREATE VIEW automatic_nested_suppression_outer AS SELECT id FROM automatic_nested_suppression_inner; CREATE RULE automatic_nested_suppression_inner_rule AS ON INSERT TO automatic_nested_suppression_inner DO ALSO INSERT INTO automatic_nested_suppression_log(event) VALUES ('inner'); CREATE RULE automatic_nested_suppression_outer_rule AS ON INSERT TO automatic_nested_suppression_outer DO INSTEAD INSERT INTO automatic_nested_suppression_log(event) VALUES ('outer');
-- @end

-- @case outer_instead_rule_suppresses_inner_insert_rule ok
INSERT INTO automatic_nested_suppression_outer VALUES (1);
-- @end

-- @case outer_instead_rule_suppression_state rows
SELECT event FROM automatic_nested_suppression_log ORDER BY seq;
-- @end

-- @case create_suppressed_identity_fixture ok
CREATE TABLE automatic_suppressed_identity_base (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, payload text DEFAULT 'base-default'); CREATE TABLE automatic_suppressed_identity_log (id bigint, payload text); CREATE VIEW automatic_suppressed_identity_view AS SELECT id, payload FROM automatic_suppressed_identity_base; CREATE RULE automatic_suppressed_identity_rule AS ON INSERT TO automatic_suppressed_identity_view DO INSTEAD INSERT INTO automatic_suppressed_identity_log VALUES (NEW.id, NEW.payload);
-- @end

-- @case suppressed_view_insert_does_not_apply_defaults ok
INSERT INTO automatic_suppressed_identity_view (payload) VALUES (DEFAULT);
-- @end

-- @case suppressed_view_insert_rule_sees_nulls rows
SELECT id, payload FROM automatic_suppressed_identity_log;
-- @end

-- @case drop_suppressed_identity_rule ok
DROP RULE automatic_suppressed_identity_rule ON automatic_suppressed_identity_view;
-- @end

-- @case suppressed_view_insert_does_not_consume_identity rows
INSERT INTO automatic_suppressed_identity_view (payload) VALUES (DEFAULT) RETURNING id, payload;
-- @end

-- @case create_nested_conditional_instead_fixture ok
CREATE TABLE automatic_nested_conditional_base (id integer PRIMARY KEY, value integer NOT NULL); INSERT INTO automatic_nested_conditional_base VALUES (1, 10), (2, 20); CREATE VIEW automatic_nested_conditional_inner AS SELECT id, value FROM automatic_nested_conditional_base; CREATE VIEW automatic_nested_conditional_outer AS SELECT id, value FROM automatic_nested_conditional_inner; CREATE RULE automatic_nested_conditional_rule AS ON UPDATE TO automatic_nested_conditional_inner WHERE OLD.id = 1 DO INSTEAD NOTHING;
-- @end

-- @case nested_conditional_instead_rejects_whole_update error
UPDATE automatic_nested_conditional_outer SET value = value + 1;
-- @end

-- @case nested_conditional_instead_update_is_atomic rows
SELECT id, value FROM automatic_nested_conditional_base ORDER BY id;
-- @end

-- @case create_nested_returning_provider_fixture ok
CREATE TABLE automatic_nested_returning_base (id integer PRIMARY KEY, value integer NOT NULL); CREATE TABLE automatic_nested_returning_action (id integer, value integer); CREATE TABLE automatic_nested_returning_log (id integer); CREATE TABLE automatic_nested_returning_capture (id integer, value integer); CREATE VIEW automatic_nested_returning_inner AS SELECT id, value FROM automatic_nested_returning_base; CREATE VIEW automatic_nested_returning_outer AS SELECT id, value FROM automatic_nested_returning_inner; CREATE RULE automatic_nested_returning_inner_rule AS ON INSERT TO automatic_nested_returning_inner DO INSTEAD INSERT INTO automatic_nested_returning_action VALUES (NEW.id, NEW.value) RETURNING id, value; CREATE RULE automatic_nested_returning_outer_rule AS ON INSERT TO automatic_nested_returning_outer DO ALSO INSERT INTO automatic_nested_returning_log VALUES (NEW.id);
-- @end

-- @case nested_inner_rule_provides_returning ok
DO $$ DECLARE returned_id integer; returned_value integer; BEGIN INSERT INTO automatic_nested_returning_outer VALUES (1, 10) RETURNING id, value INTO returned_id, returned_value; INSERT INTO automatic_nested_returning_capture VALUES (returned_id, returned_value); END $$;
-- @end

-- @case nested_returning_provider_actions_state rows
SELECT (SELECT count(*) FROM automatic_nested_returning_base) AS base_count, (SELECT count(*) FROM automatic_nested_returning_action) AS action_count, (SELECT count(*) FROM automatic_nested_returning_log) AS log_count, id, value FROM automatic_nested_returning_capture;
-- @end

-- @case create_lazy_automatic_rule_projection_fixture ok
CREATE TABLE automatic_lazy_projection_base (id integer PRIMARY KEY); CREATE TABLE automatic_lazy_projection_log (id integer); CREATE VIEW automatic_lazy_projection_view AS SELECT id, 1 / (id - id) AS boom FROM automatic_lazy_projection_base; CREATE RULE automatic_lazy_projection_rule AS ON INSERT TO automatic_lazy_projection_view DO INSTEAD INSERT INTO automatic_lazy_projection_log VALUES (NEW.id);
-- @end

-- @case automatic_rule_projection_is_lazy ok
INSERT INTO automatic_lazy_projection_view (id) VALUES (1);
-- @end

-- @case automatic_rule_projection_lazy_state rows
SELECT id FROM automatic_lazy_projection_log;
-- @end

-- @case create_insert_rule_layer_order_fixture ok
CREATE TABLE automatic_rule_order_base (id integer PRIMARY KEY); CREATE TABLE automatic_rule_order_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, event text NOT NULL); CREATE VIEW automatic_rule_order_inner AS SELECT id FROM automatic_rule_order_base; CREATE VIEW automatic_rule_order_outer AS SELECT id FROM automatic_rule_order_inner; CREATE RULE automatic_rule_order_base_rule AS ON INSERT TO automatic_rule_order_base DO ALSO INSERT INTO automatic_rule_order_log(event) VALUES ('base'); CREATE RULE automatic_rule_order_inner_rule AS ON INSERT TO automatic_rule_order_inner DO ALSO INSERT INTO automatic_rule_order_log(event) VALUES ('inner'); CREATE RULE automatic_rule_order_outer_rule AS ON INSERT TO automatic_rule_order_outer DO ALSO INSERT INTO automatic_rule_order_log(event) VALUES ('outer');
-- @end

-- @case insert_rules_run_base_to_outer ok
INSERT INTO automatic_rule_order_outer VALUES (1);
-- @end

-- @case insert_rule_layer_order_state rows
SELECT event FROM automatic_rule_order_log ORDER BY seq;
-- @end

-- @case create_unaliased_derived_source_fixture ok
CREATE TABLE automatic_unaliased_source_base (id integer PRIMARY KEY, shown text NOT NULL, secret text NOT NULL); INSERT INTO automatic_unaliased_source_base VALUES (1, 'before', 'hidden'); CREATE VIEW automatic_unaliased_source_view AS SELECT id, shown FROM automatic_unaliased_source_base;
-- @end

-- @case unaliased_derived_source_preserves_hidden_name rows
UPDATE automatic_unaliased_source_view SET shown = secret FROM (SELECT 'source' AS secret) WHERE id = 1 RETURNING shown;
-- @end

-- @case create_alter_check_option_fixture ok
CREATE VIEW automatic_alter_check_option_view AS SELECT DISTINCT visible FROM automatic_view_base;
-- @end

-- @case alter_non_updatable_view_check_option_rejected error
ALTER VIEW automatic_alter_check_option_view SET (check_option = local);
-- @end

-- @case rejected_alter_check_option_does_not_persist rows
SELECT check_option FROM information_schema.views WHERE table_schema = current_schema() AND table_name = 'automatic_alter_check_option_view';
-- @end

-- @case create_non_updatable_hidden_binding_fixture ok
CREATE TABLE automatic_hidden_binding_base (id integer PRIMARY KEY, value text NOT NULL, secret text NOT NULL); CREATE TABLE automatic_hidden_binding_source (id integer PRIMARY KEY); CREATE VIEW automatic_hidden_binding_join AS SELECT base.id, base.value FROM automatic_hidden_binding_base AS base JOIN automatic_hidden_binding_source AS source USING (id);
-- @end

-- @case non_updatable_view_binds_public_columns_before_shape error
UPDATE automatic_hidden_binding_join AS target SET value = 'changed' WHERE target.secret = 'hidden';
-- @end

-- @case create_view_update_delete_rule_trigger_fixture ok
CREATE TABLE automatic_rule_trigger_ud_base (id integer PRIMARY KEY, value integer NOT NULL); INSERT INTO automatic_rule_trigger_ud_base VALUES (1, 10), (2, 20); CREATE TABLE automatic_rule_trigger_ud_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, event text NOT NULL); CREATE VIEW automatic_rule_trigger_ud_view AS SELECT id, value FROM automatic_rule_trigger_ud_base; CREATE FUNCTION automatic_rule_trigger_ud_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO automatic_rule_trigger_ud_log(event) VALUES (lower(TG_OP) || '-trigger'); IF TG_OP = 'DELETE' THEN RETURN OLD; END IF; RETURN NEW; END $$; CREATE TRIGGER automatic_rule_trigger_ud INSTEAD OF UPDATE OR DELETE ON automatic_rule_trigger_ud_view FOR EACH ROW EXECUTE FUNCTION automatic_rule_trigger_ud_fn(); CREATE RULE automatic_rule_trigger_update_also AS ON UPDATE TO automatic_rule_trigger_ud_view DO ALSO INSERT INTO automatic_rule_trigger_ud_log(event) VALUES ('update-rule'); CREATE RULE automatic_rule_trigger_delete_also AS ON DELETE TO automatic_rule_trigger_ud_view DO ALSO INSERT INTO automatic_rule_trigger_ud_log(event) VALUES ('delete-rule');
-- @end

-- @case view_update_also_rule_and_instead_trigger_both_run ok
UPDATE automatic_rule_trigger_ud_view SET value = 11 WHERE id = 1;
-- @end

-- @case view_delete_also_rule_and_instead_trigger_both_run ok
DELETE FROM automatic_rule_trigger_ud_view WHERE id = 2;
-- @end

-- @case view_update_delete_rule_trigger_order rows
SELECT event FROM automatic_rule_trigger_ud_log ORDER BY seq;
-- @end

-- @case create_view_update_delete_instead_rule_fixture ok
TRUNCATE automatic_rule_trigger_ud_log; CREATE VIEW automatic_rule_trigger_ud_suppressed AS SELECT id, value FROM automatic_rule_trigger_ud_base; CREATE TRIGGER automatic_rule_trigger_ud_suppressed_trigger INSTEAD OF UPDATE OR DELETE ON automatic_rule_trigger_ud_suppressed FOR EACH ROW EXECUTE FUNCTION automatic_rule_trigger_ud_fn(); CREATE RULE automatic_rule_trigger_update_instead AS ON UPDATE TO automatic_rule_trigger_ud_suppressed DO INSTEAD INSERT INTO automatic_rule_trigger_ud_log(event) VALUES ('update-rule'); CREATE RULE automatic_rule_trigger_delete_instead AS ON DELETE TO automatic_rule_trigger_ud_suppressed DO INSTEAD INSERT INTO automatic_rule_trigger_ud_log(event) VALUES ('delete-rule');
-- @end

-- @case view_update_instead_rule_suppresses_trigger ok
UPDATE automatic_rule_trigger_ud_suppressed SET value = 12 WHERE id = 1;
-- @end

-- @case view_delete_instead_rule_suppresses_trigger ok
DELETE FROM automatic_rule_trigger_ud_suppressed WHERE id = 2;
-- @end

-- @case view_update_delete_instead_rule_state rows
SELECT log.event, base.id, base.value FROM automatic_rule_trigger_ud_log AS log CROSS JOIN automatic_rule_trigger_ud_base AS base ORDER BY log.seq, base.id;
-- @end

-- @case create_suppressed_generated_view_rule_fixture ok
CREATE TABLE automatic_suppressed_generated_base (id integer PRIMARY KEY, boom integer GENERATED ALWAYS AS (1 / (id - id)) VIRTUAL); CREATE TABLE automatic_suppressed_generated_log (id integer); CREATE VIEW automatic_suppressed_generated_view AS SELECT id, boom FROM automatic_suppressed_generated_base; CREATE RULE automatic_suppressed_generated_rule AS ON INSERT TO automatic_suppressed_generated_view DO INSTEAD INSERT INTO automatic_suppressed_generated_log VALUES (NEW.id);
-- @end

-- @case suppressed_view_rule_does_not_evaluate_generated_column ok
INSERT INTO automatic_suppressed_generated_view (id) VALUES (1);
-- @end

-- @case suppressed_view_rule_generated_state rows
SELECT id FROM automatic_suppressed_generated_log;
-- @end

-- @case create_suppressed_partition_view_rule_fixture ok
CREATE TABLE automatic_suppressed_partition_base (id integer, value text) PARTITION BY RANGE (id); CREATE TABLE automatic_suppressed_partition_low PARTITION OF automatic_suppressed_partition_base FOR VALUES FROM (0) TO (10); CREATE TABLE automatic_suppressed_partition_log (id integer); CREATE VIEW automatic_suppressed_partition_view AS SELECT id, value FROM automatic_suppressed_partition_base; CREATE RULE automatic_suppressed_partition_rule AS ON INSERT TO automatic_suppressed_partition_view DO INSTEAD INSERT INTO automatic_suppressed_partition_log VALUES (NEW.id);
-- @end

-- @case suppressed_view_rule_does_not_route_partition ok
INSERT INTO automatic_suppressed_partition_view VALUES (20, 'outside');
-- @end

-- @case suppressed_view_rule_partition_state rows
SELECT id FROM automatic_suppressed_partition_log;
-- @end

-- @case create_insert_rule_default_fixture ok
CREATE TABLE automatic_insert_rule_default_base (id integer DEFAULT 5, value integer); CREATE TABLE automatic_insert_rule_default_log (id integer); CREATE RULE automatic_insert_rule_default AS ON INSERT TO automatic_insert_rule_default_base DO ALSO INSERT INTO automatic_insert_rule_default_log VALUES (NEW.id);
-- @end

-- @case insert_rule_sees_applied_default ok
INSERT INTO automatic_insert_rule_default_base (value) VALUES (10);
-- @end

-- @case insert_rule_default_state rows
SELECT base.id AS stored_id, log.id AS rule_id FROM automatic_insert_rule_default_base AS base CROSS JOIN automatic_insert_rule_default_log AS log;
-- @end

-- @case create_insert_rule_identity_fixture ok
CREATE TABLE automatic_insert_rule_identity_base (id bigint GENERATED BY DEFAULT AS IDENTITY, value integer); CREATE TABLE automatic_insert_rule_identity_log (id bigint); CREATE TABLE automatic_insert_rule_identity_capture (affected bigint); CREATE RULE automatic_insert_rule_identity AS ON INSERT TO automatic_insert_rule_identity_base DO INSTEAD INSERT INTO automatic_insert_rule_identity_log VALUES (NEW.id);
-- @end

-- @case insert_rule_sees_prepared_identity ok
DO $$ DECLARE affected bigint; BEGIN INSERT INTO automatic_insert_rule_identity_base (value) VALUES (10); GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_insert_rule_identity_capture VALUES (affected); END $$;
-- @end

-- @case insert_rule_identity_state rows
SELECT log.id, capture.affected FROM automatic_insert_rule_identity_log AS log CROSS JOIN automatic_insert_rule_identity_capture AS capture;
-- @end

-- @case create_conditional_rule_projection_fixture ok
CREATE TABLE automatic_conditional_projection_base (id integer PRIMARY KEY); CREATE TABLE automatic_conditional_projection_log (value integer); CREATE VIEW automatic_conditional_projection_view AS SELECT id, 1 / id AS danger FROM automatic_conditional_projection_base; CREATE RULE automatic_conditional_projection_rule AS ON INSERT TO automatic_conditional_projection_view WHERE NEW.id < 0 DO ALSO INSERT INTO automatic_conditional_projection_log VALUES (NEW.danger);
-- @end

-- @case unmatched_rule_does_not_project_action_columns ok
INSERT INTO automatic_conditional_projection_view (id) VALUES (0);
-- @end

-- @case conditional_rule_projection_state rows
SELECT (SELECT count(*) FROM automatic_conditional_projection_base) AS base_count, (SELECT count(*) FROM automatic_conditional_projection_log) AS log_count;
-- @end

-- @case create_rule_returning_subquery_fixture ok
CREATE TABLE automatic_provider_base (id integer, value text); CREATE TABLE automatic_provider_action (id integer, value text); CREATE VIEW automatic_provider_view (item_id, label) AS SELECT id, value FROM automatic_provider_base; CREATE RULE automatic_provider_rule AS ON INSERT TO automatic_provider_view DO INSTEAD INSERT INTO automatic_provider_action VALUES (NEW.item_id, NEW.label) RETURNING id, value;
-- @end

-- @case rule_returning_preserves_provider_subqueries rows
INSERT INTO automatic_provider_view VALUES (1, 'one') RETURNING (SELECT label) AS got;
-- @end

-- @case create_unrouted_rule_tableoid_fixture ok
CREATE TABLE automatic_unrouted_tableoid_base (id integer); CREATE TABLE automatic_unrouted_tableoid_log (was_null boolean); CREATE TABLE automatic_rule_command_tag_capture (affected bigint); CREATE VIEW automatic_unrouted_tableoid_view AS SELECT id, tableoid AS physical_oid FROM automatic_unrouted_tableoid_base; CREATE RULE automatic_unrouted_tableoid_rule AS ON INSERT TO automatic_unrouted_tableoid_view DO INSTEAD INSERT INTO automatic_unrouted_tableoid_log VALUES (NEW.physical_oid IS NULL);
-- @end

-- @case unrouted_rule_tableoid_and_command_tag ok
DO $$ DECLARE affected bigint; BEGIN INSERT INTO automatic_unrouted_tableoid_view (id) VALUES (1); GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_rule_command_tag_capture VALUES (affected); END $$;
-- @end

-- @case unrouted_rule_tableoid_and_command_tag_state rows
SELECT log.was_null, capture.affected FROM automatic_unrouted_tableoid_log AS log CROSS JOIN automatic_rule_command_tag_capture AS capture;
-- @end

-- @case non_updatable_insert_returning_binds_before_shape error
INSERT INTO automatic_hidden_binding_join (id, value) VALUES (1, 'changed') RETURNING secret;
-- @end

-- @case create_rule_expression_suppression_fixture ok
CREATE TABLE automatic_lazy_rule_base (id integer PRIMARY KEY, value integer); INSERT INTO automatic_lazy_rule_base VALUES (1, 10); CREATE VIEW automatic_lazy_insert_rule_view AS SELECT id, value FROM automatic_lazy_rule_base; CREATE VIEW automatic_lazy_update_rule_view AS SELECT id, value FROM automatic_lazy_rule_base; CREATE RULE automatic_lazy_insert_rule AS ON INSERT TO automatic_lazy_insert_rule_view DO INSTEAD NOTHING; CREATE RULE automatic_lazy_update_rule AS ON UPDATE TO automatic_lazy_update_rule_view DO INSTEAD NOTHING; CREATE TABLE automatic_lazy_rule_capture (event text, affected bigint);
-- @end

-- @case suppressed_rules_defer_unused_expressions ok
DO $$ DECLARE affected bigint; BEGIN INSERT INTO automatic_lazy_insert_rule_view VALUES (2, 1 / 0); GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_lazy_rule_capture VALUES ('insert', affected); UPDATE automatic_lazy_update_rule_view SET value = 1 / 0 WHERE id = 1; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_lazy_rule_capture VALUES ('update', affected); END $$;
-- @end

-- @case suppressed_rule_expression_state rows
SELECT capture.event, capture.affected, base.value FROM automatic_lazy_rule_capture AS capture CROSS JOIN automatic_lazy_rule_base AS base ORDER BY capture.event;
-- @end

-- @case create_computed_column_rule_fixture ok
CREATE TABLE automatic_computed_rule_base (id integer PRIMARY KEY, value integer); INSERT INTO automatic_computed_rule_base VALUES (1, 2); CREATE TABLE automatic_computed_rule_log (event text, id integer, doubled integer); CREATE VIEW automatic_computed_insert_rule_view AS SELECT id, value * 2 AS doubled FROM automatic_computed_rule_base; CREATE VIEW automatic_computed_update_rule_view AS SELECT id, value * 2 AS doubled FROM automatic_computed_rule_base; CREATE RULE automatic_computed_insert_rule AS ON INSERT TO automatic_computed_insert_rule_view DO INSTEAD INSERT INTO automatic_computed_rule_log VALUES ('insert', NEW.id, NEW.doubled); CREATE RULE automatic_computed_update_rule AS ON UPDATE TO automatic_computed_update_rule_view DO INSTEAD INSERT INTO automatic_computed_rule_log VALUES ('update', NEW.id, NEW.doubled);
-- @end

-- @case rules_consume_supplied_computed_columns ok
INSERT INTO automatic_computed_insert_rule_view VALUES (2, 8); UPDATE automatic_computed_update_rule_view SET doubled = 12 WHERE id = 1;
-- @end

-- @case computed_column_rule_state rows
SELECT event, id, doubled FROM automatic_computed_rule_log ORDER BY event;
-- @end

-- @case create_multiple_instead_rule_count_fixture ok
CREATE TABLE automatic_multi_rule_base (id integer); CREATE TABLE automatic_multi_rule_first (id integer); CREATE TABLE automatic_multi_rule_second (id integer); CREATE TABLE automatic_multi_rule_capture (affected bigint); CREATE VIEW automatic_multi_rule_view AS SELECT id FROM automatic_multi_rule_base; CREATE RULE automatic_multi_rule_a AS ON INSERT TO automatic_multi_rule_view DO INSTEAD INSERT INTO automatic_multi_rule_first VALUES (NEW.id); CREATE RULE automatic_multi_rule_b AS ON INSERT TO automatic_multi_rule_view DO INSTEAD INSERT INTO automatic_multi_rule_second VALUES (NEW.id);
-- @end

-- @case multiple_instead_rule_command_tag ok
DO $$ DECLARE affected bigint; BEGIN INSERT INTO automatic_multi_rule_view VALUES (1), (2); GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_multi_rule_capture VALUES (affected); END $$;
-- @end

-- @case multiple_instead_rule_command_tag_state rows
SELECT capture.affected, (SELECT count(*) FROM automatic_multi_rule_first) AS first_count, (SELECT count(*) FROM automatic_multi_rule_second) AS second_count FROM automatic_multi_rule_capture AS capture;
-- @end

-- @case create_suppressed_statement_trigger_fixture ok
CREATE TABLE automatic_statement_rule_base (id integer); CREATE TABLE automatic_statement_rule_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, event text NOT NULL); CREATE VIEW automatic_statement_rule_view AS SELECT id FROM automatic_statement_rule_base; CREATE FUNCTION automatic_statement_rule_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO automatic_statement_rule_log(event) VALUES (TG_WHEN || ':' || TG_LEVEL || ':' || TG_OP); RETURN NEW; END $$; CREATE TRIGGER automatic_statement_rule_before BEFORE INSERT ON automatic_statement_rule_view FOR EACH STATEMENT EXECUTE FUNCTION automatic_statement_rule_fn(); CREATE TRIGGER automatic_statement_rule_row INSTEAD OF INSERT ON automatic_statement_rule_view FOR EACH ROW EXECUTE FUNCTION automatic_statement_rule_fn(); CREATE TRIGGER automatic_statement_rule_after AFTER INSERT ON automatic_statement_rule_view FOR EACH STATEMENT EXECUTE FUNCTION automatic_statement_rule_fn(); CREATE RULE automatic_statement_rule_instead AS ON INSERT TO automatic_statement_rule_view DO INSTEAD INSERT INTO automatic_statement_rule_log(event) VALUES ('rule');
-- @end

-- @case unconditional_instead_rule_suppresses_all_view_triggers ok
INSERT INTO automatic_statement_rule_view VALUES (1);
-- @end

-- @case suppressed_view_statement_trigger_state rows
SELECT event FROM automatic_statement_rule_log ORDER BY seq;
-- @end

-- @case create_complete_dml_subquery_scope_fixture ok
CREATE TABLE automatic_scoped_view_base (id integer PRIMARY KEY, label text NOT NULL); CREATE TABLE automatic_scoped_view_source (id integer PRIMARY KEY, label text NOT NULL); INSERT INTO automatic_scoped_view_source VALUES (1, 'from-source'), (2, 'delete-source'); CREATE VIEW automatic_scoped_view (item_id, item_label) AS SELECT id, label FROM automatic_scoped_view_base; INSERT INTO automatic_scoped_view VALUES (1, 'before');
-- @end

-- @case conflict_subquery_keeps_excluded_scope rows
INSERT INTO automatic_scoped_view VALUES (1, 'from-excluded') ON CONFLICT (item_id) DO UPDATE SET item_label = (SELECT excluded.item_label) RETURNING item_id, item_label;
-- @end

-- @case update_returning_subqueries_keep_source_and_row_images rows
UPDATE automatic_scoped_view AS target SET item_label = source.label FROM automatic_scoped_view_source AS source WHERE target.item_id = source.id AND source.id = 1 RETURNING WITH (OLD AS before, NEW AS after) (SELECT source.label) AS source_label, (SELECT before.item_label) AS old_label, (SELECT after.item_label) AS new_label;
-- @end

-- @case create_delete_subquery_scope_row ok
INSERT INTO automatic_scoped_view VALUES (2, 'before-delete');
-- @end

-- @case delete_returning_subqueries_keep_source_and_target rows
DELETE FROM automatic_scoped_view AS target USING automatic_scoped_view_source AS source WHERE target.item_id = source.id AND source.id = 2 RETURNING (SELECT source.label) AS source_label, (SELECT target.item_label) AS deleted_label;
-- @end

-- @case create_nested_rule_rewrite_stop_fixture ok
CREATE TABLE automatic_nested_stop_base (id integer PRIMARY KEY, value integer); INSERT INTO automatic_nested_stop_base VALUES (1, 10), (2, 20); CREATE TABLE automatic_nested_stop_log (event text); CREATE TABLE automatic_nested_stop_capture (event text, affected bigint); CREATE RULE automatic_nested_stop_base_update AS ON UPDATE TO automatic_nested_stop_base DO ALSO INSERT INTO automatic_nested_stop_log VALUES ('base-update'); CREATE RULE automatic_nested_stop_base_delete AS ON DELETE TO automatic_nested_stop_base DO ALSO INSERT INTO automatic_nested_stop_log VALUES ('base-delete'); CREATE VIEW automatic_nested_stop_low AS SELECT id, value FROM automatic_nested_stop_base; CREATE RULE automatic_nested_stop_low_update AS ON UPDATE TO automatic_nested_stop_low DO ALSO INSERT INTO automatic_nested_stop_log VALUES ('low-update'); CREATE RULE automatic_nested_stop_low_delete AS ON DELETE TO automatic_nested_stop_low DO ALSO INSERT INTO automatic_nested_stop_log VALUES ('low-delete'); CREATE VIEW automatic_nested_stop_mid AS SELECT id, value FROM automatic_nested_stop_low; CREATE RULE automatic_nested_stop_mid_update AS ON UPDATE TO automatic_nested_stop_mid DO INSTEAD NOTHING; CREATE RULE automatic_nested_stop_mid_delete AS ON DELETE TO automatic_nested_stop_mid DO INSTEAD NOTHING; CREATE VIEW automatic_nested_stop_top AS SELECT id, value FROM automatic_nested_stop_mid;
-- @end

-- @case nested_instead_rules_stop_lower_rewrite_layers ok
DO $$ DECLARE affected bigint; BEGIN UPDATE automatic_nested_stop_top SET value = 11 WHERE id = 1; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_nested_stop_capture VALUES ('update', affected); DELETE FROM automatic_nested_stop_top WHERE id = 2; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_nested_stop_capture VALUES ('delete', affected); END $$;
-- @end

-- @case nested_rule_rewrite_stop_state rows
SELECT capture.event, capture.affected, (SELECT count(*) FROM automatic_nested_stop_log) AS log_count, (SELECT string_agg(id || ':' || value, ',' ORDER BY id) FROM automatic_nested_stop_base) AS base_rows FROM automatic_nested_stop_capture AS capture ORDER BY capture.event;
-- @end

-- @case create_omitted_computed_rule_image_fixture ok
CREATE TABLE automatic_omitted_image_base (id integer PRIMARY KEY); CREATE TABLE automatic_omitted_image_log (label text, value integer); CREATE VIEW automatic_omitted_image_view AS SELECT id, id * 2 AS doubled FROM automatic_omitted_image_base; CREATE RULE automatic_omitted_image_null AS ON INSERT TO automatic_omitted_image_view WHERE NEW.doubled IS NULL DO ALSO INSERT INTO automatic_omitted_image_log VALUES ('null', NEW.doubled); CREATE RULE automatic_omitted_image_value AS ON INSERT TO automatic_omitted_image_view WHERE NEW.doubled IS NOT NULL DO ALSO INSERT INTO automatic_omitted_image_log VALUES ('value', NEW.doubled);
-- @end

-- @case omitted_computed_insert_rule_image_stays_null ok
INSERT INTO automatic_omitted_image_view (id) VALUES (3);
-- @end

-- @case omitted_computed_rule_image_state rows
SELECT label, value FROM automatic_omitted_image_log;
-- @end

-- @case create_view_rule_conflict_fixture ok
CREATE TABLE automatic_rule_conflict_base (id integer PRIMARY KEY, value integer); INSERT INTO automatic_rule_conflict_base VALUES (1, 10); CREATE TABLE automatic_rule_conflict_log (id integer); CREATE TABLE automatic_rule_conflict_capture (affected bigint); CREATE VIEW automatic_rule_conflict_view AS SELECT id, value FROM automatic_rule_conflict_base; CREATE RULE automatic_rule_conflict_insert AS ON INSERT TO automatic_rule_conflict_view DO ALSO INSERT INTO automatic_rule_conflict_log VALUES (NEW.id);
-- @end

-- @case view_also_rule_allows_on_conflict ok
DO $$ DECLARE affected bigint; BEGIN INSERT INTO automatic_rule_conflict_view VALUES (1, 20) ON CONFLICT (id) DO NOTHING; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_rule_conflict_capture VALUES (affected); END $$;
-- @end

-- @case view_rule_conflict_state rows
SELECT capture.affected, base.value, (SELECT count(*) FROM automatic_rule_conflict_log) AS log_count FROM automatic_rule_conflict_capture AS capture CROSS JOIN automatic_rule_conflict_base AS base;
-- @end

-- @case create_suppressed_insert_select_fixture ok
CREATE TABLE automatic_lazy_select_base (id integer); CREATE TABLE automatic_lazy_select_capture (event text, affected bigint); CREATE VIEW automatic_lazy_select_direct AS SELECT id FROM automatic_lazy_select_base; CREATE RULE automatic_lazy_select_direct_rule AS ON INSERT TO automatic_lazy_select_direct DO INSTEAD NOTHING; CREATE VIEW automatic_lazy_select_inner AS SELECT id FROM automatic_lazy_select_base; CREATE RULE automatic_lazy_select_inner_rule AS ON INSERT TO automatic_lazy_select_inner DO INSTEAD NOTHING; CREATE VIEW automatic_lazy_select_outer AS SELECT id FROM automatic_lazy_select_inner;
-- @end

-- @case suppressed_insert_select_skips_unused_projection ok
DO $$ DECLARE affected bigint; BEGIN INSERT INTO automatic_lazy_select_direct SELECT 1 / 0; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_lazy_select_capture VALUES ('direct', affected); INSERT INTO automatic_lazy_select_outer SELECT 1 / 0; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_lazy_select_capture VALUES ('nested', affected); END $$;
-- @end

-- @case suppressed_insert_select_state rows
SELECT capture.event, capture.affected, (SELECT count(*) FROM automatic_lazy_select_base) AS base_count FROM automatic_lazy_select_capture AS capture ORDER BY capture.event;
-- @end

-- @case create_constant_insert_rule_cardinality_fixture ok
CREATE TABLE automatic_constant_insert_base (id integer); CREATE TABLE automatic_constant_insert_log (marker integer); CREATE VIEW automatic_constant_insert_view AS SELECT id FROM automatic_constant_insert_base; CREATE RULE automatic_constant_insert_rule AS ON INSERT TO automatic_constant_insert_view DO ALSO INSERT INTO automatic_constant_insert_log VALUES (1);
-- @end

-- @case constant_insert_rule_action_tracks_input_rows ok
INSERT INTO automatic_constant_insert_view VALUES (1), (2), (3);
-- @end

-- @case constant_insert_rule_cardinality_state rows
SELECT (SELECT count(*) FROM automatic_constant_insert_base) AS base_count, (SELECT count(*) FROM automatic_constant_insert_log) AS log_count;
-- @end

-- @case create_nested_suppressed_update_fixture ok
CREATE TABLE automatic_nested_lazy_base (id integer PRIMARY KEY, value integer); INSERT INTO automatic_nested_lazy_base VALUES (1, 10); CREATE TABLE automatic_nested_lazy_source (id integer PRIMARY KEY); INSERT INTO automatic_nested_lazy_source VALUES (1); CREATE TABLE automatic_nested_lazy_capture (event text, affected bigint); CREATE VIEW automatic_nested_lazy_inner AS SELECT id, value FROM automatic_nested_lazy_base; CREATE RULE automatic_nested_lazy_inner_update AS ON UPDATE TO automatic_nested_lazy_inner DO INSTEAD NOTHING; CREATE VIEW automatic_nested_lazy_outer AS SELECT id, value FROM automatic_nested_lazy_inner;
-- @end

-- @case nested_suppressed_updates_skip_assignments ok
DO $$ DECLARE affected bigint; BEGIN UPDATE automatic_nested_lazy_outer SET value = 1 / 0 WHERE id = 1; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_nested_lazy_capture VALUES ('plain', affected); UPDATE automatic_nested_lazy_outer SET value = 1 / 0 FROM automatic_nested_lazy_source AS source WHERE automatic_nested_lazy_outer.id = source.id; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_nested_lazy_capture VALUES ('from', affected); END $$;
-- @end

-- @case nested_suppressed_update_state rows
SELECT capture.event, capture.affected, base.value FROM automatic_nested_lazy_capture AS capture CROSS JOIN automatic_nested_lazy_base AS base ORDER BY capture.event;
-- @end

-- @case create_direct_suppressed_view_materialization_fixture ok
CREATE TABLE automatic_direct_lazy_base (id integer PRIMARY KEY); INSERT INTO automatic_direct_lazy_base VALUES (1); CREATE TABLE automatic_direct_lazy_capture (event text, affected bigint); CREATE VIEW automatic_direct_lazy_update AS SELECT id, 1 / (id - id) AS boom FROM automatic_direct_lazy_base; CREATE VIEW automatic_direct_lazy_delete AS SELECT id, 1 / (id - id) AS boom FROM automatic_direct_lazy_base; CREATE RULE automatic_direct_lazy_update_rule AS ON UPDATE TO automatic_direct_lazy_update DO INSTEAD NOTHING; CREATE RULE automatic_direct_lazy_delete_rule AS ON DELETE TO automatic_direct_lazy_delete DO INSTEAD NOTHING;
-- @end

-- @case direct_suppressed_view_dml_skips_materialization ok
DO $$ DECLARE affected bigint; BEGIN UPDATE automatic_direct_lazy_update SET id = id WHERE id = 1; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_direct_lazy_capture VALUES ('update', affected); DELETE FROM automatic_direct_lazy_delete WHERE id = 1; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_direct_lazy_capture VALUES ('delete', affected); END $$;
-- @end

-- @case direct_suppressed_view_dml_state rows
SELECT capture.event, capture.affected, (SELECT count(*) FROM automatic_direct_lazy_base) AS base_count FROM automatic_direct_lazy_capture AS capture ORDER BY capture.event;
-- @end

-- @case create_lazy_rule_case_fixture ok
CREATE TABLE automatic_lazy_case_base (id integer PRIMARY KEY); INSERT INTO automatic_lazy_case_base VALUES (1); CREATE TABLE automatic_lazy_case_log (id integer); CREATE VIEW automatic_lazy_case_view AS SELECT id, 1 / (id - id) AS boom FROM automatic_lazy_case_base; CREATE RULE automatic_lazy_case_rule AS ON UPDATE TO automatic_lazy_case_view WHERE CASE WHEN NEW.id = 1 THEN false ELSE NEW.boom > 0 END DO ALSO INSERT INTO automatic_lazy_case_log VALUES (NEW.id);
-- @end

-- @case rule_case_condition_short_circuits_projection ok
UPDATE automatic_lazy_case_view SET id = id WHERE id = 1;
-- @end

-- @case lazy_rule_case_state rows
SELECT (SELECT count(*) FROM automatic_lazy_case_base) AS base_count, (SELECT count(*) FROM automatic_lazy_case_log) AS log_count;
-- @end

-- @case create_correlated_source_only_scalar_fixture ok
CREATE TABLE automatic_source_scalar_base (id integer PRIMARY KEY, shown text NOT NULL, secret text NOT NULL); INSERT INTO automatic_source_scalar_base VALUES (1, 'before', 'base'); CREATE VIEW automatic_source_scalar_view AS SELECT id, shown FROM automatic_source_scalar_base;
-- @end

-- @case correlated_scalar_source_only_name_stays_source_bound rows
UPDATE automatic_source_scalar_view SET shown = (SELECT secret) FROM (SELECT 'source'::text AS secret) AS source WHERE id = 1 RETURNING shown;
-- @end

-- @case create_rule_catalog_flag_fixture ok
CREATE TABLE automatic_catalog_rule_base (id integer); CREATE VIEW automatic_catalog_rule_insert AS SELECT DISTINCT id FROM automatic_catalog_rule_base; CREATE RULE automatic_catalog_rule_insert_rule AS ON INSERT TO automatic_catalog_rule_insert DO INSTEAD NOTHING; CREATE VIEW automatic_catalog_rule_update AS SELECT DISTINCT id FROM automatic_catalog_rule_base; CREATE RULE automatic_catalog_rule_update_rule AS ON UPDATE TO automatic_catalog_rule_update DO INSTEAD NOTHING; CREATE VIEW automatic_catalog_rule_all AS SELECT DISTINCT id FROM automatic_catalog_rule_base; CREATE RULE automatic_catalog_rule_all_insert AS ON INSERT TO automatic_catalog_rule_all DO INSTEAD NOTHING; CREATE RULE automatic_catalog_rule_all_update AS ON UPDATE TO automatic_catalog_rule_all DO INSTEAD NOTHING; CREATE RULE automatic_catalog_rule_all_delete AS ON DELETE TO automatic_catalog_rule_all DO INSTEAD NOTHING;
-- @end

-- @case rule_backed_view_catalog_flags rows
SELECT view_flags.table_name, view_flags.is_updatable, view_flags.is_insertable_into, column_flags.is_updatable AS column_is_updatable FROM information_schema.views AS view_flags JOIN information_schema.columns AS column_flags ON column_flags.table_schema = view_flags.table_schema AND column_flags.table_name = view_flags.table_name WHERE view_flags.table_schema = current_schema() AND view_flags.table_name IN ('automatic_catalog_rule_insert', 'automatic_catalog_rule_update', 'automatic_catalog_rule_all') ORDER BY view_flags.table_name;
-- @end

-- @case create_view_check_error_order_fixture ok
CREATE TABLE automatic_check_order_base (id integer PRIMARY KEY, value integer); INSERT INTO automatic_check_order_base VALUES (1, 1), (2, 2); CREATE VIEW automatic_check_order_view AS SELECT id, value FROM automatic_check_order_base WHERE value > 0 WITH CHECK OPTION;
-- @end

-- @case view_insert_uniqueness_precedes_check_option error
INSERT INTO automatic_check_order_view VALUES (1, -1);
-- @end

-- @case view_update_uniqueness_precedes_check_option error
UPDATE automatic_check_order_view SET id = 1, value = -1 WHERE id = 2;
-- @end

-- @case create_duplicate_view_mapping_fixture ok
CREATE TABLE automatic_duplicate_map_base (id integer PRIMARY KEY, value integer); CREATE VIEW automatic_duplicate_map_view (first_id, second_id, value) AS SELECT id, id, value FROM automatic_duplicate_map_base;
-- @end

-- @case duplicate_mapped_insert_alias_is_syntax_error error
INSERT INTO automatic_duplicate_map_view (first_id, second_id, value) VALUES (1, 1, 2);
-- @end

-- @case duplicate_conflict_inference_mapping_is_accepted ok
INSERT INTO automatic_duplicate_map_view (first_id, value) VALUES (1, 2) ON CONFLICT (first_id, second_id) DO NOTHING;
-- @end

-- @case duplicate_view_mapping_state rows
SELECT id, value FROM automatic_duplicate_map_base;
-- @end

-- @case create_rule_action_cardinality_fixture ok
CREATE TABLE automatic_cardinality_update_base (id integer PRIMARY KEY); INSERT INTO automatic_cardinality_update_base VALUES (1), (2), (3); CREATE TABLE automatic_cardinality_update_log (marker text); CREATE RULE automatic_cardinality_update_rule AS ON UPDATE TO automatic_cardinality_update_base DO ALSO INSERT INTO automatic_cardinality_update_log VALUES ('updated'); CREATE TABLE automatic_cardinality_update_source (id integer); INSERT INTO automatic_cardinality_update_source VALUES (1), (2); CREATE TABLE automatic_cardinality_direct_base (id integer PRIMARY KEY); INSERT INTO automatic_cardinality_direct_base VALUES (1), (2), (3); CREATE TABLE automatic_cardinality_direct_log (marker text); CREATE VIEW automatic_cardinality_direct_view AS SELECT id FROM automatic_cardinality_direct_base; CREATE RULE automatic_cardinality_direct_rule AS ON UPDATE TO automatic_cardinality_direct_view DO INSTEAD INSERT INTO automatic_cardinality_direct_log VALUES ('updated'); CREATE TABLE automatic_cardinality_delete_base (id integer PRIMARY KEY); INSERT INTO automatic_cardinality_delete_base VALUES (1), (2), (3); CREATE TABLE automatic_cardinality_delete_log (marker text); CREATE RULE automatic_cardinality_delete_rule AS ON DELETE TO automatic_cardinality_delete_base DO ALSO INSERT INTO automatic_cardinality_delete_log VALUES ('deleted'); CREATE TABLE automatic_cardinality_capture (event text, affected bigint, action_count bigint);
-- @end

-- @case rule_action_cardinality_tracks_event_semantics ok
DO $$ DECLARE affected bigint; BEGIN UPDATE automatic_cardinality_update_base SET id = id; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_cardinality_capture SELECT 'update-all', affected, count(*) FROM automatic_cardinality_update_log; TRUNCATE automatic_cardinality_update_log; UPDATE automatic_cardinality_update_base SET id = id WHERE false; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_cardinality_capture SELECT 'update-none', affected, count(*) FROM automatic_cardinality_update_log; UPDATE automatic_cardinality_update_base SET id = id WHERE id > 0; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_cardinality_capture SELECT 'update-qualified', affected, count(*) FROM automatic_cardinality_update_log; TRUNCATE automatic_cardinality_update_log; UPDATE automatic_cardinality_update_base AS target SET id = target.id FROM automatic_cardinality_update_source AS source; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_cardinality_capture SELECT 'update-from-source', affected, count(*) FROM automatic_cardinality_update_log; TRUNCATE automatic_cardinality_update_log; UPDATE automatic_cardinality_update_base AS target SET id = target.id FROM automatic_cardinality_update_source AS source WHERE target.id = 1; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_cardinality_capture SELECT 'update-from-target', affected, count(*) FROM automatic_cardinality_update_log; UPDATE automatic_cardinality_direct_view AS target SET id = target.id FROM automatic_cardinality_update_source AS source; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_cardinality_capture SELECT 'direct-from-source', affected, count(*) FROM automatic_cardinality_direct_log; TRUNCATE automatic_cardinality_direct_log; UPDATE automatic_cardinality_direct_view AS target SET id = target.id FROM automatic_cardinality_update_source AS source WHERE target.id = 1; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_cardinality_capture SELECT 'direct-from-target', affected, count(*) FROM automatic_cardinality_direct_log; DELETE FROM automatic_cardinality_delete_base WHERE id > 1; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_cardinality_capture SELECT 'delete-two', affected, count(*) FROM automatic_cardinality_delete_log; TRUNCATE automatic_cardinality_delete_log; DELETE FROM automatic_cardinality_delete_base WHERE false; GET DIAGNOSTICS affected = ROW_COUNT; INSERT INTO automatic_cardinality_capture SELECT 'delete-none', affected, count(*) FROM automatic_cardinality_delete_log; END $$;
-- @end

-- @case rule_action_cardinality_state rows
SELECT event, affected, action_count FROM automatic_cardinality_capture ORDER BY event;
-- @end

-- @case create_check_option_nonupdatable_source_fixture ok
CREATE TABLE automatic_check_source_base (id integer PRIMARY KEY); CREATE VIEW automatic_check_source_inner AS SELECT DISTINCT id FROM automatic_check_source_base; CREATE VIEW automatic_check_source_outer AS SELECT id FROM automatic_check_source_inner WHERE id > 0 WITH CHECK OPTION;
-- @end

-- @case check_option_nonupdatable_source_catalog rows
SELECT is_updatable, is_insertable_into FROM information_schema.views WHERE table_schema = current_schema() AND table_name = 'automatic_check_source_outer';
-- @end

-- @case create_only_partition_view_fixture ok
CREATE TABLE automatic_only_partition_base (id integer, value integer) PARTITION BY RANGE (id); CREATE TABLE automatic_only_partition_child PARTITION OF automatic_only_partition_base FOR VALUES FROM (0) TO (10); CREATE VIEW automatic_only_partition_view AS SELECT id, value FROM ONLY automatic_only_partition_base; INSERT INTO automatic_only_partition_view VALUES (1, 10);
-- @end

-- @case only_partition_view_insert_routes_to_child rows
SELECT (SELECT count(*) FROM automatic_only_partition_child) AS child_count, (SELECT count(*) FROM automatic_only_partition_view) AS view_count;
-- @end

-- @case create_nested_computed_rule_fixture ok
CREATE TABLE automatic_nested_computed_base (id integer PRIMARY KEY, value integer); CREATE TABLE automatic_nested_computed_log (value integer); CREATE VIEW automatic_nested_computed_inner AS SELECT id, value * 2 AS doubled FROM automatic_nested_computed_base; CREATE RULE automatic_nested_computed_insert AS ON INSERT TO automatic_nested_computed_inner DO INSTEAD INSERT INTO automatic_nested_computed_log VALUES (NEW.doubled); CREATE RULE automatic_nested_computed_update AS ON UPDATE TO automatic_nested_computed_inner DO INSTEAD INSERT INTO automatic_nested_computed_log VALUES (NEW.doubled); CREATE VIEW automatic_nested_computed_outer AS SELECT id, doubled FROM automatic_nested_computed_inner;
-- @end

-- @case nested_rules_consume_outer_computed_inputs ok
INSERT INTO automatic_nested_computed_outer VALUES (1, 8); INSERT INTO automatic_nested_computed_base VALUES (2, 2); UPDATE automatic_nested_computed_outer SET doubled = 12 WHERE id = 2;
-- @end

-- @case nested_computed_rule_state rows
SELECT (SELECT string_agg(value::text, ',' ORDER BY value) FROM automatic_nested_computed_log) AS logged, string_agg(table_name || ':' || is_updatable || ':' || is_insertable_into, ',' ORDER BY table_name) AS flags FROM information_schema.views WHERE table_schema = current_schema() AND table_name IN ('automatic_nested_computed_inner', 'automatic_nested_computed_outer');
-- @end

-- @case create_nested_nonautomatic_rule_fixture ok
CREATE TABLE automatic_nested_aggregate_base (value integer); INSERT INTO automatic_nested_aggregate_base VALUES (1), (2); CREATE TABLE automatic_nested_aggregate_log (value integer); CREATE VIEW automatic_nested_aggregate_inner AS SELECT sum(value)::integer AS total FROM automatic_nested_aggregate_base; CREATE RULE automatic_nested_aggregate_update AS ON UPDATE TO automatic_nested_aggregate_inner DO INSTEAD INSERT INTO automatic_nested_aggregate_log VALUES (NEW.total); CREATE VIEW automatic_nested_aggregate_outer AS SELECT total FROM automatic_nested_aggregate_inner;
-- @end

-- @case nested_nonautomatic_rule_executes ok
UPDATE automatic_nested_aggregate_outer SET total = 9 WHERE total = 3;
-- @end

-- @case nested_nonautomatic_rule_state rows
SELECT (SELECT value FROM automatic_nested_aggregate_log) AS logged, string_agg(table_name || ':' || is_updatable || ':' || is_insertable_into, ',' ORDER BY table_name) AS flags FROM information_schema.views WHERE table_schema = current_schema() AND table_name IN ('automatic_nested_aggregate_inner', 'automatic_nested_aggregate_outer');
-- @end

-- @case create_consumed_lazy_rule_fixture ok
CREATE TABLE automatic_consumed_lazy_base (id integer PRIMARY KEY, value integer); INSERT INTO automatic_consumed_lazy_base VALUES (1, 10); CREATE TABLE automatic_consumed_lazy_log (event text, id integer); CREATE VIEW automatic_consumed_lazy_update AS SELECT id, 1 / (id - id) AS boom FROM automatic_consumed_lazy_base; CREATE RULE automatic_consumed_lazy_update_rule AS ON UPDATE TO automatic_consumed_lazy_update DO INSTEAD INSERT INTO automatic_consumed_lazy_log VALUES ('update', NEW.id); CREATE VIEW automatic_consumed_lazy_insert AS SELECT id, value FROM automatic_consumed_lazy_base; CREATE RULE automatic_consumed_lazy_insert_rule AS ON INSERT TO automatic_consumed_lazy_insert DO INSTEAD INSERT INTO automatic_consumed_lazy_log VALUES ('insert-select', NEW.id); CREATE VIEW automatic_consumed_lazy_inner AS SELECT id, value FROM automatic_consumed_lazy_base; CREATE RULE automatic_consumed_lazy_inner_rule AS ON INSERT TO automatic_consumed_lazy_inner DO INSTEAD INSERT INTO automatic_consumed_lazy_log VALUES ('nested-values', NEW.id); CREATE VIEW automatic_consumed_lazy_outer AS SELECT id, value FROM automatic_consumed_lazy_inner;
-- @end

-- @case consumed_rule_inputs_skip_unused_expressions ok
UPDATE automatic_consumed_lazy_update SET id = id WHERE id = 1; INSERT INTO automatic_consumed_lazy_insert SELECT 2, 1 / 0; INSERT INTO automatic_consumed_lazy_outer VALUES (3, 1 / 0);
-- @end

-- @case consumed_lazy_rule_state rows
SELECT event, id FROM automatic_consumed_lazy_log ORDER BY id;
-- @end

-- @case create_replication_mode_catalog_fixture ok
CREATE TABLE automatic_mode_catalog_base (id integer); CREATE VIEW automatic_mode_catalog_view AS SELECT DISTINCT id FROM automatic_mode_catalog_base; CREATE RULE automatic_mode_catalog_insert AS ON INSERT TO automatic_mode_catalog_view DO INSTEAD NOTHING;
-- @end

-- @case set_replica_for_rule_catalog ok
SET session_replication_role = replica;
-- @end

-- @case rule_catalog_flags_ignore_replication_mode rows
SELECT is_insertable_into FROM information_schema.views WHERE table_schema = current_schema() AND table_name = 'automatic_mode_catalog_view';
-- @end

-- @case reset_replica_for_rule_catalog ok
RESET session_replication_role;
-- @end

-- @case select_star_without_relation_rejected error
SELECT *;
-- @end

-- @case create_view_star_without_relation_rejected error
CREATE VIEW automatic_invalid_star_view AS SELECT *;
-- @end

-- @case create_nonautomatic_rule_boundary_fixture ok
CREATE TABLE automatic_rule_boundary_base (id integer); INSERT INTO automatic_rule_boundary_base VALUES (1), (2); CREATE TABLE automatic_rule_boundary_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, event text, value integer); CREATE VIEW automatic_rule_boundary_insert_inner AS SELECT DISTINCT id FROM automatic_rule_boundary_base; CREATE RULE automatic_rule_boundary_insert_inner_rule AS ON INSERT TO automatic_rule_boundary_insert_inner DO INSTEAD INSERT INTO automatic_rule_boundary_log(event, value) VALUES ('inner-insert', NEW.id); CREATE VIEW automatic_rule_boundary_insert_outer AS SELECT id FROM automatic_rule_boundary_insert_inner; CREATE RULE automatic_rule_boundary_insert_outer_rule AS ON INSERT TO automatic_rule_boundary_insert_outer DO ALSO INSERT INTO automatic_rule_boundary_log(event, value) VALUES ('outer-insert', NEW.id); CREATE VIEW automatic_rule_boundary_update_inner AS SELECT sum(id)::integer AS total FROM automatic_rule_boundary_base; CREATE RULE automatic_rule_boundary_update_inner_rule AS ON UPDATE TO automatic_rule_boundary_update_inner DO INSTEAD INSERT INTO automatic_rule_boundary_log(event, value) VALUES ('inner-update', NEW.total); CREATE VIEW automatic_rule_boundary_update_outer AS SELECT total FROM automatic_rule_boundary_update_inner; CREATE RULE automatic_rule_boundary_update_outer_rule AS ON UPDATE TO automatic_rule_boundary_update_outer DO ALSO INSERT INTO automatic_rule_boundary_log(event, value) VALUES ('outer-update', NEW.total); CREATE VIEW automatic_rule_boundary_delete_inner AS SELECT DISTINCT id FROM automatic_rule_boundary_base; CREATE RULE automatic_rule_boundary_delete_inner_rule AS ON DELETE TO automatic_rule_boundary_delete_inner DO INSTEAD INSERT INTO automatic_rule_boundary_log(event, value) VALUES ('inner-delete', OLD.id); CREATE VIEW automatic_rule_boundary_delete_outer AS SELECT id FROM automatic_rule_boundary_delete_inner; CREATE RULE automatic_rule_boundary_delete_outer_rule AS ON DELETE TO automatic_rule_boundary_delete_outer DO ALSO INSERT INTO automatic_rule_boundary_log(event, value) VALUES ('outer-delete', OLD.id);
-- @end

-- @case nonautomatic_rule_boundary_preserves_outer_layers ok
INSERT INTO automatic_rule_boundary_insert_outer VALUES (3); UPDATE automatic_rule_boundary_update_outer SET total = 9; DELETE FROM automatic_rule_boundary_delete_outer WHERE id = 1;
-- @end

-- @case nonautomatic_rule_boundary_order rows
SELECT event, value FROM automatic_rule_boundary_log ORDER BY seq;
-- @end

-- @case create_automatic_merge_view_fixture ok
CREATE TABLE automatic_merge_base (id integer PRIMARY KEY, value integer, hidden text DEFAULT 'defaulted'); INSERT INTO automatic_merge_base VALUES (1, 10, 'one'), (2, 20, 'two'), (4, 140, 'outside'); CREATE VIEW automatic_merge_inner (item_id, value, computed) AS SELECT id, value, value + 1 FROM automatic_merge_base; CREATE VIEW automatic_merge_outer (id, visible_value, doubled_computed) AS SELECT item_id, value, computed * 2 FROM automatic_merge_inner WHERE value < 100; CREATE TABLE automatic_merge_source (id integer, value integer);
-- @end

-- @case prepare_automatic_merge_update_source ok
INSERT INTO automatic_merge_source VALUES (1, 30);
-- @end

-- @case automatic_merge_view_update_returning rows
MERGE INTO automatic_merge_outer AS target USING automatic_merge_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET visible_value = source.value RETURNING merge_action(), source.id, target.id, target.visible_value, target.doubled_computed, old.visible_value, new.visible_value;
-- @end

-- @case prepare_automatic_merge_insert_source ok
TRUNCATE automatic_merge_source; INSERT INTO automatic_merge_source VALUES (3, 40);
-- @end

-- @case automatic_merge_view_insert_returning rows
MERGE INTO automatic_merge_outer AS target USING automatic_merge_source AS source ON target.id = source.id WHEN NOT MATCHED THEN INSERT (id, visible_value) VALUES (source.id, source.value) RETURNING merge_action(), source.id, target.id, target.visible_value, target.doubled_computed, old.visible_value, new.visible_value;
-- @end

-- @case prepare_automatic_merge_delete_source ok
TRUNCATE automatic_merge_source; INSERT INTO automatic_merge_source VALUES (1, 30), (3, 40);
-- @end

-- @case automatic_merge_view_delete_returning rows
MERGE INTO automatic_merge_outer AS target USING automatic_merge_source AS source ON target.id = source.id WHEN MATCHED THEN DO NOTHING WHEN NOT MATCHED BY SOURCE THEN DELETE RETURNING merge_action(), source.id, target.id, target.visible_value, target.doubled_computed, old.visible_value, new.visible_value;
-- @end

-- @case automatic_merge_view_state rows
SELECT id, value, hidden FROM automatic_merge_base ORDER BY id;
-- @end

-- @case prepare_automatic_merge_leave_source ok
TRUNCATE automatic_merge_source; INSERT INTO automatic_merge_source VALUES (1, 130);
-- @end

-- @case automatic_merge_view_can_leave_filter rows
MERGE INTO automatic_merge_outer AS target USING automatic_merge_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET visible_value = source.value RETURNING merge_action(), target.id, target.visible_value, target.doubled_computed;
-- @end

-- @case prepare_automatic_merge_star_source ok
TRUNCATE automatic_merge_source; INSERT INTO automatic_merge_source VALUES (3, 45);
-- @end

-- @case automatic_merge_view_bare_returning_star rows
MERGE INTO automatic_merge_outer AS target USING automatic_merge_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET visible_value = source.value RETURNING *;
-- @end

-- @case automatic_merge_view_filter_state rows
SELECT (SELECT string_agg(id::text || ':' || value::text, ',' ORDER BY id) FROM automatic_merge_base) AS base_rows, (SELECT string_agg(id::text || ':' || visible_value::text, ',' ORDER BY id) FROM automatic_merge_outer) AS visible_rows;
-- @end

-- @case create_automatic_merge_check_fixture ok
CREATE TABLE automatic_merge_check_base (id integer PRIMARY KEY, value integer); INSERT INTO automatic_merge_check_base VALUES (1, 10); CREATE VIEW automatic_merge_check_view AS SELECT id, value, value + 1 AS computed FROM automatic_merge_check_base WHERE value > 0 WITH CASCADED CHECK OPTION; CREATE FUNCTION automatic_merge_check_mutate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.value = 40 THEN NEW.value := -40; END IF; RETURN NEW; END; $$; CREATE TRIGGER automatic_merge_check_mutate_before BEFORE UPDATE ON automatic_merge_check_base FOR EACH ROW EXECUTE FUNCTION automatic_merge_check_mutate(); CREATE TABLE automatic_merge_check_source (id integer, value integer); INSERT INTO automatic_merge_check_source VALUES (1, -1), (2, -2); CREATE MATERIALIZED VIEW automatic_merge_materialized AS SELECT id, value FROM automatic_merge_check_base; CREATE VIEW automatic_merge_aggregate AS SELECT sum(value)::integer AS value FROM automatic_merge_check_base;
-- @end

-- @case automatic_merge_update_check_option error
MERGE INTO automatic_merge_check_view AS target USING (SELECT id, value FROM automatic_merge_check_source WHERE id = 1) AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = source.value;
-- @end

-- @case automatic_merge_insert_check_option error
MERGE INTO automatic_merge_check_view AS target USING (SELECT id, value FROM automatic_merge_check_source WHERE id = 2) AS source ON target.id = source.id WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value);
-- @end

-- @case prepare_automatic_merge_post_trigger_check_source ok
UPDATE automatic_merge_check_source SET value = 40 WHERE id = 1;
-- @end

-- @case automatic_merge_update_post_trigger_check_option error
MERGE INTO automatic_merge_check_view AS target USING (SELECT id, value FROM automatic_merge_check_source WHERE id = 1) AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = source.value;
-- @end

-- @case automatic_merge_post_trigger_check_is_atomic rows
SELECT id, value FROM automatic_merge_check_base ORDER BY id;
-- @end

-- @case automatic_merge_computed_target_rejected error
MERGE INTO automatic_merge_check_view AS target USING automatic_merge_check_source AS source ON false WHEN NOT MATCHED THEN INSERT (id, computed) VALUES (source.id, source.value);
-- @end

-- @case create_automatic_merge_rule_fixture ok
CREATE TABLE automatic_merge_rule_log (value integer); CREATE RULE automatic_merge_rule AS ON UPDATE TO automatic_merge_check_view DO ALSO INSERT INTO automatic_merge_rule_log VALUES (NEW.value);
-- @end

-- @case automatic_merge_rule_target_rejected error
MERGE INTO automatic_merge_check_view AS target USING automatic_merge_check_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = source.value;
-- @end

-- @case automatic_merge_materialized_target_rejected error
MERGE INTO automatic_merge_materialized AS target USING automatic_merge_check_source AS source ON target.id = source.id WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, source.value);
-- @end

-- @case automatic_merge_aggregate_target_rejected error
MERGE INTO automatic_merge_aggregate AS target USING automatic_merge_check_source AS source ON false WHEN NOT MATCHED THEN INSERT (value) VALUES (source.value);
-- @end

-- @case automatic_merge_errors_are_atomic rows
SELECT (SELECT string_agg(id::text || ':' || value::text, ',' ORDER BY id) FROM automatic_merge_check_base) AS base_rows, (SELECT count(*) FROM automatic_merge_rule_log) AS rule_rows;
-- @end

-- @case create_automatic_merge_delete_only_fixture ok
CREATE TABLE automatic_merge_delete_base (value integer); INSERT INTO automatic_merge_delete_base VALUES (1), (2); CREATE VIEW automatic_merge_delete_view AS SELECT value + 1 AS computed FROM automatic_merge_delete_base; CREATE TABLE automatic_merge_delete_source (computed integer); INSERT INTO automatic_merge_delete_source VALUES (2);
-- @end

-- @case automatic_merge_delete_only_computed_view rows
MERGE INTO automatic_merge_delete_view AS target USING automatic_merge_delete_source AS source ON target.computed = source.computed WHEN NOT MATCHED BY SOURCE THEN DELETE RETURNING merge_action(), target.computed;
-- @end

-- @case automatic_merge_delete_only_state rows
SELECT value FROM automatic_merge_delete_base ORDER BY value;
-- @end

-- @case create_automatic_merge_only_partition_fixture ok
CREATE TABLE automatic_merge_only_parent (id integer, value integer) PARTITION BY RANGE (id); CREATE TABLE automatic_merge_only_child PARTITION OF automatic_merge_only_parent FOR VALUES FROM (0) TO (10); CREATE VIEW automatic_merge_only_view AS SELECT id, value FROM ONLY automatic_merge_only_parent; CREATE TABLE automatic_merge_only_source (id integer, value integer); INSERT INTO automatic_merge_only_source VALUES (1, 10);
-- @end

-- @case automatic_merge_only_view_insert_routes_child ok
MERGE INTO automatic_merge_only_view AS target USING automatic_merge_only_source AS source ON false WHEN NOT MATCHED THEN INSERT VALUES (source.id, source.value);
-- @end

-- @case automatic_merge_only_view_state rows
SELECT (SELECT count(*) FROM automatic_merge_only_child) AS child_count, (SELECT count(*) FROM automatic_merge_only_view) AS view_count;
-- @end
