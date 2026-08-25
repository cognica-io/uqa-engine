-- Stateful PostgreSQL 18.4 range, temporal-constraint, and type-rewrite parity fixture.
-- The runner replaces __UQA_STATEFUL_SCHEMA__ and executes each delimited case in order.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- @case canonical_range_literals rows
SELECT '[1,4]'::int4range::text, '(1,4]'::int8range::text, '[2024-01-01,2024-01-02]'::daterange::text, '{[1,3),[3,5),[10,12)}'::int4multirange::text;
-- @end

-- @case range_functions_and_operators rows
SELECT int4range(1, 4, '[]')::text, lower('[1,5)'::int4range), upper('[1,5)'::int4range), lower_inc('[1,5)'::int4range), upper_inc('[1,5)'::int4range), isempty('empty'::int4range), ('[1,5)'::int4range && '[4,8)'::int4range), ('[1,5)'::int4range @> '[2,3)'::int4range), ('[1,5)'::int4range -|- '[5,8)'::int4range), multirange('[1,5)'::int4range)::text, range_merge('{[1,3),[8,10)}'::int4multirange)::text;
-- @end

-- @case create_range_polymorphic_routines ok
CREATE FUNCTION __UQA_STATEFUL_SCHEMA__.range_lower(value anyrange) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT lower($1)'; CREATE FUNCTION __UQA_STATEFUL_SCHEMA__.range_to_multi(value anyrange) RETURNS anymultirange LANGUAGE SQL IMMUTABLE AS 'SELECT multirange($1)'; CREATE FUNCTION __UQA_STATEFUL_SCHEMA__.multi_to_range(value anymultirange) RETURNS anyrange LANGUAGE SQL IMMUTABLE AS 'SELECT range_merge($1)'; CREATE FUNCTION __UQA_STATEFUL_SCHEMA__.range_scalar_pair(value anyrange, item anyelement) RETURNS anyelement LANGUAGE SQL IMMUTABLE AS 'SELECT $2'; CREATE FUNCTION __UQA_STATEFUL_SCHEMA__.compatible_range_pair(value anycompatiblerange, item anycompatible) RETURNS anycompatible LANGUAGE SQL IMMUTABLE AS 'SELECT $2';
-- @end

-- @case range_polymorphic_routine_results rows
SELECT __UQA_STATEFUL_SCHEMA__.range_lower('[1,5)'::int4range), pg_typeof(__UQA_STATEFUL_SCHEMA__.range_lower('[1,5)'::int4range))::text, __UQA_STATEFUL_SCHEMA__.range_to_multi('[1,5)'::int4range)::text, pg_typeof(__UQA_STATEFUL_SCHEMA__.range_to_multi('[1,5)'::int4range))::text, __UQA_STATEFUL_SCHEMA__.multi_to_range('{[1,5)}'::int4multirange)::text, pg_typeof(__UQA_STATEFUL_SCHEMA__.multi_to_range('{[1,5)}'::int4multirange))::text, __UQA_STATEFUL_SCHEMA__.range_scalar_pair('[1,5)'::int4range, 2::integer), pg_typeof(__UQA_STATEFUL_SCHEMA__.compatible_range_pair('[1,5)'::int8range, 2::integer))::text;
-- @end

-- @case simple_range_polymorphic_mismatch error
SELECT __UQA_STATEFUL_SCHEMA__.range_scalar_pair('[1,5)'::int4range, 2::bigint);
-- @end

-- @case compatible_range_polymorphic_mismatch error
SELECT __UQA_STATEFUL_SCHEMA__.compatible_range_pair('[1,5)'::int4range, 2::bigint);
-- @end

-- @case indeterminate_range_polymorphic_call error
SELECT __UQA_STATEFUL_SCHEMA__.range_lower(NULL);
-- @end

-- @case create_temporal_parent ok
CREATE TABLE temporal_parent (tenant integer, valid_at daterange, CONSTRAINT temporal_parent_pk PRIMARY KEY (tenant, valid_at WITHOUT OVERLAPS));
-- @end

-- @case create_temporal_child ok
CREATE TABLE temporal_child (row_id integer PRIMARY KEY, tenant integer, valid_at daterange, CONSTRAINT temporal_child_fk FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES temporal_parent (tenant, PERIOD valid_at));
-- @end

-- @case insert_adjacent_parent_ranges ok
INSERT INTO temporal_parent VALUES (7, '[2024-01-01,2024-01-10)'), (7, '[2024-01-10,2024-01-20)');
-- @end

-- @case insert_aggregate_covered_child ok
INSERT INTO temporal_child VALUES (1, 7, '[2024-01-05,2024-01-15)');
-- @end

-- @case overlapping_parent_rejected error
INSERT INTO temporal_parent VALUES (7, '[2024-01-05,2024-01-12)');
-- @end

-- @case empty_temporal_key_rejected error
INSERT INTO temporal_parent VALUES (8, 'empty');
-- @end

-- @case uncovered_child_rejected error
INSERT INTO temporal_child VALUES (2, 7, '[2024-01-05,2024-01-25)');
-- @end

-- @case parent_delete_breaking_coverage_rejected error
DELETE FROM temporal_parent WHERE tenant = 7 AND valid_at = '[2024-01-01,2024-01-10)'::daterange;
-- @end

-- @case parent_update_breaking_coverage_rejected error
UPDATE temporal_parent SET valid_at = '[2024-01-12,2024-01-20)' WHERE tenant = 7 AND valid_at = '[2024-01-10,2024-01-20)'::daterange;
-- @end

-- @case temporal_constraint_catalog rows
SELECT conname, contype, conperiod FROM pg_constraint WHERE conname IN ('temporal_child_fk', 'temporal_parent_pk') ORDER BY conname;
-- @end

-- @case create_alter_parent ok
CREATE TABLE alter_parent (tenant integer, valid_at int4range);
-- @end

-- @case insert_alter_parent_rows ok
INSERT INTO alter_parent VALUES (3, '[1,5)'), (3, '[5,10)');
-- @end

-- @case add_without_overlaps ok
ALTER TABLE alter_parent ADD CONSTRAINT alter_parent_uq UNIQUE (tenant, valid_at WITHOUT OVERLAPS);
-- @end

-- @case alter_range_to_multirange ok
ALTER TABLE alter_parent ALTER COLUMN valid_at TYPE int4multirange USING multirange(valid_at);
-- @end

-- @case altered_range_identity rows
SELECT valid_at::text, pg_typeof(valid_at)::text FROM alter_parent ORDER BY valid_at::text;
-- @end

-- @case create_existing_child ok
CREATE TABLE alter_child (row_id integer PRIMARY KEY, tenant integer, valid_at int4multirange);
-- @end

-- @case insert_existing_child ok
INSERT INTO alter_child VALUES (1, 3, '{[2,8)}');
-- @end

-- @case add_period_foreign_key ok
ALTER TABLE alter_child ADD CONSTRAINT alter_child_fk FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES alter_parent (tenant, PERIOD valid_at);
-- @end

-- @case added_constraint_survives_reopen rows
SELECT conname, conperiod FROM pg_constraint WHERE conname IN ('alter_parent_uq', 'alter_child_fk') ORDER BY conname;
-- @end

-- @case incompatible_parent_type_rewrite_rejected error
ALTER TABLE alter_parent ALTER COLUMN valid_at TYPE int4range USING range_merge(valid_at);
-- @end

-- @case create_overlapping_existing_rows ok
CREATE TABLE overlap_existing (tenant integer, valid_at daterange);
-- @end

-- @case insert_overlapping_existing_rows ok
INSERT INTO overlap_existing VALUES (1, '[2024-01-01,2024-01-10)'), (1, '[2024-01-05,2024-01-12)');
-- @end

-- @case add_without_overlaps_to_overlapping_rows_rejected error
ALTER TABLE overlap_existing ADD CONSTRAINT overlap_existing_uq UNIQUE (tenant, valid_at WITHOUT OVERLAPS);
-- @end

-- @case failed_without_overlaps_add_is_atomic rows
SELECT conname FROM pg_constraint WHERE conname = 'overlap_existing_uq';
-- @end

-- @case overlapping_insert_still_allowed_after_failed_add ok
INSERT INTO overlap_existing VALUES (1, '[2024-01-06,2024-01-07)');
-- @end

-- @case create_coverage_parent ok
CREATE TABLE coverage_parent (tenant integer, valid_at daterange, UNIQUE (tenant, valid_at WITHOUT OVERLAPS));
-- @end

-- @case insert_coverage_parent ok
INSERT INTO coverage_parent VALUES (1, '[2024-01-01,2024-01-10)');
-- @end

-- @case create_uncovered_existing_child ok
CREATE TABLE uncovered_existing (id integer PRIMARY KEY, tenant integer, valid_at daterange);
-- @end

-- @case insert_uncovered_existing_child ok
INSERT INTO uncovered_existing VALUES (1, 1, '[2024-01-05,2024-01-15)');
-- @end

-- @case add_period_to_uncovered_rows_rejected error
ALTER TABLE uncovered_existing ADD CONSTRAINT uncovered_existing_fk FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES coverage_parent (tenant, PERIOD valid_at);
-- @end

-- @case failed_period_add_is_atomic rows
SELECT conname FROM pg_constraint WHERE conname = 'uncovered_existing_fk';
-- @end

-- @case uncovered_insert_still_allowed_after_failed_add ok
INSERT INTO uncovered_existing VALUES (2, 1, '[2024-02-01,2024-02-02)');
-- @end

-- @case create_ordinary_range_target ok
CREATE TABLE ordinary_range_target (tenant integer, valid_at daterange, UNIQUE (tenant, valid_at));
-- @end

-- @case period_requires_temporal_target_key error
CREATE TABLE missing_temporal_target (id integer PRIMARY KEY, tenant integer, valid_at daterange, FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES ordinary_range_target (tenant, PERIOD valid_at));
-- @end

-- @case period_requires_matching_range_type error
CREATE TABLE mismatched_period_type (id integer PRIMARY KEY, tenant integer, valid_at int4range, FOREIGN KEY (tenant, PERIOD valid_at) REFERENCES coverage_parent (tenant, PERIOD valid_at));
-- @end

-- @case failed_period_create_is_atomic rows
SELECT table_name FROM information_schema.tables WHERE table_name IN ('missing_temporal_target', 'mismatched_period_type') ORDER BY table_name;
-- @end

-- @case create_durable_range_generated ok
CREATE TABLE durable_range_generated (id integer PRIMARY KEY, source int4range, lower_value integer GENERATED ALWAYS AS (lower(source)) STORED, wrapped int4multirange GENERATED ALWAYS AS (multirange(source)) STORED);
-- @end

-- @case insert_durable_range_generated ok
INSERT INTO durable_range_generated (id, source) VALUES (1, '[1,5)');
-- @end

-- @case durable_range_generated_binding_after_reopen rows
SELECT lower_value, wrapped::text, pg_typeof(wrapped)::text FROM durable_range_generated ORDER BY id;
-- @end

-- @case pg_range_identity rows
SELECT rngtypid, rngsubtype, rngmultitypid, rngcollation, rngsubopc, rngcanonical::oid, rngsubdiff::oid FROM pg_range ORDER BY rngtypid;
-- @end

-- @case pg_proc_range_identity rows
SELECT oid, proname, provariadic, proisstrict, provolatile, proparallel, prorettype, proargtypes::text, prosrc FROM pg_proc WHERE oid IN (3840, 3841, 3848, 3850, 4057, 4228, 4235, 4237, 4280, 4281, 4282, 4298) ORDER BY oid;
-- @end
