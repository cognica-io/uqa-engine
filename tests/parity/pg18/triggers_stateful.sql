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
