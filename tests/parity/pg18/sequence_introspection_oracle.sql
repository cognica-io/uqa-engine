\set ON_ERROR_STOP on
DROP SCHEMA IF EXISTS uqa_sequence_introspection_oracle CASCADE;
DROP ROLE IF EXISTS uqa_sequence_introspection_none;
DROP ROLE IF EXISTS uqa_sequence_introspection_usage;
DROP ROLE IF EXISTS uqa_sequence_introspection_update;
DROP ROLE IF EXISTS uqa_sequence_introspection_select;
CREATE ROLE uqa_sequence_introspection_none;
CREATE ROLE uqa_sequence_introspection_usage;
CREATE ROLE uqa_sequence_introspection_update;
CREATE ROLE uqa_sequence_introspection_select;
CREATE SCHEMA uqa_sequence_introspection_oracle;
GRANT USAGE ON SCHEMA uqa_sequence_introspection_oracle TO uqa_sequence_introspection_none, uqa_sequence_introspection_usage, uqa_sequence_introspection_update, uqa_sequence_introspection_select;
SET search_path = uqa_sequence_introspection_oracle, pg_catalog;

CREATE SEQUENCE physical_ids;
SELECT 'relation|' || relkind::text || '|' || relnatts::text || '|' || reltype::text FROM pg_class WHERE oid = 'physical_ids'::regclass;
SELECT 'attribute|' || attnum::text || '|' || attname::text || '|' || atttypid::text || '|' || attnotnull::text FROM pg_attribute WHERE attrelid = 'physical_ids'::regclass AND attnum > 0 ORDER BY attnum;
SELECT 'physical-new|' || last_value || '|' || log_cnt || '|' || is_called FROM physical_ids;
SELECT 'next|' || nextval('physical_ids');
SELECT 'physical-called|' || last_value || '|' || (log_cnt IN (31, 32)) || '|' || is_called FROM physical_ids;

CREATE SEQUENCE inspect_ids AS smallint INCREMENT 2 MINVALUE 3 MAXVALUE 9 START 5 CACHE 3 CYCLE;
SELECT 'parameters|' || start_value || '|' || minimum_value || '|' || maximum_value || '|' || increment || '|' || cycle_option || '|' || cache_size || '|' || data_type FROM pg_sequence_parameters('inspect_ids'::regclass);
SELECT 'data-new|' || last_value || '|' || is_called FROM pg_get_sequence_data('inspect_ids');
SELECT 'last-new|' || coalesce(pg_sequence_last_value('inspect_ids')::text, 'NULL');
SELECT 'next-bounded|' || nextval('inspect_ids');
SELECT 'data-called|' || last_value || '|' || is_called FROM pg_get_sequence_data('inspect_ids');
SELECT 'last-called|' || pg_sequence_last_value('inspect_ids');
SELECT setval('inspect_ids', 7, false);
SELECT 'data-setval-false|' || last_value || '|' || is_called FROM pg_get_sequence_data('inspect_ids');
SELECT 'next-after-setval|' || nextval('inspect_ids');

GRANT USAGE ON SEQUENCE inspect_ids TO uqa_sequence_introspection_usage;
GRANT UPDATE ON SEQUENCE inspect_ids TO uqa_sequence_introspection_update;
GRANT SELECT ON SEQUENCE inspect_ids TO uqa_sequence_introspection_select;

SET ROLE uqa_sequence_introspection_none;
SELECT 'none-data|' || coalesce(last_value::text, 'NULL') || '|' || coalesce(is_called::text, 'NULL') FROM pg_get_sequence_data('inspect_ids');
SELECT 'none-last|' || coalesce(pg_sequence_last_value('inspect_ids')::text, 'NULL');
SELECT 'none-view|' || coalesce(last_value::text, 'NULL') FROM pg_sequences WHERE schemaname = 'uqa_sequence_introspection_oracle' AND sequencename = 'inspect_ids';
RESET ROLE;

SET ROLE uqa_sequence_introspection_usage;
SELECT 'usage-parameters|' || start_value || '|' || cache_size FROM pg_sequence_parameters('inspect_ids'::regclass);
SELECT 'usage-data|' || coalesce(last_value::text, 'NULL') || '|' || coalesce(is_called::text, 'NULL') FROM pg_get_sequence_data('inspect_ids');
SELECT 'usage-last|' || coalesce(pg_sequence_last_value('inspect_ids')::text, 'NULL');
SELECT 'usage-view|' || coalesce(last_value::text, 'NULL') FROM pg_sequences WHERE schemaname = 'uqa_sequence_introspection_oracle' AND sequencename = 'inspect_ids';
RESET ROLE;

SET ROLE uqa_sequence_introspection_update;
SELECT 'update-parameters|' || start_value || '|' || cache_size FROM pg_sequence_parameters('inspect_ids'::regclass);
SELECT 'update-data|' || coalesce(last_value::text, 'NULL') || '|' || coalesce(is_called::text, 'NULL') FROM pg_get_sequence_data('inspect_ids');
SELECT 'update-last|' || coalesce(pg_sequence_last_value('inspect_ids')::text, 'NULL');
SELECT 'update-view|' || coalesce(last_value::text, 'NULL') FROM pg_sequences WHERE schemaname = 'uqa_sequence_introspection_oracle' AND sequencename = 'inspect_ids';
RESET ROLE;

SET ROLE uqa_sequence_introspection_select;
SELECT 'select-direct|' || last_value || '|' || log_cnt || '|' || is_called FROM inspect_ids;
SELECT 'select-parameters|' || start_value || '|' || cache_size FROM pg_sequence_parameters('inspect_ids'::regclass);
SELECT 'select-data|' || last_value || '|' || is_called FROM pg_get_sequence_data('inspect_ids');
SELECT 'select-last|' || coalesce(pg_sequence_last_value('inspect_ids')::text, 'NULL');
SELECT 'select-view|' || coalesce(last_value::text, 'NULL') FROM pg_sequences WHERE schemaname = 'uqa_sequence_introspection_oracle' AND sequencename = 'inspect_ids';
RESET ROLE;

SELECT 'proc|' || oid::text || '|' || proname || '|' || prorettype::text || '|' || proargtypes::text || '|' || coalesce(proallargtypes::text, 'NULL') || '|' || coalesce(proargmodes::text, 'NULL') || '|' || coalesce(proargnames::text, 'NULL') || '|' || proisstrict::text || '|' || provolatile::text || '|' || proparallel::text FROM pg_proc WHERE oid IN (3078, 4032, 6427) ORDER BY oid;

DROP SCHEMA uqa_sequence_introspection_oracle CASCADE;
DROP ROLE uqa_sequence_introspection_none;
DROP ROLE uqa_sequence_introspection_usage;
DROP ROLE uqa_sequence_introspection_update;
DROP ROLE uqa_sequence_introspection_select;
