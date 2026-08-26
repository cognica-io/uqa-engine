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
