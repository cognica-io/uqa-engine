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

-- @case create_set_rule_source ok
CREATE TABLE set_rule_source (id integer PRIMARY KEY, value integer);
-- @end

-- @case create_set_rule_target ok
CREATE TABLE set_rule_target (id integer PRIMARY KEY, value integer);
-- @end

-- @case create_set_rule_rows ok
CREATE TABLE set_rule_rows (id integer);
-- @end

-- @case create_set_rule_statements ok
CREATE TABLE set_rule_statements (seq bigserial PRIMARY KEY, event text);
-- @end

-- @case seed_set_rule_relations ok
INSERT INTO set_rule_source VALUES (1, 10), (2, 20);
INSERT INTO set_rule_target VALUES (1, 0);
-- @end

-- @case create_set_rule_trigger_function ok
CREATE FUNCTION log_set_rule_statement() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO set_rule_statements(event) VALUES (TG_OP); RETURN NULL; END $$;
-- @end

-- @case create_set_rule_insert_trigger ok
CREATE TRIGGER log_set_rule_insert AFTER INSERT ON set_rule_rows FOR EACH STATEMENT EXECUTE FUNCTION log_set_rule_statement();
-- @end

-- @case create_set_rule_update_trigger ok
CREATE TRIGGER log_set_rule_update AFTER UPDATE ON set_rule_target FOR EACH STATEMENT EXECUTE FUNCTION log_set_rule_statement();
-- @end

-- @case create_set_rule_insert_action ok
CREATE RULE a_set_insert AS ON UPDATE TO set_rule_source DO ALSO INSERT INTO set_rule_rows VALUES (NEW.id);
-- @end

-- @case create_set_rule_update_action ok
CREATE RULE b_set_update AS ON UPDATE TO set_rule_source DO ALSO UPDATE set_rule_target SET value = NEW.id WHERE NEW.id = 1;
-- @end

-- @case execute_set_rule_actions ok
UPDATE set_rule_source SET value = value + 1;
-- @end

-- @case set_rule_insert_rows rows
SELECT id FROM set_rule_rows ORDER BY id;
-- @end

-- @case set_rule_update_once rows
SELECT value FROM set_rule_target;
-- @end

-- @case set_rule_statement_cardinality rows
SELECT event FROM set_rule_statements ORDER BY seq;
-- @end

-- @case create_lateral_rule_relations ok
CREATE TABLE lateral_rule_event (id integer PRIMARY KEY, value integer);
CREATE TABLE lateral_rule_log (value integer);
-- @end

-- @case seed_lateral_rule_event ok
INSERT INTO lateral_rule_event VALUES (1, 10), (2, 20);
-- @end

-- @case create_lateral_rule_action ok
CREATE RULE lateral_rule_action AS ON UPDATE TO lateral_rule_event DO ALSO INSERT INTO lateral_rule_log SELECT item.value FROM LATERAL (SELECT NEW.value AS value) AS item;
-- @end

-- @case execute_lateral_rule_action ok
UPDATE lateral_rule_event SET value = value + 1;
-- @end

-- @case lateral_rule_action_rows rows
SELECT value FROM lateral_rule_log ORDER BY value;
-- @end

-- @case create_alias_collision_rule_relations ok
CREATE TABLE alias_collision_rule_event (id integer PRIMARY KEY, value integer);
CREATE TABLE alias_collision_rule_source (__new_0 integer);
CREATE TABLE alias_collision_rule_log (event_id integer, source_value integer);
-- @end

-- @case seed_alias_collision_rule_relations ok
INSERT INTO alias_collision_rule_event VALUES (1, 10);
INSERT INTO alias_collision_rule_source VALUES (999);
-- @end

-- @case create_alias_collision_rule_action ok
CREATE RULE alias_collision_rule_action AS ON UPDATE TO alias_collision_rule_event DO ALSO INSERT INTO alias_collision_rule_log SELECT NEW.id, __uqa_rule_rows_0_0.__new_0 FROM alias_collision_rule_source AS __uqa_rule_rows_0_0;
-- @end

-- @case execute_alias_collision_rule_action ok
UPDATE alias_collision_rule_event SET value = value + 1;
-- @end

-- @case alias_collision_rule_action_rows rows
SELECT event_id, source_value FROM alias_collision_rule_log;
-- @end

-- @case create_lazy_rule_returning_relations ok
CREATE SEQUENCE lazy_rule_returning_sequence START 1;
CREATE TABLE lazy_rule_returning_event (id bigint);
CREATE TABLE lazy_rule_returning_action (id bigint);
-- @end

-- @case create_lazy_rule_returning_provider ok
CREATE RULE lazy_rule_returning_provider AS ON INSERT TO lazy_rule_returning_event DO INSTEAD INSERT INTO lazy_rule_returning_action VALUES (NEW.id) RETURNING nextval('lazy_rule_returning_sequence');
-- @end

-- @case execute_lazy_rule_returning_without_outer_projection ok
INSERT INTO lazy_rule_returning_event VALUES (10);
-- @end

-- @case lazy_rule_returning_sequence_rows rows
SELECT nextval('lazy_rule_returning_sequence');
-- @end

-- @case lazy_rule_returning_action_rows rows
SELECT id FROM lazy_rule_returning_action;
-- @end

-- @case create_action_image_rule_relations ok
CREATE TABLE action_image_rule_event (x integer, y integer);
CREATE TABLE action_image_rule_target (id integer, value integer);
-- @end

-- @case seed_action_image_rule_event ok
INSERT INTO action_image_rule_event VALUES (1, 10), (2, 20);
-- @end

-- @case create_action_image_rule_provider ok
CREATE RULE action_image_rule_provider AS ON UPDATE TO action_image_rule_event DO INSTEAD INSERT INTO action_image_rule_target VALUES (42, 43) RETURNING NEW.id, NEW.value;
-- @end

-- @case action_image_rule_provider_rows rows
UPDATE action_image_rule_event SET y = y + 1 RETURNING x, y;
-- @end

-- @case action_image_rule_target_rows rows
SELECT id, value FROM action_image_rule_target;
-- @end

-- @case create_scoped_rule_relations ok
CREATE TABLE scoped_rule_event (event_value integer);
CREATE TABLE scoped_rule_target (action_value integer PRIMARY KEY);
-- @end

-- @case set_operation_rule_reference_rejected error
CREATE RULE set_operation_rule AS ON INSERT TO scoped_rule_event DO ALSO INSERT INTO scoped_rule_target SELECT NEW.event_value UNION ALL SELECT 999;
-- @end

-- @case create_constant_set_operation_rule ok
CREATE RULE constant_set_operation_rule AS ON INSERT TO scoped_rule_event DO ALSO INSERT INTO scoped_rule_target SELECT 1000 UNION ALL SELECT 1001;
-- @end

-- @case conditional_set_operation_rule_rejected error
CREATE RULE conditional_set_operation_rule AS ON UPDATE TO scoped_rule_event WHERE NEW.event_value > 0 DO ALSO INSERT INTO scoped_rule_target SELECT 1002 UNION ALL SELECT 1003;
-- @end

-- @case execute_single_row_set_operation_rule ok
INSERT INTO scoped_rule_event VALUES (1);
-- @end

-- @case single_row_set_operation_rule_rows rows
SELECT action_value FROM scoped_rule_target ORDER BY action_value;
-- @end

-- @case clear_single_row_set_operation_rule ok
DELETE FROM scoped_rule_event;
DELETE FROM scoped_rule_target;
-- @end

-- @case rewritten_set_operation_rule_rejected error
INSERT INTO scoped_rule_event VALUES (1), (2);
-- @end

-- @case rewritten_set_operation_event_rollback rows
SELECT count(*) FROM scoped_rule_event;
-- @end

-- @case rewritten_set_operation_action_rollback rows
SELECT count(*) FROM scoped_rule_target;
-- @end

-- @case cte_rule_reference_rejected error
CREATE RULE cte_rule AS ON INSERT TO scoped_rule_event DO ALSO WITH item AS (SELECT NEW.event_value AS value) INSERT INTO scoped_rule_target SELECT value FROM item;
-- @end

-- @case conflict_rule_reference_rejected error
CREATE RULE conflict_rule AS ON INSERT TO scoped_rule_event DO ALSO INSERT INTO scoped_rule_target VALUES (NEW.event_value) ON CONFLICT (action_value) DO UPDATE SET action_value = NEW.event_value;
-- @end

-- @case returning_event_image_reference_rejected error
CREATE RULE returning_event_image_rule AS ON UPDATE TO scoped_rule_event DO INSTEAD INSERT INTO scoped_rule_target VALUES (1) RETURNING NEW.event_value;
-- @end

-- @case create_rule_action_view ok
CREATE VIEW scoped_rule_view AS SELECT action_value FROM scoped_rule_target;
-- @end

-- @case create_view_target_rule_action ok
CREATE RULE view_target_rule_action AS ON INSERT TO scoped_rule_event DO ALSO INSERT INTO scoped_rule_view VALUES (NEW.event_value);
-- @end

-- @case create_insert_returning_event ok
CREATE TABLE insert_returning_event (z integer, a text);
-- @end

-- @case create_insert_returning_action ok
CREATE TABLE insert_returning_action (mapped_z integer, mapped_a text);
-- @end

-- @case create_insert_returning_provider ok
CREATE RULE insert_returning_provider AS ON INSERT TO insert_returning_event DO INSTEAD INSERT INTO insert_returning_action VALUES (NEW.z, NEW.a) RETURNING mapped_z + 10, mapped_a || '!';
-- @end

-- @case insert_returning_provider_rows rows
INSERT INTO insert_returning_event VALUES (1, 'one'), (2, 'two') RETURNING old.z AS old_z, new.z AS new_z, z * 2 AS doubled, a;
-- @end

-- @case insert_returning_event_suppressed rows
SELECT count(*) FROM insert_returning_event;
-- @end

-- @case insert_returning_action_rows rows
SELECT mapped_z, mapped_a FROM insert_returning_action ORDER BY mapped_z;
-- @end

-- @case create_update_returning_event ok
CREATE TABLE update_returning_event (id integer PRIMARY KEY, value integer);
-- @end

-- @case create_update_returning_action ok
CREATE TABLE update_returning_action (id integer PRIMARY KEY, mapped integer);
-- @end

-- @case seed_update_returning_relations ok
INSERT INTO update_returning_event VALUES (1, 10);
INSERT INTO update_returning_action VALUES (1, 100);
-- @end

-- @case create_update_returning_provider ok
CREATE RULE update_returning_provider AS ON UPDATE TO update_returning_event DO INSTEAD UPDATE update_returning_action SET mapped = NEW.value + 10 WHERE id = OLD.id RETURNING id, mapped + 100;
-- @end

-- @case update_returning_provider_rows rows
UPDATE update_returning_event SET value = value + 1 RETURNING old.value AS old_value, new.value AS new_value, value;
-- @end

-- @case update_returning_relations rows
SELECT 'action' AS relation, mapped AS value FROM update_returning_action UNION ALL SELECT 'event', value FROM update_returning_event ORDER BY relation;
-- @end

-- @case create_alias_returning_event ok
CREATE TABLE alias_returning_event (id integer PRIMARY KEY, value integer);
-- @end

-- @case create_alias_returning_action ok
CREATE TABLE alias_returning_action (id integer PRIMARY KEY, mapped integer);
-- @end

-- @case seed_alias_returning_relations ok
INSERT INTO alias_returning_event VALUES (1, 10);
INSERT INTO alias_returning_action VALUES (1, 100);
-- @end

-- @case create_alias_returning_provider ok
CREATE RULE alias_returning_provider AS ON UPDATE TO alias_returning_event DO INSTEAD UPDATE alias_returning_action SET mapped = NEW.value + 10 WHERE id = OLD.id RETURNING WITH (OLD AS action_old, NEW AS action_new) id, action_old.mapped + action_new.mapped;
-- @end

-- @case alias_returning_provider_rows rows
UPDATE alias_returning_event SET value = value + 1 RETURNING old.value AS old_value, new.value AS new_value, value;
-- @end

-- @case create_delete_returning_event ok
CREATE TABLE delete_returning_event (id integer PRIMARY KEY, value integer);
-- @end

-- @case create_delete_returning_action ok
CREATE TABLE delete_returning_action (id integer PRIMARY KEY, mapped integer);
-- @end

-- @case seed_delete_returning_relations ok
INSERT INTO delete_returning_event VALUES (1, 10);
INSERT INTO delete_returning_action VALUES (1, 100);
-- @end

-- @case create_delete_returning_provider ok
CREATE RULE delete_returning_provider AS ON DELETE TO delete_returning_event DO INSTEAD DELETE FROM delete_returning_action WHERE id = OLD.id RETURNING id, mapped + 10;
-- @end

-- @case delete_returning_provider_rows rows
DELETE FROM delete_returning_event RETURNING old.value AS old_value, new.value AS new_value, value;
-- @end

-- @case delete_returning_relations rows
SELECT (SELECT count(*) FROM delete_returning_event) AS event_count, (SELECT count(*) FROM delete_returning_action) AS action_count;
-- @end

-- @case create_returning_validation_event ok
CREATE TABLE returning_validation_event (id integer, note varchar(3));
-- @end

-- @case create_returning_validation_action ok
CREATE TABLE returning_validation_action (id bigint, note varchar(20));
-- @end

-- @case returning_wrong_type_rejected error
CREATE RULE returning_wrong_type AS ON INSERT TO returning_validation_event DO INSTEAD INSERT INTO returning_validation_action VALUES (NEW.id, NEW.note) RETURNING id, note::varchar(3);
-- @end

-- @case returning_wrong_size_rejected error
CREATE RULE returning_wrong_size AS ON INSERT TO returning_validation_event DO INSTEAD INSERT INTO returning_validation_action VALUES (NEW.id, NEW.note) RETURNING id::integer, note;
-- @end

-- @case returning_too_few_rejected error
CREATE RULE returning_too_few AS ON INSERT TO returning_validation_event DO INSTEAD INSERT INTO returning_validation_action VALUES (NEW.id, NEW.note) RETURNING id::integer;
-- @end

-- @case conditional_returning_rejected error
CREATE RULE conditional_returning AS ON INSERT TO returning_validation_event WHERE NEW.id > 0 DO INSTEAD INSERT INTO returning_validation_action VALUES (NEW.id, NEW.note) RETURNING id::integer, note::varchar(3);
-- @end

-- @case non_instead_returning_rejected error
CREATE RULE non_instead_returning AS ON INSERT TO returning_validation_event DO ALSO INSERT INTO returning_validation_action VALUES (NEW.id, NEW.note) RETURNING id::integer, note::varchar(3);
-- @end

-- @case multiple_action_returning_rejected error
CREATE RULE multiple_action_returning AS ON INSERT TO returning_validation_event DO INSTEAD (INSERT INTO returning_validation_action VALUES (NEW.id, NEW.note) RETURNING id::integer, note::varchar(3); INSERT INTO returning_validation_action VALUES (NEW.id + 1, NEW.note) RETURNING id::integer, note::varchar(3););
-- @end

-- @case insert_action_event_returning_rejected error
CREATE RULE insert_action_event_returning AS ON INSERT TO returning_validation_event DO INSTEAD INSERT INTO returning_validation_action VALUES (NEW.id, NEW.note) RETURNING NEW.id, note::varchar(3);
-- @end

-- @case create_returning_contract_source ok
CREATE TABLE returning_contract_source (id integer);
-- @end

-- @case create_returning_conditional_suppress ok
CREATE RULE returning_conditional_suppress AS ON INSERT TO returning_contract_source WHERE NEW.id < 0 DO INSTEAD NOTHING;
-- @end

-- @case returning_without_provider_rejected error
INSERT INTO returning_contract_source VALUES (2) RETURNING id;
-- @end

-- @case returning_without_provider_rollback rows
SELECT id FROM returning_contract_source ORDER BY id;
-- @end

-- @case create_returning_provider_a_table ok
CREATE TABLE returning_provider_a (id integer);
-- @end

-- @case create_returning_provider_b_table ok
CREATE TABLE returning_provider_b (id integer);
-- @end

-- @case create_returning_provider_a ok
CREATE RULE returning_provider_a_rule AS ON INSERT TO returning_contract_source DO INSTEAD INSERT INTO returning_provider_a VALUES (NEW.id) RETURNING id;
-- @end

-- @case create_returning_provider_b ok
CREATE RULE returning_provider_b_rule AS ON INSERT TO returning_contract_source DO INSTEAD INSERT INTO returning_provider_b VALUES (NEW.id) RETURNING id;
-- @end

-- @case multiple_rule_returning_rejected error
INSERT INTO returning_contract_source VALUES (3) RETURNING id;
-- @end

-- @case multiple_rule_returning_rollback rows
SELECT (SELECT count(*) FROM returning_provider_a) AS a_count, (SELECT count(*) FROM returning_provider_b) AS b_count;
-- @end

-- @case create_update_context_event ok
CREATE TABLE update_context_event (id integer PRIMARY KEY, value integer);
-- @end

-- @case create_update_context_source ok
CREATE TABLE update_context_source (id integer PRIMARY KEY, delta integer);
-- @end

-- @case create_update_context_action ok
CREATE TABLE update_context_action (id integer PRIMARY KEY, mapped integer);
-- @end

-- @case seed_update_context_relations ok
INSERT INTO update_context_event VALUES (1, 10);
INSERT INTO update_context_source VALUES (1, 5);
INSERT INTO update_context_action VALUES (1, 100);
-- @end

-- @case create_update_context_provider ok
CREATE RULE update_context_provider AS ON UPDATE TO update_context_event DO INSTEAD UPDATE update_context_action SET mapped = NEW.value WHERE id = OLD.id RETURNING id, mapped;
-- @end

-- @case update_context_provider_rows rows
UPDATE update_context_event AS event SET value = event.value + source.delta FROM update_context_source AS source WHERE event.id = source.id RETURNING source.delta, old.value, new.value;
-- @end

-- @case update_context_relations rows
SELECT 'action' AS relation, mapped AS value FROM update_context_action UNION ALL SELECT 'event', value FROM update_context_event ORDER BY relation;
-- @end

-- @case create_delete_context_event ok
CREATE TABLE delete_context_event (id integer PRIMARY KEY, value integer);
-- @end

-- @case create_delete_context_source ok
CREATE TABLE delete_context_source (id integer PRIMARY KEY, tag text);
-- @end

-- @case create_delete_context_action ok
CREATE TABLE delete_context_action (id integer PRIMARY KEY, mapped integer);
-- @end

-- @case seed_delete_context_relations ok
INSERT INTO delete_context_event VALUES (1, 10);
INSERT INTO delete_context_source VALUES (1, 'hit');
INSERT INTO delete_context_action VALUES (1, 100);
-- @end

-- @case create_delete_context_provider ok
CREATE RULE delete_context_provider AS ON DELETE TO delete_context_event DO INSTEAD DELETE FROM delete_context_action WHERE id = OLD.id RETURNING id, mapped;
-- @end

-- @case delete_context_provider_rows rows
DELETE FROM delete_context_event AS event USING delete_context_source AS source WHERE event.id = source.id RETURNING source.tag, old.value, new.value;
-- @end

-- @case delete_context_relations rows
SELECT (SELECT count(*) FROM delete_context_event) AS event_count, (SELECT count(*) FROM delete_context_action) AS action_count;
-- @end

-- @case create_session_replication_rule_fixture ok
CREATE TABLE replication_rule_items (id integer PRIMARY KEY); CREATE TABLE replication_rule_log (seq bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, message text NOT NULL); CREATE RULE rule_origin AS ON INSERT TO replication_rule_items DO ALSO INSERT INTO replication_rule_log(message) VALUES ('rule_origin:' || NEW.id::text); CREATE RULE rule_replica AS ON INSERT TO replication_rule_items DO ALSO INSERT INTO replication_rule_log(message) VALUES ('rule_replica:' || NEW.id::text); CREATE RULE rule_always AS ON INSERT TO replication_rule_items DO ALSO INSERT INTO replication_rule_log(message) VALUES ('rule_always:' || NEW.id::text); CREATE RULE rule_disabled AS ON INSERT TO replication_rule_items DO ALSO INSERT INTO replication_rule_log(message) VALUES ('rule_disabled:' || NEW.id::text); ALTER TABLE replication_rule_items ENABLE REPLICA RULE rule_replica; ALTER TABLE replication_rule_items ENABLE ALWAYS RULE rule_always; ALTER TABLE replication_rule_items DISABLE RULE rule_disabled;
-- @end

-- @case session_replication_origin_rule_execution ok
SET session_replication_role = origin; INSERT INTO replication_rule_items VALUES (1); RESET session_replication_role;
-- @end

-- @case session_replication_local_rule_execution ok
SET session_replication_role = local; INSERT INTO replication_rule_items VALUES (2); RESET session_replication_role;
-- @end

-- @case session_replication_replica_rule_execution ok
SET session_replication_role = replica; INSERT INTO replication_rule_items VALUES (3); RESET session_replication_role;
-- @end

-- @case session_replication_rule_mode_rows rows
SELECT message FROM replication_rule_log ORDER BY seq;
-- @end
