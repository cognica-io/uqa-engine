\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rule_column_dependency_oracle CASCADE;
CREATE SCHEMA uqa_rule_column_dependency_oracle;

CREATE OR REPLACE FUNCTION pg_temp.rule_column_dependency_probe(label text, command text)
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

CREATE TABLE uqa_rule_column_dependency_oracle.events(id integer);
CREATE TABLE uqa_rule_column_dependency_oracle.source(source_value integer, predicate_value integer, disposable_value integer);
CREATE TABLE uqa_rule_column_dependency_oracle.log(target_value integer);
INSERT INTO uqa_rule_column_dependency_oracle.source VALUES (10, 1, 100);
CREATE RULE bound_columns AS ON INSERT TO uqa_rule_column_dependency_oracle.events
  WHERE EXISTS (
    SELECT 1 FROM uqa_rule_column_dependency_oracle.source AS source
    WHERE source.predicate_value = NEW.id
  )
  DO ALSO INSERT INTO uqa_rule_column_dependency_oracle.log(target_value)
    SELECT source.source_value FROM uqa_rule_column_dependency_oracle.source AS source
    WHERE source.predicate_value = NEW.id;
ALTER TABLE uqa_rule_column_dependency_oracle.source RENAME COLUMN source_value TO renamed_source_value;
ALTER TABLE uqa_rule_column_dependency_oracle.source RENAME COLUMN predicate_value TO renamed_predicate_value;
ALTER TABLE uqa_rule_column_dependency_oracle.log RENAME COLUMN target_value TO renamed_target_value;
INSERT INTO uqa_rule_column_dependency_oracle.events VALUES (1);
SELECT 'renamed-execution|' || renamed_target_value FROM uqa_rule_column_dependency_oracle.log;
SELECT pg_temp.rule_column_dependency_probe('source-projection-restrict', 'ALTER TABLE uqa_rule_column_dependency_oracle.source DROP COLUMN renamed_source_value');
SELECT pg_temp.rule_column_dependency_probe('source-predicate-restrict', 'ALTER TABLE uqa_rule_column_dependency_oracle.source DROP COLUMN renamed_predicate_value');
SELECT pg_temp.rule_column_dependency_probe('target-restrict', 'ALTER TABLE uqa_rule_column_dependency_oracle.log DROP COLUMN renamed_target_value');
SELECT pg_temp.rule_column_dependency_probe('unreferenced-drop', 'ALTER TABLE uqa_rule_column_dependency_oracle.source DROP COLUMN disposable_value');

CREATE TABLE uqa_rule_column_dependency_oracle.positional_events(id integer);
CREATE TABLE uqa_rule_column_dependency_oracle.positional_log(first_value integer, second_value integer);
CREATE RULE positional_columns AS ON INSERT TO uqa_rule_column_dependency_oracle.positional_events DO ALSO
  INSERT INTO uqa_rule_column_dependency_oracle.positional_log VALUES (NEW.id, 7);
ALTER TABLE uqa_rule_column_dependency_oracle.positional_log RENAME COLUMN first_value TO renamed_first_value;
ALTER TABLE uqa_rule_column_dependency_oracle.positional_log RENAME COLUMN second_value TO renamed_second_value;
ALTER TABLE uqa_rule_column_dependency_oracle.positional_log ADD COLUMN later_value integer DEFAULT 99;
INSERT INTO uqa_rule_column_dependency_oracle.positional_events VALUES (5);
SELECT 'positional|' || renamed_first_value || '|' || renamed_second_value || '|' || later_value FROM uqa_rule_column_dependency_oracle.positional_log;
SELECT pg_temp.rule_column_dependency_probe('positional-restrict', 'ALTER TABLE uqa_rule_column_dependency_oracle.positional_log DROP COLUMN renamed_second_value');

CREATE TABLE uqa_rule_column_dependency_oracle.binding_events(id integer);
CREATE TABLE uqa_rule_column_dependency_oracle.binding_left(id integer, selected_value integer);
CREATE TABLE uqa_rule_column_dependency_oracle.binding_right(id integer);
CREATE TABLE uqa_rule_column_dependency_oracle.binding_log(value integer);
INSERT INTO uqa_rule_column_dependency_oracle.binding_left VALUES (1, 11);
INSERT INTO uqa_rule_column_dependency_oracle.binding_right VALUES (1);
CREATE RULE stable_unqualified_column AS ON INSERT TO uqa_rule_column_dependency_oracle.binding_events DO ALSO
  INSERT INTO uqa_rule_column_dependency_oracle.binding_log
    SELECT selected_value
    FROM uqa_rule_column_dependency_oracle.binding_left AS left_source
    JOIN uqa_rule_column_dependency_oracle.binding_right AS right_source ON left_source.id = right_source.id;
ALTER TABLE uqa_rule_column_dependency_oracle.binding_right ADD COLUMN selected_value integer;
UPDATE uqa_rule_column_dependency_oracle.binding_right SET selected_value = 99;
INSERT INTO uqa_rule_column_dependency_oracle.binding_events VALUES (1);
SELECT 'stable-owner|' || value FROM uqa_rule_column_dependency_oracle.binding_log;

CREATE TABLE uqa_rule_column_dependency_oracle.join_events(id integer);
CREATE TABLE uqa_rule_column_dependency_oracle.join_left(disposable_value integer, key_value integer, left_value integer);
CREATE TABLE uqa_rule_column_dependency_oracle.join_right(key_value integer, right_value integer);
CREATE TABLE uqa_rule_column_dependency_oracle.join_log(value integer);
INSERT INTO uqa_rule_column_dependency_oracle.join_left VALUES (99, 1, 10);
INSERT INTO uqa_rule_column_dependency_oracle.join_right VALUES (1, 20);
CREATE RULE join_keys AS ON INSERT TO uqa_rule_column_dependency_oracle.join_events DO ALSO
  INSERT INTO uqa_rule_column_dependency_oracle.join_log
    SELECT joined.key_value
    FROM (uqa_rule_column_dependency_oracle.join_left AS left_source
    JOIN uqa_rule_column_dependency_oracle.join_right AS right_source USING (key_value))
    AS joined(key_value, disposable_value, left_value, right_value)
    WHERE joined.key_value = NEW.id;
ALTER TABLE uqa_rule_column_dependency_oracle.join_left RENAME COLUMN key_value TO left_key;
ALTER TABLE uqa_rule_column_dependency_oracle.join_left DROP COLUMN disposable_value;
INSERT INTO uqa_rule_column_dependency_oracle.join_events VALUES (1);
SELECT 'join-left-rename|' || count(*) || '|' || min(value) FROM uqa_rule_column_dependency_oracle.join_log;
SELECT 'join-deparse|' || CASE WHEN pg_get_ruledef(oid, true) LIKE '%left_source(key_value, left_value)%' AND pg_get_ruledef(oid, true) NOT LIKE '%disposable_value%' AND pg_get_ruledef(oid, true) LIKE '%USING (key_value)%' THEN 'yes' ELSE 'no' END FROM pg_rewrite WHERE rulename = 'join_keys';
ALTER TABLE uqa_rule_column_dependency_oracle.join_right RENAME COLUMN key_value TO right_key;
INSERT INTO uqa_rule_column_dependency_oracle.join_events VALUES (1);
SELECT 'join-both-renamed|' || count(*) || '|' || min(value) FROM uqa_rule_column_dependency_oracle.join_log;

CREATE TABLE uqa_rule_column_dependency_oracle.natural_events(id integer);
CREATE TABLE uqa_rule_column_dependency_oracle.natural_left(key_value integer, left_value integer);
CREATE TABLE uqa_rule_column_dependency_oracle.natural_right(key_value integer, right_value integer);
CREATE TABLE uqa_rule_column_dependency_oracle.natural_log(value integer);
INSERT INTO uqa_rule_column_dependency_oracle.natural_left VALUES (2, 10);
INSERT INTO uqa_rule_column_dependency_oracle.natural_right VALUES (2, 20);
CREATE RULE natural_keys AS ON INSERT TO uqa_rule_column_dependency_oracle.natural_events DO ALSO
  INSERT INTO uqa_rule_column_dependency_oracle.natural_log
    SELECT key_value
    FROM uqa_rule_column_dependency_oracle.natural_left AS left_source
    NATURAL JOIN uqa_rule_column_dependency_oracle.natural_right AS right_source
    WHERE left_source.key_value = NEW.id;
ALTER TABLE uqa_rule_column_dependency_oracle.natural_left RENAME COLUMN key_value TO left_key;
INSERT INTO uqa_rule_column_dependency_oracle.natural_events VALUES (2);
SELECT 'natural-left-rename|' || value FROM uqa_rule_column_dependency_oracle.natural_log;

CREATE TABLE uqa_rule_column_dependency_oracle.projection_star_events(id integer);
CREATE TABLE uqa_rule_column_dependency_oracle.projection_star_source(first_value integer, second_value integer);
CREATE TABLE uqa_rule_column_dependency_oracle.projection_star_log(first_value integer, second_value integer);
INSERT INTO uqa_rule_column_dependency_oracle.projection_star_source VALUES (3, 4);
CREATE RULE projection_star AS ON INSERT TO uqa_rule_column_dependency_oracle.projection_star_events DO ALSO
  INSERT INTO uqa_rule_column_dependency_oracle.projection_star_log
    SELECT source.* FROM uqa_rule_column_dependency_oracle.projection_star_source AS source;
ALTER TABLE uqa_rule_column_dependency_oracle.projection_star_source ADD COLUMN later_value integer DEFAULT 99;
INSERT INTO uqa_rule_column_dependency_oracle.projection_star_events VALUES (1);
SELECT 'projection-star|' || first_value || '|' || second_value FROM uqa_rule_column_dependency_oracle.projection_star_log;
SELECT pg_temp.rule_column_dependency_probe('projection-added-drop', 'ALTER TABLE uqa_rule_column_dependency_oracle.projection_star_source DROP COLUMN later_value');

CREATE TABLE uqa_rule_column_dependency_oracle.whole_row_events(id integer);
CREATE TABLE uqa_rule_column_dependency_oracle.whole_row_source(kept_value integer, disposable_value integer);
CREATE TABLE uqa_rule_column_dependency_oracle.whole_row_log(payload jsonb);
CREATE RULE whole_row_source AS ON INSERT TO uqa_rule_column_dependency_oracle.whole_row_events DO ALSO
  INSERT INTO uqa_rule_column_dependency_oracle.whole_row_log
    SELECT to_jsonb(source.*) FROM uqa_rule_column_dependency_oracle.whole_row_source AS source;
SELECT pg_temp.rule_column_dependency_probe('whole-row-drop', 'ALTER TABLE uqa_rule_column_dependency_oracle.whole_row_source DROP COLUMN disposable_value');

ALTER TABLE uqa_rule_column_dependency_oracle.source DROP COLUMN renamed_predicate_value CASCADE;
SELECT 'cascade|' || count(*) FROM pg_rewrite WHERE rulename = 'bound_columns';

DROP SCHEMA uqa_rule_column_dependency_oracle CASCADE;
