\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rule_row_expansion_oracle CASCADE;
CREATE SCHEMA uqa_rule_row_expansion_oracle;

CREATE OR REPLACE FUNCTION pg_temp.rule_row_expansion_probe(label text, command text)
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

CREATE TABLE uqa_rule_row_expansion_oracle.update_source(z integer, a integer DEFAULT 0);
CREATE TABLE uqa_rule_row_expansion_oracle.update_log(seq bigserial PRIMARY KEY, z integer, a integer, tag text);
INSERT INTO uqa_rule_row_expansion_oracle.update_source VALUES (1, 2), (11, 12);
CREATE RULE update_star AS ON UPDATE TO uqa_rule_row_expansion_oracle.update_source DO ALSO INSERT INTO uqa_rule_row_expansion_oracle.update_log(z, a, tag) VALUES (OLD.*, 'old'), (NEW.*, 'new');
UPDATE uqa_rule_row_expansion_oracle.update_source SET a = a + 1;
SELECT 'update|' || string_agg(z || ':' || a || ':' || tag, ',' ORDER BY seq) FROM uqa_rule_row_expansion_oracle.update_log;

CREATE TABLE uqa_rule_row_expansion_oracle.insert_source(z integer, a integer DEFAULT 0);
CREATE TABLE uqa_rule_row_expansion_oracle.insert_log(seq bigserial PRIMARY KEY, z integer, a integer, tag text);
CREATE RULE insert_star AS ON INSERT TO uqa_rule_row_expansion_oracle.insert_source DO ALSO INSERT INTO uqa_rule_row_expansion_oracle.insert_log(z, a, tag) VALUES (NULL, NULL, 'constant'), (NEW.*, 'new');
INSERT INTO uqa_rule_row_expansion_oracle.insert_source VALUES (22, 23), (33, DEFAULT);
SELECT 'insert|' || string_agg(coalesce(z::text, '-') || ':' || coalesce(a::text, '-') || ':' || tag, ',' ORDER BY seq) FROM uqa_rule_row_expansion_oracle.insert_log;

CREATE TABLE uqa_rule_row_expansion_oracle.redirect_source(z integer, a integer DEFAULT 0);
CREATE TABLE uqa_rule_row_expansion_oracle.redirect_target(z integer, a integer);
CREATE RULE redirect_star AS ON INSERT TO uqa_rule_row_expansion_oracle.redirect_source DO INSTEAD INSERT INTO uqa_rule_row_expansion_oracle.redirect_target SELECT NEW.*;
INSERT INTO uqa_rule_row_expansion_oracle.redirect_source VALUES (41, 42), (51, DEFAULT);
SELECT 'redirect|' || (SELECT count(*) FROM uqa_rule_row_expansion_oracle.redirect_source) || '|' || (SELECT string_agg(z || ':' || a, ',' ORDER BY z) FROM uqa_rule_row_expansion_oracle.redirect_target);

CREATE TABLE uqa_rule_row_expansion_oracle.row_source(z integer, a text);
CREATE TABLE uqa_rule_row_expansion_oracle.row_log(seq bigserial PRIMARY KEY, matched boolean);
CREATE RULE row_star AS ON INSERT TO uqa_rule_row_expansion_oracle.row_source DO ALSO INSERT INTO uqa_rule_row_expansion_oracle.row_log(matched) VALUES (ROW(NEW.*) = ROW(5, 'five'));
INSERT INTO uqa_rule_row_expansion_oracle.row_source VALUES (5, 'five'), (6, 'other');
SELECT 'row|' || string_agg(matched::text, ',' ORDER BY seq) FROM uqa_rule_row_expansion_oracle.row_log;

CREATE TABLE uqa_rule_row_expansion_oracle.scope_source(z integer, a text);
CREATE TABLE uqa_rule_row_expansion_oracle.scope_target(z integer, a text);
SELECT pg_temp.rule_row_expansion_probe('invalid-old', 'CREATE RULE invalid_old_star AS ON INSERT TO uqa_rule_row_expansion_oracle.scope_source DO ALSO INSERT INTO uqa_rule_row_expansion_oracle.scope_target VALUES (OLD.*)');
SELECT pg_temp.rule_row_expansion_probe('invalid-new', 'CREATE RULE invalid_new_star AS ON DELETE TO uqa_rule_row_expansion_oracle.scope_source DO ALSO INSERT INTO uqa_rule_row_expansion_oracle.scope_target VALUES (NEW.*)');
SELECT pg_temp.rule_row_expansion_probe('cte-scope', 'CREATE RULE cte_star AS ON INSERT TO uqa_rule_row_expansion_oracle.scope_source DO ALSO WITH item AS (SELECT NEW.*) INSERT INTO uqa_rule_row_expansion_oracle.scope_target SELECT * FROM item');
SELECT pg_temp.rule_row_expansion_probe('set-scope', 'CREATE RULE set_star AS ON INSERT TO uqa_rule_row_expansion_oracle.scope_source DO ALSO INSERT INTO uqa_rule_row_expansion_oracle.scope_target SELECT NEW.* UNION ALL SELECT 99, ''constant''');
CREATE RULE nested_alias_star AS ON INSERT TO uqa_rule_row_expansion_oracle.scope_source DO ALSO INSERT INTO uqa_rule_row_expansion_oracle.scope_target SELECT nested.* FROM (SELECT old.* FROM (VALUES (9, 'local')) AS old(z, a)) AS nested;
INSERT INTO uqa_rule_row_expansion_oracle.scope_source VALUES (1, 'event');
SELECT 'shadow|' || string_agg(z || ':' || a, ',' ORDER BY z) FROM uqa_rule_row_expansion_oracle.scope_target;

CREATE TABLE uqa_rule_row_expansion_oracle.lifecycle_source(z integer, a text);
CREATE TABLE uqa_rule_row_expansion_oracle.lifecycle_target(z integer, a text);
CREATE RULE lifecycle_star AS ON INSERT TO uqa_rule_row_expansion_oracle.lifecycle_source DO ALSO INSERT INTO uqa_rule_row_expansion_oracle.lifecycle_target VALUES (NEW.*);
ALTER TABLE uqa_rule_row_expansion_oracle.lifecycle_source ADD COLUMN later integer DEFAULT 7;
INSERT INTO uqa_rule_row_expansion_oracle.lifecycle_source VALUES (1, 'one', 9);
SELECT 'add-stable|' || string_agg(z || ':' || a, ',' ORDER BY z) FROM uqa_rule_row_expansion_oracle.lifecycle_target;
SELECT pg_temp.rule_row_expansion_probe('drop-restrict', 'ALTER TABLE uqa_rule_row_expansion_oracle.lifecycle_source DROP COLUMN a');
ALTER TABLE uqa_rule_row_expansion_oracle.lifecycle_source RENAME COLUMN a TO renamed;
SELECT 'rename-definition|' || (pg_get_ruledef(oid, true) LIKE '%new.renamed%') || '|' || (pg_get_ruledef(oid, true) NOT LIKE '%new.later%') FROM pg_rewrite WHERE rulename = 'lifecycle_star';
INSERT INTO uqa_rule_row_expansion_oracle.lifecycle_source VALUES (2, 'two', 10);
SELECT 'rename-row|' || string_agg(z || ':' || a, ',' ORDER BY z) FROM uqa_rule_row_expansion_oracle.lifecycle_target;
ALTER TABLE uqa_rule_row_expansion_oracle.lifecycle_source DROP COLUMN renamed CASCADE;
SELECT 'drop-cascade|' || count(*) FROM pg_rewrite WHERE rulename = 'lifecycle_star';

DROP SCHEMA uqa_rule_row_expansion_oracle CASCADE;
