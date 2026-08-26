-- Stateful PostgreSQL 18.4 rewrite-rule parity fixture.
-- The runner replaces __UQA_STATEFUL_SCHEMA__ and executes each delimited case in order.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- @case create_source ok
CREATE TABLE rule_source (id integer PRIMARY KEY, value integer);
-- @end

-- @case create_items ok
CREATE TABLE rule_items (id integer PRIMARY KEY, value integer, disposable integer);
-- @end

-- @case create_log ok
CREATE TABLE rule_log (seq bigserial PRIMARY KEY, message text);
-- @end

-- @case create_condition_items ok
CREATE TABLE condition_items (id integer);
-- @end

-- @case create_unqualified_insert_condition ok
CREATE RULE suppress_small AS ON INSERT TO condition_items WHERE id < 10 DO INSTEAD NOTHING;
-- @end

-- @case exercise_unqualified_insert_condition ok
INSERT INTO condition_items VALUES (1), (10);
-- @end

-- @case unqualified_insert_condition_rows rows
SELECT id FROM condition_items ORDER BY id;
-- @end

-- @case create_unqualified_delete_condition ok
CREATE RULE retain_ten AS ON DELETE TO condition_items WHERE id = 10 DO INSTEAD NOTHING;
-- @end

-- @case exercise_unqualified_delete_condition ok
DELETE FROM condition_items;
-- @end

-- @case unqualified_delete_condition_rows rows
SELECT id FROM condition_items ORDER BY id;
-- @end

-- @case ambiguous_unqualified_update_condition error
CREATE RULE ambiguous_update AS ON UPDATE TO condition_items WHERE id = 10 DO INSTEAD NOTHING;
-- @end

-- @case create_nullable_rule_items ok
CREATE TABLE nullable_rule_items (id integer PRIMARY KEY, optional integer);
-- @end

-- @case create_nullable_rule_log ok
CREATE TABLE nullable_rule_log (value integer);
-- @end

-- @case create_nullable_rule ok
CREATE RULE log_optional AS ON INSERT TO nullable_rule_items DO ALSO INSERT INTO nullable_rule_log VALUES (NEW.optional);
-- @end

-- @case insert_missing_nullable_rule_value ok
INSERT INTO nullable_rule_items (id) VALUES (7);
-- @end

-- @case missing_nullable_rule_value rows
SELECT value FROM nullable_rule_log;
-- @end

-- @case seed_source ok
INSERT INTO rule_source VALUES (3, 30), (4, 40);
-- @end

-- @case create_insert_b ok
CREATE RULE b_insert AS ON INSERT TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('b:' || NEW.id || ':' || NEW.value);
-- @end

-- @case create_insert_a ok
CREATE RULE a_insert AS ON INSERT TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('a:' || NEW.id || ':' || NEW.value);
-- @end

-- @case create_insert_suppress ok
CREATE RULE suppress_two AS ON INSERT TO rule_items WHERE NEW.id = 2 DO INSTEAD NOTHING;
-- @end

-- @case insert_values ok
INSERT INTO rule_items VALUES (1, 10, 100), (2, 20, 200);
-- @end

-- @case insert_value_rows rows
SELECT id, value, disposable FROM rule_items ORDER BY id;
-- @end

-- @case insert_value_actions rows
SELECT message FROM rule_log ORDER BY seq;
-- @end

-- @case insert_select ok
INSERT INTO rule_items SELECT id, value, value * 10 FROM rule_source ORDER BY id;
-- @end

-- @case insert_select_rows rows
SELECT id, value, disposable FROM rule_items ORDER BY id;
-- @end

-- @case insert_select_actions rows
SELECT message FROM rule_log ORDER BY seq;
-- @end

-- @case create_update_log ok
CREATE RULE update_log AS ON UPDATE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('u:' || OLD.id || ':' || OLD.value || ':' || NEW.value);
-- @end

-- @case create_update_constant ok
CREATE RULE update_constant AS ON UPDATE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('update-constant');
-- @end

-- @case update_items ok
UPDATE rule_items SET value = value + 1;
-- @end

-- @case update_rows rows
SELECT id, value FROM rule_items ORDER BY id;
-- @end

-- @case update_actions rows
SELECT message FROM rule_log WHERE message LIKE 'u:%' OR message = 'update-constant' ORDER BY seq;
-- @end

-- @case create_delete_log ok
CREATE RULE delete_log AS ON DELETE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('d:' || OLD.id || ':' || OLD.value);
-- @end

-- @case create_delete_keep ok
CREATE RULE keep_four AS ON DELETE TO rule_items WHERE OLD.id = 4 DO INSTEAD NOTHING;
-- @end

-- @case delete_items ok
DELETE FROM rule_items;
-- @end

-- @case delete_rows rows
SELECT id, value FROM rule_items ORDER BY id;
-- @end

-- @case delete_actions rows
SELECT message FROM rule_log WHERE message LIKE 'd:%' ORDER BY seq;
-- @end

-- @case catalog_flags rows
SELECT c.relhasrules, t.hasrules
FROM pg_catalog.pg_class AS c
JOIN pg_catalog.pg_tables AS t ON t.tablename = c.relname
WHERE c.relname = 'rule_items';
-- @end

-- @case catalog_rule rows
SELECT r.rulename, r.ev_type, r.ev_enabled, r.is_instead,
       pg_catalog.pg_get_ruledef(r.oid, true) LIKE 'CREATE RULE %' AS has_definition
FROM pg_catalog.pg_rewrite AS r
JOIN pg_catalog.pg_class AS c ON c.oid = r.ev_class
WHERE c.relname = 'rule_items' AND r.rulename = 'a_insert';
-- @end

-- @case catalog_pg_rules rows
SELECT schemaname = current_schema() AS schema_matches, tablename, rulename,
       definition LIKE 'CREATE RULE a_insert%' AS has_definition
FROM pg_catalog.pg_rules
WHERE tablename = 'rule_items' AND rulename = 'a_insert';
-- @end

-- @case disable_insert_rule ok
ALTER TABLE rule_items DISABLE RULE a_insert;
-- @end

-- @case replace_disabled_rule ok
CREATE OR REPLACE RULE a_insert AS ON INSERT TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('replaced:' || NEW.id);
-- @end

-- @case disabled_mode_survives_replace rows
SELECT ev_enabled FROM pg_catalog.pg_rewrite AS r
JOIN pg_catalog.pg_class AS c ON c.oid = r.ev_class
WHERE c.relname = 'rule_items' AND r.rulename = 'a_insert';
-- @end

-- @case rename_rule ok
ALTER RULE a_insert ON rule_items RENAME TO renamed_insert;
-- @end

-- @case enable_renamed_rule ok
ALTER TABLE rule_items ENABLE RULE renamed_insert;
-- @end

-- @case create_recursive_table ok
CREATE TABLE recursive_items (id integer PRIMARY KEY);
-- @end

-- @case create_recursive_rule ok
CREATE RULE recursive_insert AS ON INSERT TO __UQA_STATEFUL_SCHEMA__.recursive_items DO ALSO INSERT INTO __UQA_STATEFUL_SCHEMA__.recursive_items VALUES (NEW.id + 100);
-- @end

-- @case recursive_insert error
INSERT INTO recursive_items VALUES (1);
-- @end

-- @case recursive_rollback rows
SELECT count(*) FROM recursive_items;
-- @end

-- @case on_conflict_with_rule error
INSERT INTO rule_items VALUES (5, 50, 500) ON CONFLICT DO NOTHING;
-- @end

-- @case merge_with_rule error
MERGE INTO recursive_items AS target
USING (VALUES (1)) AS source(id)
ON target.id = source.id
WHEN NOT MATCHED THEN INSERT VALUES (source.id);
-- @end

-- @case insert_old_rejected error
CREATE RULE invalid_old AS ON INSERT TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES (OLD.id::text);
-- @end

-- @case delete_new_rejected error
CREATE RULE invalid_new AS ON DELETE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES (NEW.id::text);
-- @end

-- @case non_select_return_name_rejected error
CREATE RULE "_RETURN" AS ON INSERT TO rule_items DO INSTEAD NOTHING;
-- @end

-- @case create_column_rule ok
CREATE RULE disposable_rule AS ON UPDATE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES (NEW.disposable::text);
-- @end

-- @case create_deparse_literal_rule ok
CREATE RULE deparse_literal_rule AS ON UPDATE TO rule_items DO ALSO INSERT INTO rule_log(message) VALUES ('NEW.disposable');
-- @end

-- @case rename_rule_column ok
ALTER TABLE rule_items RENAME COLUMN disposable TO payload;
-- @end

-- @case renamed_rule_literal_preserved rows
SELECT pg_get_ruledef(oid, true) LIKE '%''NEW.disposable''%' AS literal_preserved
FROM pg_catalog.pg_rewrite
WHERE rulename = 'deparse_literal_rule';
-- @end

-- @case renamed_rule_column_executes ok
UPDATE rule_items SET payload = payload + 1 WHERE id = 4;
-- @end

-- @case renamed_rule_column_action rows
SELECT message FROM rule_log WHERE message = '401';
-- @end

-- @case drop_rule_column_restrict error
ALTER TABLE rule_items DROP COLUMN payload;
-- @end

-- @case drop_rule_column_cascade ok
ALTER TABLE rule_items DROP COLUMN payload CASCADE;
-- @end

-- @case cascaded_rule_removed rows
SELECT count(*) FROM pg_catalog.pg_rewrite AS r
JOIN pg_catalog.pg_class AS c ON c.oid = r.ev_class
WHERE c.relname = 'rule_items' AND r.rulename = 'disposable_rule';
-- @end

-- @case create_view ok
CREATE VIEW rule_view AS SELECT id, value FROM rule_source;
-- @end

-- @case create_materialized_view ok
CREATE MATERIALIZED VIEW rule_snapshot AS SELECT id, value FROM rule_source;
-- @end

-- @case materialized_view_rule_rejected error
CREATE RULE snapshot_update AS ON UPDATE TO rule_snapshot DO INSTEAD NOTHING;
-- @end

-- @case replace_return_rule ok
CREATE OR REPLACE RULE "_RETURN" AS ON SELECT TO rule_view DO INSTEAD SELECT id, value + 1 AS value FROM rule_source;
-- @end

-- @case replaced_view_rows rows
SELECT id, value FROM rule_view ORDER BY id;
-- @end

-- @case return_rule_catalog rows
SELECT count(*) FROM pg_catalog.pg_rewrite AS r
JOIN pg_catalog.pg_class AS c ON c.oid = r.ev_class
WHERE c.relname = 'rule_view' AND r.rulename = '_RETURN';
-- @end

-- @case drop_return_rule_rejected error
DROP RULE "_RETURN" ON rule_view;
-- @end

-- @case rename_return_rule_rejected error
ALTER RULE "_RETURN" ON rule_view RENAME TO renamed_return;
-- @end

-- @case disable_return_rule_rejected error
ALTER TABLE rule_view DISABLE RULE "_RETURN";
-- @end
