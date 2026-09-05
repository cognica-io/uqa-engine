\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rule_relation_kind_oracle CASCADE;
DROP SERVER IF EXISTS uqa_rule_relation_kind_server CASCADE;
DROP FOREIGN DATA WRAPPER IF EXISTS uqa_rule_relation_kind_fdw CASCADE;

CREATE SCHEMA uqa_rule_relation_kind_oracle;
CREATE FOREIGN DATA WRAPPER uqa_rule_relation_kind_fdw;
CREATE SERVER uqa_rule_relation_kind_server FOREIGN DATA WRAPPER uqa_rule_relation_kind_fdw;
SET search_path = uqa_rule_relation_kind_oracle, public;

CREATE OR REPLACE FUNCTION pg_temp.rule_relation_kind_probe(label text, command text)
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

CREATE TABLE kind_base(id integer);
INSERT INTO kind_base VALUES (1);
CREATE VIEW kind_view AS SELECT id FROM kind_base;
CREATE MATERIALIZED VIEW kind_materialized AS SELECT id FROM kind_base;
CREATE FOREIGN TABLE kind_foreign(id integer) SERVER uqa_rule_relation_kind_server;
CREATE TEMPORARY TABLE original_identity(kind text PRIMARY KEY, relation_oid oid, rowtype_oid oid);
INSERT INTO original_identity
SELECT CASE relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized' ELSE 'foreign' END,
       oid,
       reltype
FROM pg_class
WHERE oid IN ('kind_view'::regclass, 'kind_materialized'::regclass, 'kind_foreign'::regclass);

CREATE VIEW kind_view_wrapper AS SELECT id FROM kind_view;
CREATE VIEW kind_materialized_wrapper AS SELECT id FROM kind_materialized;
CREATE VIEW kind_foreign_wrapper AS SELECT id FROM kind_foreign;
CREATE TABLE readable_events(id integer);
CREATE TABLE readable_log(id integer);
CREATE RULE kind_view_rule AS ON INSERT TO readable_events DO ALSO
  INSERT INTO readable_log SELECT id FROM kind_view;
CREATE RULE kind_materialized_rule AS ON INSERT TO readable_events DO ALSO
  INSERT INTO readable_log SELECT id FROM kind_materialized;
CREATE TABLE foreign_events(id integer);
CREATE TABLE foreign_log(id integer);
CREATE RULE kind_foreign_rule AS ON INSERT TO foreign_events DO ALSO
  INSERT INTO foreign_log SELECT id FROM kind_foreign;

CREATE TABLE view_event_log(id integer);
CREATE VIEW kind_event_view AS SELECT id FROM view_event_log;
CREATE RULE kind_event_rule AS ON INSERT TO kind_event_view DO INSTEAD
  INSERT INTO view_event_log VALUES (NEW.id);
SELECT pg_temp.rule_relation_kind_probe(
  'event-materialized',
  'CREATE RULE invalid_materialized_event AS ON INSERT TO kind_materialized DO INSTEAD NOTHING'
);
SELECT pg_temp.rule_relation_kind_probe(
  'event-foreign',
  'CREATE RULE invalid_foreign_event AS ON INSERT TO kind_foreign DO INSTEAD NOTHING'
);

INSERT INTO readable_events VALUES (1);
SELECT 'readable-before|' || count(*) FROM readable_log;
ALTER VIEW kind_view RENAME TO renamed_kind_view;
ALTER MATERIALIZED VIEW kind_materialized RENAME TO renamed_kind_materialized;
ALTER FOREIGN TABLE kind_foreign RENAME TO renamed_kind_foreign;
ALTER VIEW kind_event_view RENAME TO renamed_kind_event_view;

BEGIN;
ALTER TABLE renamed_kind_view RENAME TO table_kind_view;
ALTER TABLE renamed_kind_materialized RENAME TO table_kind_materialized;
ALTER TABLE renamed_kind_foreign RENAME TO table_kind_foreign;
SELECT 'table-syntax-identities|' || bool_and(original.relation_oid = renamed.oid AND original.rowtype_oid = renamed.reltype)::text
FROM original_identity AS original
JOIN pg_class AS renamed
  ON renamed.relname = CASE original.kind
       WHEN 'view' THEN 'table_kind_view'
       WHEN 'materialized' THEN 'table_kind_materialized'
       ELSE 'table_kind_foreign'
     END;
ROLLBACK;

CREATE VIEW kind_view AS SELECT 101 AS id;
CREATE MATERIALIZED VIEW kind_materialized AS SELECT 102 AS id;
CREATE FOREIGN TABLE kind_foreign(id integer) SERVER uqa_rule_relation_kind_server;

SELECT 'identity-' || original.kind || '|' ||
       (original.relation_oid = renamed.oid)::text || '|' ||
       (original.rowtype_oid = renamed.reltype)::text
FROM original_identity AS original
JOIN pg_class AS renamed
  ON renamed.relname = CASE original.kind
       WHEN 'view' THEN 'renamed_kind_view'
       WHEN 'materialized' THEN 'renamed_kind_materialized'
       ELSE 'renamed_kind_foreign'
     END
ORDER BY original.kind;
SELECT 'wrapper-view|' || (position('renamed_kind_view' IN pg_get_viewdef('kind_view_wrapper'::regclass)) > 0)::text;
SELECT 'wrapper-materialized|' || (position('renamed_kind_materialized' IN pg_get_viewdef('kind_materialized_wrapper'::regclass)) > 0)::text;
SELECT 'wrapper-foreign|' || (position('renamed_kind_foreign' IN pg_get_viewdef('kind_foreign_wrapper'::regclass)) > 0)::text;
SELECT 'rule-view|' || (position('renamed_kind_view' IN pg_get_ruledef(oid)) > 0)::text
FROM pg_rewrite WHERE rulename = 'kind_view_rule';
SELECT 'rule-materialized|' || (position('renamed_kind_materialized' IN pg_get_ruledef(oid)) > 0)::text
FROM pg_rewrite WHERE rulename = 'kind_materialized_rule';
SELECT 'rule-foreign|' || (position('renamed_kind_foreign' IN pg_get_ruledef(oid)) > 0)::text
FROM pg_rewrite WHERE rulename = 'kind_foreign_rule';
SELECT 'rule-event|' || (position('renamed_kind_event_view' IN pg_get_ruledef(oid)) > 0)::text
FROM pg_rewrite WHERE rulename = 'kind_event_rule';
INSERT INTO readable_events VALUES (2);
SELECT 'readable-after|' || count(*) FROM readable_log;
INSERT INTO renamed_kind_event_view VALUES (9);
SELECT 'event-view-after|' || string_agg(id::text, ',' ORDER BY id) FROM view_event_log;

BEGIN;
ALTER VIEW renamed_kind_view RENAME TO rolled_back_kind_view;
ROLLBACK;
SELECT 'rename-rollback|' ||
       (to_regclass('renamed_kind_view') IS NOT NULL)::text || '|' ||
       (to_regclass('rolled_back_kind_view') IS NULL)::text;
SELECT pg_temp.rule_relation_kind_probe('rename-view-collision', 'ALTER VIEW renamed_kind_view RENAME TO kind_base');
SELECT pg_temp.rule_relation_kind_probe('rename-materialized-collision', 'ALTER MATERIALIZED VIEW renamed_kind_materialized RENAME TO kind_base');
SELECT pg_temp.rule_relation_kind_probe('rename-foreign-collision', 'ALTER FOREIGN TABLE renamed_kind_foreign RENAME TO kind_base');
SELECT pg_temp.rule_relation_kind_probe('rename-missing-view', 'ALTER VIEW IF EXISTS uqa_missing_rename_schema.missing_view RENAME TO missing_target');
SELECT pg_temp.rule_relation_kind_probe('rename-missing-materialized', 'ALTER MATERIALIZED VIEW IF EXISTS uqa_missing_rename_schema.missing_materialized RENAME TO missing_target');
SELECT pg_temp.rule_relation_kind_probe('rename-missing-foreign', 'ALTER FOREIGN TABLE IF EXISTS uqa_missing_rename_schema.missing_foreign RENAME TO missing_target');
SELECT pg_temp.rule_relation_kind_probe('rename-missing-table', 'ALTER TABLE IF EXISTS uqa_missing_rename_schema.missing_table RENAME TO missing_target');

DROP VIEW kind_view_wrapper;
DROP VIEW kind_materialized_wrapper;
DROP VIEW kind_foreign_wrapper;
SELECT pg_temp.rule_relation_kind_probe('drop-view-restrict', 'DROP VIEW renamed_kind_view RESTRICT');
SELECT pg_temp.rule_relation_kind_probe('drop-materialized-restrict', 'DROP MATERIALIZED VIEW renamed_kind_materialized RESTRICT');
SELECT pg_temp.rule_relation_kind_probe('drop-foreign-restrict', 'DROP FOREIGN TABLE renamed_kind_foreign RESTRICT');
DROP VIEW renamed_kind_view CASCADE;
DROP MATERIALIZED VIEW renamed_kind_materialized CASCADE;
DROP FOREIGN TABLE renamed_kind_foreign CASCADE;
SELECT 'rules-after-cascade|' || count(*)
FROM pg_rewrite
WHERE rulename IN ('kind_view_rule', 'kind_materialized_rule', 'kind_foreign_rule');

DROP SCHEMA uqa_rule_relation_kind_oracle CASCADE;
DROP SERVER uqa_rule_relation_kind_server CASCADE;
DROP FOREIGN DATA WRAPPER uqa_rule_relation_kind_fdw CASCADE;
