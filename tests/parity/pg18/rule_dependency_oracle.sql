\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rule_dependency_oracle CASCADE;
DROP SCHEMA IF EXISTS uqa_rule_dependency_first CASCADE;
DROP SCHEMA IF EXISTS uqa_rule_dependency_second CASCADE;

CREATE SCHEMA uqa_rule_dependency_oracle;
CREATE SCHEMA uqa_rule_dependency_first;
CREATE SCHEMA uqa_rule_dependency_second;

CREATE OR REPLACE FUNCTION pg_temp.rule_dependency_probe(label text, command text)
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

CREATE TABLE uqa_rule_dependency_first.lookup(id integer);
CREATE TABLE uqa_rule_dependency_second.lookup(id integer);
CREATE TABLE uqa_rule_dependency_oracle.source_events(id integer);
CREATE TABLE uqa_rule_dependency_oracle.source_log(id integer);
INSERT INTO uqa_rule_dependency_first.lookup VALUES (1);
INSERT INTO uqa_rule_dependency_second.lookup VALUES (2);
SET search_path = uqa_rule_dependency_first, public;
CREATE RULE bound_source AS ON INSERT TO uqa_rule_dependency_oracle.source_events DO ALSO
  INSERT INTO uqa_rule_dependency_oracle.source_log
  SELECT id FROM lookup WHERE id = NEW.id;
SET search_path = uqa_rule_dependency_second, public;
INSERT INTO uqa_rule_dependency_oracle.source_events VALUES (1), (2);
SELECT 'source-bound|' || COALESCE(string_agg(id::text, ',' ORDER BY id), '') FROM uqa_rule_dependency_oracle.source_log;
RESET search_path;
SELECT pg_temp.rule_dependency_probe('source-restrict', 'DROP TABLE uqa_rule_dependency_first.lookup');
SELECT pg_temp.rule_dependency_probe('target-restrict', 'DROP TABLE uqa_rule_dependency_oracle.source_log');
ALTER TABLE uqa_rule_dependency_first.lookup RENAME TO renamed_lookup;
INSERT INTO uqa_rule_dependency_oracle.source_events VALUES (1), (2);
SELECT 'source-renamed|' || COALESCE(string_agg(id::text, ',' ORDER BY id), '') FROM uqa_rule_dependency_oracle.source_log;
DROP TABLE uqa_rule_dependency_first.renamed_lookup CASCADE;
SELECT 'source-cascade|' || count(*) FROM pg_rewrite WHERE rulename = 'bound_source';

CREATE FUNCTION uqa_rule_dependency_first.mapped(value bigint) RETURNS integer
LANGUAGE SQL IMMUTABLE AS 'SELECT 10';
CREATE FUNCTION uqa_rule_dependency_first.mapped(value text) RETURNS integer
LANGUAGE SQL IMMUTABLE AS 'SELECT 20';
CREATE TABLE uqa_rule_dependency_oracle.routine_events(id integer, note text);
CREATE TABLE uqa_rule_dependency_oracle.routine_log(first_value integer, second_value integer);
SET search_path = uqa_rule_dependency_first, public;
CREATE RULE bound_routines AS ON INSERT TO uqa_rule_dependency_oracle.routine_events DO ALSO
  INSERT INTO uqa_rule_dependency_oracle.routine_log
  VALUES (mapped(NEW.id), mapped(NEW.note));
CREATE FUNCTION uqa_rule_dependency_first.mapped(value integer) RETURNS integer
LANGUAGE SQL IMMUTABLE AS 'SELECT 99';
CREATE FUNCTION uqa_rule_dependency_second.mapped(value integer) RETURNS integer
LANGUAGE SQL IMMUTABLE AS 'SELECT 100';
CREATE FUNCTION uqa_rule_dependency_second.mapped(value text) RETURNS integer
LANGUAGE SQL IMMUTABLE AS 'SELECT 200';
SET search_path = uqa_rule_dependency_second, public;
INSERT INTO uqa_rule_dependency_oracle.routine_events VALUES (1, 'one');
SELECT 'routine-bound|' || first_value || '|' || second_value FROM uqa_rule_dependency_oracle.routine_log;
RESET search_path;
SELECT pg_temp.rule_dependency_probe('routine-unrelated', 'DROP FUNCTION uqa_rule_dependency_first.mapped(integer)');
SELECT pg_temp.rule_dependency_probe('routine-bigint-restrict', 'DROP FUNCTION uqa_rule_dependency_first.mapped(bigint)');
SELECT pg_temp.rule_dependency_probe('routine-text-restrict', 'DROP FUNCTION uqa_rule_dependency_first.mapped(text)');
DROP FUNCTION uqa_rule_dependency_first.mapped(bigint) CASCADE;
SELECT 'routine-cascade|' || count(*) FROM pg_rewrite WHERE rulename = 'bound_routines';

CREATE FUNCTION uqa_rule_dependency_first.accepted(value bigint) RETURNS boolean
LANGUAGE SQL IMMUTABLE AS 'SELECT true';
CREATE FUNCTION uqa_rule_dependency_second.accepted(value integer) RETURNS boolean
LANGUAGE SQL IMMUTABLE AS 'SELECT false';
CREATE TABLE uqa_rule_dependency_oracle.condition_events(id integer);
SET search_path = uqa_rule_dependency_first, public;
CREATE RULE bound_condition AS ON INSERT TO uqa_rule_dependency_oracle.condition_events
  WHERE accepted(NEW.id) DO INSTEAD NOTHING;
SET search_path = uqa_rule_dependency_second, public;
INSERT INTO uqa_rule_dependency_oracle.condition_events VALUES (1);
SELECT 'condition-bound|' || count(*) FROM uqa_rule_dependency_oracle.condition_events;
RESET search_path;
SELECT pg_temp.rule_dependency_probe('condition-restrict', 'DROP FUNCTION uqa_rule_dependency_first.accepted(bigint)');
DROP FUNCTION uqa_rule_dependency_first.accepted(bigint) CASCADE;
SELECT 'condition-cascade|' || count(*) FROM pg_rewrite WHERE rulename = 'bound_condition';

CREATE FUNCTION uqa_rule_dependency_first.chosen(value integer)
RETURNS TABLE(result integer)
LANGUAGE SQL IMMUTABLE AS 'SELECT value + 10';
CREATE FUNCTION uqa_rule_dependency_second.chosen(value integer)
RETURNS TABLE(result integer)
LANGUAGE SQL IMMUTABLE AS 'SELECT value + 100';
CREATE TABLE uqa_rule_dependency_oracle.function_events(id integer);
CREATE TABLE uqa_rule_dependency_oracle.function_log(value integer);
SET search_path = uqa_rule_dependency_first, public;
CREATE RULE bound_table_function AS ON INSERT TO uqa_rule_dependency_oracle.function_events DO ALSO
  INSERT INTO uqa_rule_dependency_oracle.function_log
  SELECT result FROM chosen(NEW.id);
SET search_path = uqa_rule_dependency_second, public;
INSERT INTO uqa_rule_dependency_oracle.function_events VALUES (1);
SELECT 'table-function-bound|' || value FROM uqa_rule_dependency_oracle.function_log;
RESET search_path;
SELECT pg_temp.rule_dependency_probe('table-function-restrict', 'DROP FUNCTION uqa_rule_dependency_first.chosen(integer)');
DROP FUNCTION uqa_rule_dependency_first.chosen(integer) CASCADE;
SELECT 'table-function-cascade|' || count(*) FROM pg_rewrite WHERE rulename = 'bound_table_function';

DROP SCHEMA uqa_rule_dependency_oracle CASCADE;
DROP SCHEMA uqa_rule_dependency_first CASCADE;
DROP SCHEMA uqa_rule_dependency_second CASCADE;
