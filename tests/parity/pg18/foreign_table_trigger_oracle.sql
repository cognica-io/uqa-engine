\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_foreign_trigger_probe CASCADE;
DROP SERVER IF EXISTS uqa_foreign_trigger_server CASCADE;
DROP ROLE IF EXISTS uqa_foreign_trigger_owner;
DROP ROLE IF EXISTS uqa_foreign_trigger_creator;
DROP ROLE IF EXISTS uqa_foreign_trigger_function_owner;
DROP ROLE IF EXISTS uqa_foreign_trigger_outsider;
DROP ROLE IF EXISTS uqa_foreign_trigger_next_owner;

CREATE EXTENSION IF NOT EXISTS postgres_fdw;
CREATE ROLE uqa_foreign_trigger_owner;
CREATE ROLE uqa_foreign_trigger_creator;
CREATE ROLE uqa_foreign_trigger_function_owner;
CREATE ROLE uqa_foreign_trigger_outsider;
CREATE ROLE uqa_foreign_trigger_next_owner;
GRANT uqa_foreign_trigger_next_owner TO uqa_foreign_trigger_owner WITH INHERIT FALSE, SET TRUE;
CREATE SCHEMA uqa_foreign_trigger_probe AUTHORIZATION uqa_foreign_trigger_owner;
CREATE SERVER uqa_foreign_trigger_server FOREIGN DATA WRAPPER postgres_fdw OPTIONS (dbname 'postgres');
GRANT USAGE ON FOREIGN SERVER uqa_foreign_trigger_server TO uqa_foreign_trigger_owner;
GRANT USAGE, CREATE ON SCHEMA uqa_foreign_trigger_probe TO uqa_foreign_trigger_function_owner;
GRANT USAGE ON SCHEMA uqa_foreign_trigger_probe TO uqa_foreign_trigger_creator, uqa_foreign_trigger_outsider;
GRANT USAGE, CREATE ON SCHEMA uqa_foreign_trigger_probe TO uqa_foreign_trigger_next_owner;

CREATE OR REPLACE FUNCTION pg_temp.foreign_trigger_probe(label text, role_name text, command text)
RETURNS text
LANGUAGE plpgsql
AS $probe$
DECLARE
    state text;
    message text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    EXECUTE command;
    RESET ROLE;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RESET ROLE;
    RETURN label || '|' || state || '|' || message;
END
$probe$;

SET ROLE uqa_foreign_trigger_function_owner;
CREATE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    RETURN COALESCE(NEW, OLD);
END
$function$;
CREATE FUNCTION uqa_foreign_trigger_probe.denied_trigger()
RETURNS trigger
LANGUAGE plpgsql
AS $function$
BEGIN
    RETURN COALESCE(NEW, OLD);
END
$function$;
REVOKE ALL ON FUNCTION uqa_foreign_trigger_probe.allowed_trigger() FROM PUBLIC;
REVOKE ALL ON FUNCTION uqa_foreign_trigger_probe.denied_trigger() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION uqa_foreign_trigger_probe.allowed_trigger() TO uqa_foreign_trigger_owner, uqa_foreign_trigger_creator;
RESET ROLE;

SET ROLE uqa_foreign_trigger_owner;
CREATE FOREIGN TABLE uqa_foreign_trigger_probe.items(id integer, value text)
SERVER uqa_foreign_trigger_server
OPTIONS (schema_name 'public', table_name 'items');
CREATE FOREIGN TABLE uqa_foreign_trigger_probe.transfer_items(id integer)
SERVER uqa_foreign_trigger_server
OPTIONS (schema_name 'public', table_name 'transfer_items');
CREATE FOREIGN TABLE uqa_foreign_trigger_probe.rollback_items(id integer)
SERVER uqa_foreign_trigger_server
OPTIONS (schema_name 'public', table_name 'rollback_items');
RESET ROLE;
GRANT TRIGGER ON TABLE uqa_foreign_trigger_probe.items TO uqa_foreign_trigger_creator;

SELECT pg_temp.foreign_trigger_probe('owner-row-before', 'uqa_foreign_trigger_owner', $cmd$CREATE TRIGGER row_before BEFORE INSERT OR UPDATE OF value ON uqa_foreign_trigger_probe.items FOR EACH ROW WHEN (NEW.id > 0) EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('owner-delete-before', 'uqa_foreign_trigger_owner', $cmd$CREATE TRIGGER delete_before BEFORE DELETE ON uqa_foreign_trigger_probe.items FOR EACH ROW WHEN (OLD.id > 0) EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('owner-statement-after', 'uqa_foreign_trigger_owner', $cmd$CREATE TRIGGER statement_after AFTER INSERT OR UPDATE OR DELETE ON uqa_foreign_trigger_probe.items FOR EACH STATEMENT EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('owner-truncate', 'uqa_foreign_trigger_owner', $cmd$CREATE TRIGGER truncate_after AFTER TRUNCATE ON uqa_foreign_trigger_probe.items FOR EACH STATEMENT EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('creator-granted', 'uqa_foreign_trigger_creator', $cmd$CREATE TRIGGER creator_before BEFORE INSERT ON uqa_foreign_trigger_probe.items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('creator-replace', 'uqa_foreign_trigger_creator', $cmd$CREATE OR REPLACE TRIGGER creator_before BEFORE INSERT ON uqa_foreign_trigger_probe.items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('outsider-denied', 'uqa_foreign_trigger_outsider', $cmd$CREATE TRIGGER outsider_before BEFORE INSERT ON uqa_foreign_trigger_probe.items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('function-denied', 'uqa_foreign_trigger_creator', $cmd$CREATE TRIGGER denied_before BEFORE INSERT ON uqa_foreign_trigger_probe.items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.denied_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('constraint-denied', 'uqa_foreign_trigger_owner', $cmd$CREATE CONSTRAINT TRIGGER constraint_after AFTER INSERT ON uqa_foreign_trigger_probe.items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('transition-denied', 'uqa_foreign_trigger_owner', $cmd$CREATE TRIGGER transition_after AFTER INSERT ON uqa_foreign_trigger_probe.items REFERENCING NEW TABLE AS inserted FOR EACH STATEMENT EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('instead-denied', 'uqa_foreign_trigger_owner', $cmd$CREATE TRIGGER instead_row INSTEAD OF INSERT ON uqa_foreign_trigger_probe.items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('outsider-constraint-kind-first', 'uqa_foreign_trigger_outsider', $cmd$CREATE CONSTRAINT TRIGGER outsider_constraint AFTER INSERT ON uqa_foreign_trigger_probe.items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('outsider-transition-kind-first', 'uqa_foreign_trigger_outsider', $cmd$CREATE TRIGGER outsider_transition AFTER INSERT ON uqa_foreign_trigger_probe.items REFERENCING NEW TABLE AS inserted FOR EACH STATEMENT EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
SELECT pg_temp.foreign_trigger_probe('outsider-instead-kind-first', 'uqa_foreign_trigger_outsider', $cmd$CREATE TRIGGER outsider_instead INSTEAD OF INSERT ON uqa_foreign_trigger_probe.items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);

SELECT 'class|' || relkind::text || '|' || relhastriggers FROM pg_class WHERE oid = 'uqa_foreign_trigger_probe.items'::regclass;
SELECT 'trigger|' || tgname || '|' || tgtype || '|' || tgenabled::text || '|' || pg_get_triggerdef(oid, false) FROM pg_trigger WHERE tgrelid = 'uqa_foreign_trigger_probe.items'::regclass ORDER BY tgname;

SELECT pg_temp.foreign_trigger_probe('creator-disable-denied', 'uqa_foreign_trigger_creator', $cmd$ALTER FOREIGN TABLE uqa_foreign_trigger_probe.items DISABLE TRIGGER creator_before$cmd$);
SELECT pg_temp.foreign_trigger_probe('owner-disable', 'uqa_foreign_trigger_owner', $cmd$ALTER FOREIGN TABLE uqa_foreign_trigger_probe.items DISABLE TRIGGER creator_before$cmd$);
SELECT 'disabled|' || tgenabled::text FROM pg_trigger WHERE tgrelid = 'uqa_foreign_trigger_probe.items'::regclass AND tgname = 'creator_before';
SELECT pg_temp.foreign_trigger_probe('owner-enable-table-spelling', 'uqa_foreign_trigger_owner', $cmd$ALTER TABLE uqa_foreign_trigger_probe.items ENABLE TRIGGER creator_before$cmd$);
SELECT 'enabled|' || tgenabled::text FROM pg_trigger WHERE tgrelid = 'uqa_foreign_trigger_probe.items'::regclass AND tgname = 'creator_before';
SELECT pg_temp.foreign_trigger_probe('creator-rename-denied', 'uqa_foreign_trigger_creator', $cmd$ALTER TRIGGER creator_before ON uqa_foreign_trigger_probe.items RENAME TO creator_renamed$cmd$);
SELECT pg_temp.foreign_trigger_probe('owner-rename', 'uqa_foreign_trigger_owner', $cmd$ALTER TRIGGER creator_before ON uqa_foreign_trigger_probe.items RENAME TO creator_renamed$cmd$);
SELECT pg_temp.foreign_trigger_probe('creator-drop-denied', 'uqa_foreign_trigger_creator', $cmd$DROP TRIGGER creator_renamed ON uqa_foreign_trigger_probe.items$cmd$);
SELECT pg_temp.foreign_trigger_probe('owner-drop', 'uqa_foreign_trigger_owner', $cmd$DROP TRIGGER creator_renamed ON uqa_foreign_trigger_probe.items$cmd$);

SET ROLE uqa_foreign_trigger_owner;
CREATE TRIGGER transfer_trigger BEFORE INSERT ON uqa_foreign_trigger_probe.transfer_items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger();
CREATE TRIGGER rollback_trigger BEFORE INSERT ON uqa_foreign_trigger_probe.rollback_items FOR EACH ROW EXECUTE FUNCTION uqa_foreign_trigger_probe.allowed_trigger();
ALTER FOREIGN TABLE uqa_foreign_trigger_probe.transfer_items OWNER TO uqa_foreign_trigger_next_owner;
RESET ROLE;
SELECT pg_temp.foreign_trigger_probe('former-owner-drop-denied', 'uqa_foreign_trigger_owner', $cmd$DROP TRIGGER transfer_trigger ON uqa_foreign_trigger_probe.transfer_items$cmd$);
SELECT pg_temp.foreign_trigger_probe('new-owner-drop', 'uqa_foreign_trigger_next_owner', $cmd$DROP TRIGGER transfer_trigger ON uqa_foreign_trigger_probe.transfer_items$cmd$);
BEGIN;
ALTER FOREIGN TABLE uqa_foreign_trigger_probe.rollback_items OWNER TO uqa_foreign_trigger_next_owner;
ROLLBACK;
SELECT pg_temp.foreign_trigger_probe('rollback-owner-drop', 'uqa_foreign_trigger_owner', $cmd$DROP TRIGGER rollback_trigger ON uqa_foreign_trigger_probe.rollback_items$cmd$);

SELECT pg_temp.foreign_trigger_probe('drop-function-restrict', 'uqa_foreign_trigger_function_owner', $cmd$DROP FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);
DROP FOREIGN TABLE uqa_foreign_trigger_probe.items;
SELECT 'after-drop|' || count(*) FROM pg_trigger WHERE tgrelid = to_regclass('uqa_foreign_trigger_probe.items');
SELECT pg_temp.foreign_trigger_probe('drop-function-after-table', 'uqa_foreign_trigger_function_owner', $cmd$DROP FUNCTION uqa_foreign_trigger_probe.allowed_trigger()$cmd$);

DROP SCHEMA uqa_foreign_trigger_probe CASCADE;
DROP SERVER uqa_foreign_trigger_server CASCADE;
DROP ROLE uqa_foreign_trigger_owner;
DROP ROLE uqa_foreign_trigger_creator;
DROP ROLE uqa_foreign_trigger_function_owner;
DROP ROLE uqa_foreign_trigger_outsider;
DROP ROLE uqa_foreign_trigger_next_owner;
