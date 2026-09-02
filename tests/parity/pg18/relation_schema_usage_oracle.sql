\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rel_hidden CASCADE;
DROP SCHEMA IF EXISTS uqa_rel_visible CASCADE;
DROP ROLE IF EXISTS uqa_rel_caller;
DROP ROLE IF EXISTS uqa_rel_group;
DROP ROLE IF EXISTS uqa_rel_member;

CREATE ROLE uqa_rel_caller;
CREATE ROLE uqa_rel_group;
CREATE ROLE uqa_rel_member INHERIT;
GRANT uqa_rel_group TO uqa_rel_member;
CREATE SCHEMA uqa_rel_hidden;
CREATE SCHEMA uqa_rel_visible;
REVOKE ALL ON SCHEMA uqa_rel_hidden, uqa_rel_visible FROM PUBLIC;

CREATE OR REPLACE FUNCTION pg_temp.rel_scalar_probe(label text, role_name text, path text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
    message text;
    result text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    PERFORM set_config('search_path', path, true);
    EXECUTE command INTO result;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|ok|' || COALESCE(result, 'NULL');
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|' || state || '|' || message;
END
$oracle$;

CREATE OR REPLACE FUNCTION pg_temp.rel_command_probe(label text, role_name text, path text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
    message text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    PERFORM set_config('search_path', path, true);
    EXECUTE command;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|' || state || '|' || message;
END
$oracle$;

CREATE TABLE uqa_rel_hidden.pick(id integer);
INSERT INTO uqa_rel_hidden.pick VALUES (1);
CREATE TABLE uqa_rel_visible.pick(id integer);
INSERT INTO uqa_rel_visible.pick VALUES (2);
CREATE TABLE uqa_rel_hidden.only_table(id integer);
INSERT INTO uqa_rel_hidden.only_table VALUES (7);
CREATE TABLE uqa_rel_hidden.ddl_table(id integer);
ALTER TABLE uqa_rel_hidden.ddl_table OWNER TO uqa_rel_caller;
CREATE VIEW uqa_rel_visible.bound_view AS SELECT id FROM uqa_rel_hidden.only_table;
CREATE VIEW uqa_rel_visible.invoker_view WITH (security_invoker=true) AS SELECT id FROM uqa_rel_hidden.only_table;
CREATE MATERIALIZED VIEW uqa_rel_visible.bound_matview AS SELECT id FROM uqa_rel_hidden.only_table;
CREATE FUNCTION uqa_rel_visible.bound_function() RETURNS integer LANGUAGE SQL RETURN (SELECT id FROM uqa_rel_hidden.only_table);
CREATE FUNCTION uqa_rel_visible.dynamic_invoker() RETURNS integer LANGUAGE SQL SECURITY INVOKER AS 'SELECT id FROM uqa_rel_hidden.only_table';
CREATE FUNCTION uqa_rel_visible.dynamic_definer() RETURNS integer LANGUAGE SQL SECURITY DEFINER AS 'SELECT id FROM uqa_rel_hidden.only_table';

GRANT USAGE, CREATE ON SCHEMA uqa_rel_hidden, uqa_rel_visible TO uqa_rel_caller;
SET ROLE uqa_rel_caller;
CREATE TABLE uqa_rel_hidden.event_items(id integer);
CREATE TABLE uqa_rel_visible.event_items(id integer);
CREATE TABLE uqa_rel_hidden.event_log(id integer);
CREATE TABLE uqa_rel_hidden.event_updates(id integer);
INSERT INTO uqa_rel_hidden.event_updates VALUES (0);
CREATE TABLE uqa_rel_hidden.event_deletes(id integer);
INSERT INTO uqa_rel_hidden.event_deletes VALUES (41);
CREATE TABLE uqa_rel_hidden.parent_rows(id integer);
CREATE TABLE uqa_rel_visible.parent_rows(id integer);
CREATE TABLE uqa_rel_hidden.reference_rows(id integer PRIMARY KEY);
INSERT INTO uqa_rel_hidden.reference_rows VALUES (2);
CREATE TABLE uqa_rel_visible.reference_rows(id integer PRIMARY KEY);
CREATE TABLE uqa_rel_hidden.partitioned_rows(id integer) PARTITION BY RANGE(id);
CREATE TABLE uqa_rel_visible.partitioned_rows(id integer) PARTITION BY RANGE(id);
CREATE TABLE uqa_rel_hidden.attach_rows(id integer);
CREATE TABLE uqa_rel_visible.alter_rows(id integer);
CREATE FUNCTION uqa_rel_visible.event_trigger() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END';
CREATE FUNCTION uqa_rel_hidden.bound_event_trigger() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END';
CREATE TRIGGER bound_function_trigger BEFORE INSERT ON uqa_rel_visible.event_items FOR EACH ROW EXECUTE FUNCTION uqa_rel_hidden.bound_event_trigger();
RESET ROLE;
REVOKE USAGE, CREATE ON SCHEMA uqa_rel_hidden FROM uqa_rel_caller;

GRANT SELECT, INSERT, UPDATE, DELETE, TRUNCATE ON uqa_rel_hidden.pick, uqa_rel_hidden.only_table TO uqa_rel_caller;
GRANT SELECT ON uqa_rel_visible.pick, uqa_rel_visible.bound_view, uqa_rel_visible.invoker_view, uqa_rel_visible.bound_matview TO uqa_rel_caller;
GRANT USAGE, CREATE ON SCHEMA uqa_rel_visible TO uqa_rel_caller;

SELECT pg_temp.rel_scalar_probe('unqualified-skip', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'SELECT id FROM pick');
SELECT pg_temp.rel_scalar_probe('unqualified-missing', 'uqa_rel_caller', 'uqa_rel_hidden,pg_catalog', 'SELECT id FROM only_table');
SELECT pg_temp.rel_scalar_probe('qualified-existing', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_hidden.only_table');
SELECT pg_temp.rel_scalar_probe('qualified-missing', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_hidden.missing_table');
SELECT pg_temp.rel_scalar_probe('missing-schema', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_absent.missing_table');
SELECT pg_temp.rel_scalar_probe('column-precedence', 'uqa_rel_caller', 'pg_catalog', 'SELECT missing_column FROM uqa_rel_hidden.only_table');
SELECT pg_temp.rel_scalar_probe('where-column-precedence', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_hidden.only_table WHERE missing_column = 1');
SELECT pg_temp.rel_command_probe('insert-schema', 'uqa_rel_caller', 'pg_catalog', 'INSERT INTO uqa_rel_hidden.only_table VALUES (9)');
SELECT pg_temp.rel_command_probe('update-schema', 'uqa_rel_caller', 'pg_catalog', 'UPDATE uqa_rel_hidden.only_table SET id = 9');
SELECT pg_temp.rel_command_probe('delete-schema', 'uqa_rel_caller', 'pg_catalog', 'DELETE FROM uqa_rel_hidden.only_table');
SELECT pg_temp.rel_command_probe('truncate-schema', 'uqa_rel_caller', 'pg_catalog', 'TRUNCATE uqa_rel_hidden.only_table');
SELECT pg_temp.rel_command_probe('alter-schema', 'uqa_rel_caller', 'pg_catalog', 'ALTER TABLE uqa_rel_hidden.ddl_table ADD COLUMN marker integer');
SELECT pg_temp.rel_command_probe('drop-schema', 'uqa_rel_caller', 'pg_catalog', 'DROP TABLE uqa_rel_hidden.ddl_table');
SELECT pg_temp.rel_scalar_probe('regclass-hard', 'uqa_rel_caller', 'pg_catalog', $$SELECT 'uqa_rel_hidden.only_table'::regclass::oid::text$$);
SELECT pg_temp.rel_scalar_probe('regclass-soft', 'uqa_rel_caller', 'pg_catalog', $$SELECT to_regclass('uqa_rel_hidden.only_table')::oid::text$$);
SELECT pg_temp.rel_scalar_probe('bound-view', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_visible.bound_view');
SELECT pg_temp.rel_scalar_probe('invoker-view', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_visible.invoker_view');
SELECT pg_temp.rel_scalar_probe('bound-matview', 'uqa_rel_caller', 'pg_catalog', 'SELECT id FROM uqa_rel_visible.bound_matview');
SELECT pg_temp.rel_scalar_probe('bound-function', 'uqa_rel_caller', 'pg_catalog', 'SELECT uqa_rel_visible.bound_function()');
SELECT pg_temp.rel_scalar_probe('dynamic-invoker', 'uqa_rel_caller', 'pg_catalog', 'SELECT uqa_rel_visible.dynamic_invoker()');
SELECT pg_temp.rel_scalar_probe('dynamic-definer', 'uqa_rel_caller', 'pg_catalog', 'SELECT uqa_rel_visible.dynamic_definer()');
SELECT pg_temp.rel_command_probe('bound-trigger-function', 'uqa_rel_caller', 'pg_catalog', 'INSERT INTO uqa_rel_visible.event_items VALUES (73)');
SELECT pg_temp.rel_command_probe('trigger-qualified', 'uqa_rel_caller', 'pg_catalog', 'CREATE TRIGGER hidden_trigger BEFORE INSERT ON uqa_rel_hidden.event_items FOR EACH ROW EXECUTE FUNCTION uqa_rel_visible.event_trigger()');
SELECT pg_temp.rel_command_probe('trigger-qualified-missing', 'uqa_rel_caller', 'pg_catalog', 'CREATE TRIGGER hidden_missing_trigger BEFORE INSERT ON uqa_rel_hidden.missing_items FOR EACH ROW EXECUTE FUNCTION uqa_rel_visible.event_trigger()');
SELECT pg_temp.rel_command_probe('trigger-unqualified-skip', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'CREATE TRIGGER visible_trigger BEFORE INSERT ON event_items FOR EACH ROW EXECUTE FUNCTION uqa_rel_visible.event_trigger()');
SELECT pg_temp.rel_command_probe('trigger-drop-qualified', 'uqa_rel_caller', 'pg_catalog', 'DROP TRIGGER visible_trigger ON uqa_rel_hidden.event_items');
SELECT pg_temp.rel_command_probe('trigger-drop-unqualified-skip', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'DROP TRIGGER visible_trigger ON event_items');
SELECT pg_temp.rel_command_probe('trigger-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'CREATE TRIGGER missing_schema_trigger BEFORE INSERT ON uqa_rel_absent.event_items FOR EACH ROW EXECUTE FUNCTION uqa_rel_visible.event_trigger()');
SELECT pg_temp.rel_command_probe('trigger-drop-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'DROP TRIGGER missing_trigger ON uqa_rel_absent.event_items');
SELECT pg_temp.rel_command_probe('trigger-drop-if-exists-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'DROP TRIGGER IF EXISTS missing_trigger ON uqa_rel_absent.event_items');
SELECT pg_temp.rel_command_probe('trigger-drop-if-exists-missing-relation', 'uqa_rel_caller', 'pg_catalog', 'DROP TRIGGER IF EXISTS missing_trigger ON uqa_rel_visible.missing_items');
SELECT pg_temp.rel_command_probe('rule-qualified', 'uqa_rel_caller', 'pg_catalog', 'CREATE RULE hidden_rule AS ON INSERT TO uqa_rel_hidden.event_items DO ALSO NOTHING');
SELECT pg_temp.rel_command_probe('rule-qualified-missing', 'uqa_rel_caller', 'pg_catalog', 'CREATE RULE hidden_missing_rule AS ON INSERT TO uqa_rel_hidden.missing_items DO ALSO NOTHING');
SELECT pg_temp.rel_command_probe('rule-unqualified-skip', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'CREATE RULE visible_rule AS ON INSERT TO event_items DO ALSO NOTHING');
SELECT pg_temp.rel_command_probe('rule-drop-qualified', 'uqa_rel_caller', 'pg_catalog', 'DROP RULE visible_rule ON uqa_rel_hidden.event_items');
SELECT pg_temp.rel_command_probe('rule-drop-unqualified-skip', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'DROP RULE visible_rule ON event_items');
SELECT pg_temp.rel_command_probe('rule-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'CREATE RULE missing_schema_rule AS ON INSERT TO uqa_rel_absent.event_items DO ALSO NOTHING');
SELECT pg_temp.rel_command_probe('rule-drop-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'DROP RULE missing_rule ON uqa_rel_absent.event_items');
SELECT pg_temp.rel_command_probe('rule-drop-if-exists-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'DROP RULE IF EXISTS missing_rule ON uqa_rel_absent.event_items');
SELECT pg_temp.rel_command_probe('rule-drop-if-exists-missing-relation', 'uqa_rel_caller', 'pg_catalog', 'DROP RULE IF EXISTS missing_rule ON uqa_rel_visible.missing_items');
SELECT pg_temp.rel_command_probe('rule-action-qualified', 'uqa_rel_caller', 'pg_catalog', 'CREATE RULE hidden_action_rule AS ON INSERT TO uqa_rel_visible.event_items DO ALSO INSERT INTO uqa_rel_hidden.event_log VALUES (NEW.id)');
SELECT pg_temp.rel_command_probe('rule-action-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'CREATE RULE missing_action_rule AS ON INSERT TO uqa_rel_visible.event_items DO ALSO INSERT INTO uqa_rel_absent.event_log VALUES (NEW.id)');
SELECT pg_temp.rel_command_probe('constraint-trigger-from-qualified', 'uqa_rel_caller', 'pg_catalog', 'CREATE CONSTRAINT TRIGGER hidden_from_trigger AFTER INSERT ON uqa_rel_visible.event_items FROM uqa_rel_hidden.event_items DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION uqa_rel_visible.event_trigger()');
SELECT pg_temp.rel_command_probe('constraint-trigger-from-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'CREATE CONSTRAINT TRIGGER missing_from_trigger AFTER INSERT ON uqa_rel_visible.event_items FROM uqa_rel_absent.event_items DEFERRABLE INITIALLY IMMEDIATE FOR EACH ROW EXECUTE FUNCTION uqa_rel_visible.event_trigger()');
SELECT pg_temp.rel_command_probe('trigger-function-qualified', 'uqa_rel_caller', 'pg_catalog', 'CREATE TRIGGER hidden_function_trigger BEFORE INSERT ON uqa_rel_visible.event_items FOR EACH ROW EXECUTE FUNCTION uqa_rel_hidden.bound_event_trigger()');
SELECT pg_temp.rel_command_probe('trigger-function-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'CREATE TRIGGER missing_function_trigger BEFORE INSERT ON uqa_rel_visible.event_items FOR EACH ROW EXECUTE FUNCTION uqa_rel_absent.event_trigger()');
SELECT pg_temp.rel_command_probe('inherits-qualified', 'uqa_rel_caller', 'pg_catalog', 'CREATE TABLE uqa_rel_visible.inherits_hidden() INHERITS (uqa_rel_hidden.parent_rows)');
SELECT pg_temp.rel_command_probe('inherits-unqualified-skip', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'CREATE TABLE uqa_rel_visible.inherits_visible() INHERITS (parent_rows)');
SELECT pg_temp.rel_command_probe('inherits-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'CREATE TABLE uqa_rel_visible.inherits_missing() INHERITS (uqa_rel_absent.parent_rows)');
SELECT pg_temp.rel_command_probe('partition-qualified', 'uqa_rel_caller', 'pg_catalog', 'CREATE TABLE uqa_rel_visible.partition_hidden PARTITION OF uqa_rel_hidden.partitioned_rows FOR VALUES FROM (0) TO (10)');
SELECT pg_temp.rel_command_probe('partition-unqualified-skip', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'CREATE TABLE uqa_rel_visible.partition_visible PARTITION OF partitioned_rows FOR VALUES FROM (0) TO (10)');
SELECT pg_temp.rel_command_probe('partition-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'CREATE TABLE uqa_rel_visible.partition_missing PARTITION OF uqa_rel_absent.partitioned_rows FOR VALUES FROM (0) TO (10)');
SELECT pg_temp.rel_command_probe('fk-column-qualified', 'uqa_rel_caller', 'pg_catalog', 'CREATE TABLE uqa_rel_visible.fk_column(id integer REFERENCES uqa_rel_hidden.reference_rows(id))');
SELECT pg_temp.rel_command_probe('fk-table-qualified', 'uqa_rel_caller', 'pg_catalog', 'CREATE TABLE uqa_rel_visible.fk_table(id integer, FOREIGN KEY (id) REFERENCES uqa_rel_hidden.reference_rows(id))');
SELECT pg_temp.rel_command_probe('fk-unqualified-skip', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'CREATE TABLE uqa_rel_visible.fk_visible(id integer REFERENCES reference_rows(id))');
SELECT pg_temp.rel_command_probe('fk-column-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'CREATE TABLE uqa_rel_visible.fk_column_missing(id integer REFERENCES uqa_rel_absent.reference_rows(id))');
SELECT pg_temp.rel_command_probe('fk-table-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'CREATE TABLE uqa_rel_visible.fk_table_missing(id integer, FOREIGN KEY (id) REFERENCES uqa_rel_absent.reference_rows(id))');
SELECT pg_temp.rel_command_probe('fk-alter-qualified', 'uqa_rel_caller', 'pg_catalog', 'ALTER TABLE uqa_rel_visible.alter_rows ADD CONSTRAINT alter_fk FOREIGN KEY (id) REFERENCES uqa_rel_hidden.reference_rows(id)');
SELECT pg_temp.rel_command_probe('inherit-alter-qualified', 'uqa_rel_caller', 'pg_catalog', 'ALTER TABLE uqa_rel_visible.alter_rows INHERIT uqa_rel_hidden.parent_rows');
SELECT pg_temp.rel_command_probe('attach-alter-qualified', 'uqa_rel_caller', 'pg_catalog', 'ALTER TABLE uqa_rel_visible.partitioned_rows ATTACH PARTITION uqa_rel_hidden.attach_rows FOR VALUES FROM (10) TO (20)');
SELECT pg_temp.rel_command_probe('fk-alter-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'ALTER TABLE uqa_rel_visible.alter_rows ADD CONSTRAINT alter_fk_missing FOREIGN KEY (id) REFERENCES uqa_rel_absent.reference_rows(id)');
SELECT pg_temp.rel_command_probe('inherit-alter-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'ALTER TABLE uqa_rel_visible.alter_rows INHERIT uqa_rel_absent.parent_rows');
SELECT pg_temp.rel_command_probe('attach-alter-missing-schema', 'uqa_rel_caller', 'pg_catalog', 'ALTER TABLE uqa_rel_visible.partitioned_rows ATTACH PARTITION uqa_rel_absent.attach_rows FOR VALUES FROM (10) TO (20)');

GRANT USAGE ON SCHEMA uqa_rel_hidden TO uqa_rel_caller;
SELECT pg_temp.rel_command_probe('bound-rule-insert-create', 'uqa_rel_caller', 'pg_catalog', 'CREATE RULE bound_insert_action AS ON INSERT TO uqa_rel_visible.event_items DO ALSO INSERT INTO uqa_rel_hidden.event_log VALUES (NEW.id)');
SELECT pg_temp.rel_command_probe('bound-rule-update-create', 'uqa_rel_caller', 'pg_catalog', 'CREATE RULE bound_update_action AS ON INSERT TO uqa_rel_visible.event_items DO ALSO UPDATE uqa_rel_hidden.event_updates SET id = NEW.id');
SELECT pg_temp.rel_command_probe('bound-rule-delete-create', 'uqa_rel_caller', 'pg_catalog', 'CREATE RULE bound_delete_action AS ON INSERT TO uqa_rel_visible.event_items DO ALSO DELETE FROM uqa_rel_hidden.event_deletes WHERE id = NEW.id');
SELECT pg_temp.rel_command_probe('bound-fk-create', 'uqa_rel_caller', 'pg_catalog', 'CREATE TABLE uqa_rel_visible.bound_fk_rows(id integer REFERENCES uqa_rel_hidden.reference_rows(id))');
REVOKE USAGE ON SCHEMA uqa_rel_hidden FROM uqa_rel_caller;
SELECT pg_temp.rel_command_probe('bound-rule-execute', 'uqa_rel_caller', 'pg_catalog', 'INSERT INTO uqa_rel_visible.event_items VALUES (41)');
SELECT 'bound-rule-insert-result|' || count(*) FROM uqa_rel_hidden.event_log WHERE id = 41;
SELECT 'bound-rule-update-result|' || id FROM uqa_rel_hidden.event_updates;
SELECT 'bound-rule-delete-result|' || count(*) FROM uqa_rel_hidden.event_deletes;
SELECT pg_temp.rel_command_probe('bound-fk-execute', 'uqa_rel_caller', 'pg_catalog', 'INSERT INTO uqa_rel_visible.bound_fk_rows VALUES (2)');
SELECT 'bound-fk-result|' || count(*) FROM uqa_rel_visible.bound_fk_rows;

GRANT USAGE ON SCHEMA uqa_rel_hidden TO uqa_rel_caller;
SELECT pg_temp.rel_scalar_probe('first-visible', 'uqa_rel_caller', 'uqa_rel_hidden,uqa_rel_visible,pg_catalog', 'SELECT id FROM pick');
SELECT pg_temp.rel_command_probe('prepare-with-usage', 'uqa_rel_caller', 'pg_catalog', 'PREPARE uqa_rel_prepared AS SELECT id FROM uqa_rel_hidden.only_table');
REVOKE USAGE ON SCHEMA uqa_rel_hidden FROM uqa_rel_caller;
SELECT pg_temp.rel_scalar_probe('prepared-after-revoke', 'uqa_rel_caller', 'pg_catalog', 'EXECUTE uqa_rel_prepared');

GRANT USAGE ON SCHEMA uqa_rel_hidden TO uqa_rel_caller;
SET ROLE uqa_rel_caller;
BEGIN;
DECLARE uqa_rel_cursor CURSOR FOR SELECT id FROM uqa_rel_hidden.only_table;
RESET ROLE;
REVOKE USAGE ON SCHEMA uqa_rel_hidden FROM uqa_rel_caller;
SET ROLE uqa_rel_caller;
FETCH uqa_rel_cursor \gset uqa_rel_cursor_
SELECT 'bound-cursor|ok|' || :'uqa_rel_cursor_id';
ROLLBACK;
RESET ROLE;
REVOKE USAGE ON SCHEMA uqa_rel_hidden FROM uqa_rel_caller;

GRANT USAGE ON SCHEMA uqa_rel_hidden TO uqa_rel_group;
GRANT SELECT ON uqa_rel_hidden.only_table TO uqa_rel_member;
SELECT pg_temp.rel_scalar_probe('inherited-usage', 'uqa_rel_member', 'pg_catalog', 'SELECT id FROM uqa_rel_hidden.only_table');

DEALLOCATE uqa_rel_prepared;
DROP SCHEMA uqa_rel_visible CASCADE;
DROP SCHEMA uqa_rel_hidden CASCADE;
DROP OWNED BY uqa_rel_member;
DROP OWNED BY uqa_rel_group;
DROP OWNED BY uqa_rel_caller;
DROP ROLE uqa_rel_member;
DROP ROLE uqa_rel_group;
DROP ROLE uqa_rel_caller;
