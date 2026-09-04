\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_routine_rename_oracle CASCADE;
CREATE SCHEMA uqa_routine_rename_oracle;

CREATE OR REPLACE FUNCTION pg_temp.routine_rename_probe(label text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
BEGIN
    EXECUTE command;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE;
    RETURN label || '|' || state;
END
$oracle$;

CREATE FUNCTION uqa_routine_rename_oracle.oid_target(value integer) RETURNS integer LANGUAGE SQL RETURN value;
CREATE TEMP TABLE uqa_routine_rename_oids(label text PRIMARY KEY, object_id oid);
INSERT INTO uqa_routine_rename_oids
SELECT 'oid-target', oid FROM pg_proc WHERE oid = 'uqa_routine_rename_oracle.oid_target(integer)'::regprocedure;
ALTER FUNCTION uqa_routine_rename_oracle.oid_target(integer) RENAME TO oid_renamed;
SELECT 'oid-stable|' || (saved.object_id = live.oid)
FROM uqa_routine_rename_oids saved
JOIN pg_proc live ON live.oid = 'uqa_routine_rename_oracle.oid_renamed(integer)'::regprocedure
WHERE saved.label = 'oid-target';
SELECT 'specific-name|' || (r.specific_name = 'oid_renamed_' || p.oid::text)
FROM pg_proc p
JOIN information_schema.routines r
  ON r.specific_schema = 'uqa_routine_rename_oracle'
 AND r.routine_name = p.proname
WHERE p.oid = 'uqa_routine_rename_oracle.oid_renamed(integer)'::regprocedure;
CREATE OR REPLACE FUNCTION uqa_routine_rename_oracle.oid_renamed(value integer) RETURNS integer LANGUAGE SQL RETURN value + 10;
SELECT 'replace-oid|' || (saved.object_id = live.oid) || '|' || uqa_routine_rename_oracle.oid_renamed(2)
FROM uqa_routine_rename_oids saved
JOIN pg_proc live ON live.oid = 'uqa_routine_rename_oracle.oid_renamed(integer)'::regprocedure
WHERE saved.label = 'oid-target';

CREATE FUNCTION uqa_routine_rename_oracle.pick(value integer) RETURNS integer LANGUAGE SQL RETURN value + 1;
CREATE FUNCTION uqa_routine_rename_oracle.pick(value text) RETURNS text LANGUAGE SQL RETURN value || '!';
ALTER FUNCTION uqa_routine_rename_oracle.pick(integer) RENAME TO chosen;
SELECT 'overload|' || uqa_routine_rename_oracle.chosen(4) || '|' || uqa_routine_rename_oracle.pick('x');
SELECT pg_temp.routine_rename_probe('old-overload', 'SELECT uqa_routine_rename_oracle.pick(4)');

CREATE FUNCTION uqa_routine_rename_oracle.ambiguous(value integer) RETURNS integer LANGUAGE SQL RETURN value;
CREATE FUNCTION uqa_routine_rename_oracle.ambiguous(value text) RETURNS text LANGUAGE SQL RETURN value;
CREATE FUNCTION uqa_routine_rename_oracle.unique_name(value integer) RETURNS integer LANGUAGE SQL RETURN value;
CREATE FUNCTION uqa_routine_rename_oracle.collision(value integer) RETURNS integer LANGUAGE SQL RETURN value;
SELECT pg_temp.routine_rename_probe('ambiguous', 'ALTER FUNCTION uqa_routine_rename_oracle.ambiguous RENAME TO impossible');
SELECT pg_temp.routine_rename_probe('collision', 'ALTER FUNCTION uqa_routine_rename_oracle.chosen(integer) RENAME TO collision');
SELECT pg_temp.routine_rename_probe('missing', 'ALTER FUNCTION uqa_routine_rename_oracle.missing(integer) RENAME TO absent');
ALTER FUNCTION uqa_routine_rename_oracle.unique_name RENAME TO unique_renamed;
SELECT 'omitted-unique|' || uqa_routine_rename_oracle.unique_renamed(3);

CREATE PROCEDURE uqa_routine_rename_oracle.proc(value integer) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;
SELECT pg_temp.routine_rename_probe('wrong-kind', 'ALTER PROCEDURE uqa_routine_rename_oracle.chosen(integer) RENAME TO wrong_kind');
ALTER ROUTINE uqa_routine_rename_oracle.proc(integer) RENAME TO moved_proc;
SELECT pg_temp.routine_rename_probe('routine-procedure', 'CALL uqa_routine_rename_oracle.moved_proc(1)');
ALTER ROUTINE uqa_routine_rename_oracle.chosen(integer) RENAME TO moved_function;
SELECT 'routine-function|' || uqa_routine_rename_oracle.moved_function(5);
BEGIN;
ALTER FUNCTION uqa_routine_rename_oracle.moved_function(integer) RENAME TO rolled_back;
ROLLBACK;
SELECT 'rollback|' || uqa_routine_rename_oracle.moved_function(6);
SELECT pg_temp.routine_rename_probe('rollback-missing', 'SELECT uqa_routine_rename_oracle.rolled_back(6)');

CREATE ROLE uqa_routine_rename_owner;
CREATE SCHEMA uqa_routine_rename_acl;
GRANT USAGE, CREATE ON SCHEMA uqa_routine_rename_acl TO uqa_routine_rename_owner;
SET ROLE uqa_routine_rename_owner;
CREATE FUNCTION uqa_routine_rename_acl.source(value integer) RETURNS integer LANGUAGE SQL RETURN value;
CREATE FUNCTION uqa_routine_rename_acl.collision(value integer) RETURNS integer LANGUAGE SQL RETURN value;
RESET ROLE;
REVOKE CREATE ON SCHEMA uqa_routine_rename_acl FROM uqa_routine_rename_owner;
SET ROLE uqa_routine_rename_owner;
SELECT pg_temp.routine_rename_probe('schema-create-before-collision', 'ALTER FUNCTION uqa_routine_rename_acl.source(integer) RENAME TO collision');
RESET ROLE;
DROP SCHEMA uqa_routine_rename_acl CASCADE;
DROP ROLE uqa_routine_rename_owner;

CREATE FUNCTION uqa_routine_rename_oracle.base(value integer) RETURNS integer LANGUAGE SQL IMMUTABLE RETURN value + 1;
CREATE FUNCTION uqa_routine_rename_oracle.standard_caller(value integer) RETURNS integer LANGUAGE SQL RETURN uqa_routine_rename_oracle.base(value);
CREATE TABLE uqa_routine_rename_oracle.command_sink(value integer);
CREATE TABLE uqa_routine_rename_oracle.command_merge_target(id integer PRIMARY KEY, value integer);
CREATE TABLE uqa_routine_rename_oracle.command_merge_source(id integer, value integer);
INSERT INTO uqa_routine_rename_oracle.command_merge_source VALUES (1, 4);
CREATE PROCEDURE uqa_routine_rename_oracle.command_caller(value integer) LANGUAGE SQL BEGIN ATOMIC INSERT INTO uqa_routine_rename_oracle.command_sink VALUES (uqa_routine_rename_oracle.base(value)); UPDATE uqa_routine_rename_oracle.command_sink SET value = uqa_routine_rename_oracle.base(command_sink.value) WHERE false; DELETE FROM uqa_routine_rename_oracle.command_sink WHERE uqa_routine_rename_oracle.base(command_sink.value) < 0; MERGE INTO uqa_routine_rename_oracle.command_merge_target AS target USING uqa_routine_rename_oracle.command_merge_source AS source ON target.id = source.id WHEN MATCHED THEN UPDATE SET value = uqa_routine_rename_oracle.base(source.value) WHEN NOT MATCHED THEN INSERT (id, value) VALUES (source.id, uqa_routine_rename_oracle.base(source.value)); END;
CREATE FUNCTION uqa_routine_rename_oracle.default_caller(value integer DEFAULT uqa_routine_rename_oracle.base(1)) RETURNS integer LANGUAGE SQL AS 'SELECT $1';
CREATE FUNCTION uqa_routine_rename_oracle.sql_text_caller(value integer) RETURNS integer LANGUAGE SQL AS 'SELECT uqa_routine_rename_oracle.base($1)';
CREATE FUNCTION uqa_routine_rename_oracle.plpgsql_text_caller(value integer) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN RETURN uqa_routine_rename_oracle.base(value); END $$;
CREATE VIEW uqa_routine_rename_oracle.bound_view AS SELECT uqa_routine_rename_oracle.base(6) AS value;
CREATE TABLE uqa_routine_rename_oracle.generated_source(id integer, derived integer GENERATED ALWAYS AS (uqa_routine_rename_oracle.base(id)) STORED);
CREATE TABLE uqa_routine_rename_oracle.schema_source(id integer DEFAULT uqa_routine_rename_oracle.base(1), value integer CONSTRAINT schema_column_check CHECK (uqa_routine_rename_oracle.base(value) < 100), other integer, CONSTRAINT schema_table_check CHECK (uqa_routine_rename_oracle.base(other) < 100));
CREATE TABLE uqa_routine_rename_oracle.rule_source(id integer);
CREATE TABLE uqa_routine_rename_oracle.rule_log(value integer);
CREATE RULE copy_value AS ON INSERT TO uqa_routine_rename_oracle.rule_source DO ALSO INSERT INTO uqa_routine_rename_oracle.rule_log VALUES (uqa_routine_rename_oracle.base(NEW.id));
CREATE FUNCTION uqa_routine_rename_oracle.table_base(value integer) RETURNS TABLE(output integer) LANGUAGE SQL AS 'SELECT $1 + 2';
CREATE VIEW uqa_routine_rename_oracle.bound_table_view AS SELECT output FROM uqa_routine_rename_oracle.table_base(5);
CREATE TABLE uqa_routine_rename_oracle.trigger_source(id integer);
CREATE FUNCTION uqa_routine_rename_oracle.increment_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.id := NEW.id + 1; RETURN NEW; END $$;
CREATE TRIGGER increment_before BEFORE INSERT ON uqa_routine_rename_oracle.trigger_source FOR EACH ROW EXECUTE FUNCTION uqa_routine_rename_oracle.increment_trigger();
INSERT INTO uqa_routine_rename_oids
SELECT 'base', 'uqa_routine_rename_oracle.base(integer)'::regprocedure
UNION ALL SELECT 'table-base', 'uqa_routine_rename_oracle.table_base(integer)'::regprocedure
UNION ALL SELECT 'trigger', 'uqa_routine_rename_oracle.increment_trigger()'::regprocedure;
ALTER FUNCTION uqa_routine_rename_oracle.base(integer) RENAME TO renamed_base;
ALTER FUNCTION uqa_routine_rename_oracle.table_base(integer) RENAME TO renamed_table_base;
ALTER FUNCTION uqa_routine_rename_oracle.increment_trigger() RENAME TO renamed_trigger;
SELECT 'dependent-oids|' || bool_and(saved.object_id = live.oid)
FROM uqa_routine_rename_oids saved
JOIN pg_proc live ON live.oid = saved.object_id
WHERE saved.label IN ('base', 'table-base', 'trigger');
SELECT 'standard-body-deparse|' || (position('renamed_base' in pg_get_functiondef('uqa_routine_rename_oracle.standard_caller(integer)'::regprocedure)) > 0);
SELECT 'command-body-deparse|' || (position('renamed_base' in pg_get_functiondef('uqa_routine_rename_oracle.command_caller(integer)'::regprocedure)) > 0);
SELECT 'parameter-default-deparse|' || (position('renamed_base' in pg_get_function_arguments('uqa_routine_rename_oracle.default_caller(integer)'::regprocedure)) > 0);
SELECT 'routine-dependency-count|' || count(*)
FROM pg_depend dependency
JOIN pg_proc dependent ON dependent.oid = dependency.objid
WHERE dependency.classid = 'pg_proc'::regclass
  AND dependency.refclassid = 'pg_proc'::regclass
  AND dependency.refobjid = 'uqa_routine_rename_oracle.renamed_base(integer)'::regprocedure
  AND dependent.proname IN ('standard_caller', 'command_caller', 'default_caller');
SELECT 'view-deparse|' || (position('renamed_base' in view_definition) > 0)
FROM information_schema.views WHERE table_schema = 'uqa_routine_rename_oracle' AND table_name = 'bound_view';
SELECT 'table-view-deparse|' || (position('renamed_table_base' in view_definition) > 0)
FROM information_schema.views WHERE table_schema = 'uqa_routine_rename_oracle' AND table_name = 'bound_table_view';
SELECT 'generated-deparse|' || (position('renamed_base' in pg_get_expr(d.adbin, d.adrelid, true)) > 0)
FROM pg_attrdef d
JOIN pg_attribute a ON a.attrelid = d.adrelid AND a.attnum = d.adnum
WHERE d.adrelid = 'uqa_routine_rename_oracle.generated_source'::regclass AND a.attname = 'derived';
SELECT 'column-default-deparse|' || (position('renamed_base' in pg_get_expr(d.adbin, d.adrelid, true)) > 0)
FROM pg_attrdef d
JOIN pg_attribute a ON a.attrelid = d.adrelid AND a.attnum = d.adnum
WHERE d.adrelid = 'uqa_routine_rename_oracle.schema_source'::regclass AND a.attname = 'id';
SELECT 'column-check-deparse|' || (position('renamed_base' in pg_get_constraintdef(oid, true)) > 0)
FROM pg_constraint WHERE conrelid = 'uqa_routine_rename_oracle.schema_source'::regclass AND conname = 'schema_column_check';
SELECT 'table-check-deparse|' || (position('renamed_base' in pg_get_constraintdef(oid, true)) > 0)
FROM pg_constraint WHERE conrelid = 'uqa_routine_rename_oracle.schema_source'::regclass AND conname = 'schema_table_check';
SELECT 'rule-deparse|' || (position('renamed_base' in pg_get_ruledef(oid, true)) > 0)
FROM pg_rewrite WHERE ev_class = 'uqa_routine_rename_oracle.rule_source'::regclass AND rulename = 'copy_value';
SELECT 'trigger-deparse|' || (position('renamed_trigger' in pg_get_triggerdef(oid, true)) > 0)
FROM pg_trigger WHERE tgrelid = 'uqa_routine_rename_oracle.trigger_source'::regclass AND tgname = 'increment_before';
SELECT 'standard-call|' || uqa_routine_rename_oracle.standard_caller(4);
CALL uqa_routine_rename_oracle.command_caller(4);
SELECT 'command-call|' || value FROM uqa_routine_rename_oracle.command_sink ORDER BY value DESC LIMIT 1;
SELECT 'merge-command-call|' || value FROM uqa_routine_rename_oracle.command_merge_target WHERE id = 1;
SELECT 'parameter-default-call|' || uqa_routine_rename_oracle.default_caller();
SELECT 'view-call|' || value FROM uqa_routine_rename_oracle.bound_view;
SELECT 'table-view-call|' || output FROM uqa_routine_rename_oracle.bound_table_view;
INSERT INTO uqa_routine_rename_oracle.generated_source VALUES (8);
SELECT 'generated-call|' || derived FROM uqa_routine_rename_oracle.generated_source;
INSERT INTO uqa_routine_rename_oracle.schema_source(value, other) VALUES (1, 2);
SELECT 'schema-expression-call|' || id FROM uqa_routine_rename_oracle.schema_source WHERE value = 1;
INSERT INTO uqa_routine_rename_oracle.rule_source VALUES (10);
SELECT 'rule-call|' || value FROM uqa_routine_rename_oracle.rule_log;
INSERT INTO uqa_routine_rename_oracle.trigger_source VALUES (12);
SELECT 'trigger-call|' || id FROM uqa_routine_rename_oracle.trigger_source;
SELECT pg_temp.routine_rename_probe('sql-text-missing', 'SELECT uqa_routine_rename_oracle.sql_text_caller(4)');
SELECT pg_temp.routine_rename_probe('plpgsql-text-missing', 'SELECT uqa_routine_rename_oracle.plpgsql_text_caller(4)');
SELECT pg_temp.routine_rename_probe('dependent-restrict', 'DROP FUNCTION uqa_routine_rename_oracle.renamed_base(integer) RESTRICT');
CREATE FUNCTION uqa_routine_rename_oracle.base(value integer) RETURNS integer LANGUAGE SQL IMMUTABLE RETURN value + 100;
SELECT 'identity-isolation|' || uqa_routine_rename_oracle.standard_caller(4) || '|' || uqa_routine_rename_oracle.sql_text_caller(4) || '|' || uqa_routine_rename_oracle.plpgsql_text_caller(4);
INSERT INTO uqa_routine_rename_oracle.schema_source(value, other) VALUES (3, 4);
SELECT 'schema-expression-after-recreate|' || id FROM uqa_routine_rename_oracle.schema_source WHERE value = 3;
CALL uqa_routine_rename_oracle.command_caller(5);
SELECT 'command-after-recreate|' || value FROM uqa_routine_rename_oracle.command_sink ORDER BY value DESC LIMIT 1;
SELECT 'merge-command-after-recreate|' || value FROM uqa_routine_rename_oracle.command_merge_target WHERE id = 1;
SELECT 'parameter-default-after-recreate|' || uqa_routine_rename_oracle.default_caller();
DROP FUNCTION uqa_routine_rename_oracle.base(integer);
SELECT 'bound-after-old-drop|' || uqa_routine_rename_oracle.standard_caller(4);
DROP FUNCTION uqa_routine_rename_oracle.renamed_base(integer) CASCADE;
SELECT 'routine-cascade|' || (to_regprocedure('uqa_routine_rename_oracle.command_caller(integer)') IS NULL) || '|' || (to_regprocedure('uqa_routine_rename_oracle.default_caller(integer)') IS NULL);
SELECT 'schema-expression-cascade|' || (column_default IS NULL) || '|' || (NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'uqa_routine_rename_oracle.schema_source'::regclass AND conname IN ('schema_column_check', 'schema_table_check'))) || '|' || (to_regclass('uqa_routine_rename_oracle.schema_source') IS NOT NULL) FROM information_schema.columns WHERE table_schema = 'uqa_routine_rename_oracle' AND table_name = 'schema_source' AND column_name = 'id';

DROP SCHEMA uqa_routine_rename_oracle CASCADE;
