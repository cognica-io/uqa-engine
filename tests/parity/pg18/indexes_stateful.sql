-- PostgreSQL 18.4 unique indexes, predicates, catalog definitions, and dependencies.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- @case create_items ok
CREATE TABLE items(a int,b text,active boolean);
-- @end

-- @case create_unique ok
CREATE UNIQUE INDEX items_ab ON items(a,b);
-- @end

-- @case unique_duplicate ok
INSERT INTO items VALUES(1,'one',true),(NULL,'one',true),(NULL,'one',false),(1,NULL,true),(1,NULL,false);
-- @end

-- @case duplicate_atomic error
INSERT INTO items VALUES(2,'two',true),(1,'one',false);
-- @end

-- @case after_atomic rows
SELECT a,b,active FROM items ORDER BY a,b,active;
-- @end

-- @case update_duplicate error
UPDATE items SET a=1,b='one' WHERE a IS NULL;
-- @end

-- @case unique_catalog rows
SELECT c.relname,i.indisunique,i.indisprimary,i.indnullsnotdistinct,i.indnatts,i.indnkeyatts,replace(pg_get_indexdef(c.oid),current_schema()||'.','fixture.'),replace(pg_get_indexdef(c.oid,0,true),current_schema()||'.','fixture.'),replace(pg_get_indexdef(c.oid,1,false),current_schema()||'.','fixture.'),replace(pg_get_indexdef(c.oid,9,false),current_schema()||'.','fixture.') FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid WHERE i.indrelid='items'::regclass ORDER BY c.relname;
-- @end

-- @case create_null_items ok
CREATE TABLE null_items(a int,b text);
-- @end

-- @case create_null_distinct ok
CREATE UNIQUE INDEX distinct_key ON null_items(a);
-- @end

-- @case create_null_equal ok
CREATE UNIQUE INDEX equal_key ON null_items(a) NULLS NOT DISTINCT;
-- @end

-- @case insert_null ok
INSERT INTO null_items VALUES(NULL,'first');
-- @end

-- @case duplicate_null error
INSERT INTO null_items VALUES(NULL,'duplicate');
-- @end

-- @case null_arbiter_update ok
INSERT INTO null_items VALUES(NULL,'updated') ON CONFLICT(a) DO UPDATE SET b=excluded.b;
-- @end

-- @case null_arbiter_result rows
SELECT * FROM null_items;
-- @end

-- @case create_duplicates ok
CREATE TABLE duplicates(a int); INSERT INTO duplicates VALUES(1),(1);
-- @end

-- @case failed_build error
CREATE UNIQUE INDEX failed_build ON duplicates(a);
-- @end

-- @case failed_build_catalog rows
SELECT indexname FROM pg_indexes WHERE indexname='failed_build';
-- @end

-- @case nonunique_after_failed_build ok
CREATE INDEX failed_build ON duplicates(a);
-- @end

-- @case if_not_exists_before_validation ok
CREATE UNIQUE INDEX IF NOT EXISTS failed_build ON duplicates(a);
-- @end

-- @case create_partial_items ok
CREATE TABLE partial_items(a int,b int,active boolean);
-- @end

-- @case partial_existing_rows ok
INSERT INTO partial_items VALUES(1,-1,false),(1,-1,NULL),(1,1,true);
-- @end

-- @case create_partial ok
CREATE UNIQUE INDEX positive_key ON partial_items(a) WHERE b>0;
-- @end

-- @case partial_duplicate error
INSERT INTO partial_items VALUES(1,2,true);
-- @end

-- @case partial_outside ok
INSERT INTO partial_items VALUES(1,-2,true);
-- @end

-- @case partial_update_into_index error
UPDATE partial_items SET b=2 WHERE active IS NULL;
-- @end

-- @case partial_missing_inference error
INSERT INTO partial_items VALUES(1,2,true) ON CONFLICT(a) DO NOTHING;
-- @end

-- @case partial_empty_missing_inference error
INSERT INTO partial_items SELECT 1,1,true WHERE false ON CONFLICT(a) DO NOTHING;
-- @end

-- @case partial_weak_inference error
INSERT INTO partial_items VALUES(1,2,true) ON CONFLICT(a) WHERE b>=0 DO NOTHING;
-- @end

-- @case partial_strong_inference ok
INSERT INTO partial_items VALUES(1,3,true) ON CONFLICT(a) WHERE b>1 DO UPDATE SET b=excluded.b;
-- @end

-- @case partial_conjunct_inference ok
INSERT INTO partial_items VALUES(1,3,true) ON CONFLICT(a) WHERE b>0 AND a>0 DO NOTHING;
-- @end

-- @case partial_result rows
SELECT * FROM partial_items ORDER BY a,b,active;
-- @end

-- @case partial_cardinality error
INSERT INTO partial_items VALUES(2,1,true),(2,2,true) ON CONFLICT(a) WHERE b>0 DO UPDATE SET b=excluded.b;
-- @end

-- @case partial_do_nothing ok
INSERT INTO partial_items VALUES(3,-1,true),(3,1,true),(3,2,true) ON CONFLICT DO NOTHING;
-- @end

-- @case partial_catalog rows
SELECT replace(pg_get_indexdef(indexrelid),current_schema()||'.','fixture.'),replace(pg_get_indexdef(indexrelid,0,true),current_schema()||'.','fixture.'),pg_get_expr(indpred,indrelid) FROM pg_index WHERE indexrelid='positive_key'::regclass;
-- @end

-- @case create_covering ok
CREATE UNIQUE INDEX covering ON partial_items(a DESC NULLS LAST,b NULLS FIRST) INCLUDE(active) NULLS NOT DISTINCT WHERE b>0;
-- @end

-- @case covering_catalog rows
SELECT indisunique,indnullsnotdistinct,indnatts,indnkeyatts,replace(pg_get_indexdef(indexrelid),current_schema()||'.','fixture.'),replace(pg_get_indexdef(indexrelid,0,true),current_schema()||'.','fixture.'),replace(pg_get_indexdef(indexrelid,1,false),current_schema()||'.','fixture.'),replace(pg_get_indexdef(indexrelid,3,false),current_schema()||'.','fixture.'),replace(pg_get_indexdef(indexrelid,-1,false),current_schema()||'.','fixture.') FROM pg_index WHERE indexrelid='covering'::regclass;
-- @end

-- @case create_constraint_items ok
CREATE TABLE constraint_items(a int PRIMARY KEY,b int CONSTRAINT b_key UNIQUE,c int);
-- @end

-- @case constraint_catalog rows
SELECT c.relname,i.indisunique,i.indisprimary,replace(pg_get_indexdef(c.oid),current_schema()||'.','fixture.') FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid WHERE i.indrelid='constraint_items'::regclass ORDER BY c.relname;
-- @end

-- @case constraint_index_links rows
SELECT conname,contype,conindid::regclass::text FROM pg_constraint WHERE conrelid='constraint_items'::regclass AND contype IN ('p','u') ORDER BY conname;
-- @end

-- @case constraint_insert ok
INSERT INTO constraint_items VALUES(1,1,0);
-- @end

-- @case constraint_arbiter ok
INSERT INTO constraint_items VALUES(2,1,1) ON CONFLICT ON CONSTRAINT b_key DO UPDATE SET c=excluded.c;
-- @end

-- @case constraint_unrelated_conflict error
INSERT INTO constraint_items VALUES(1,2,2) ON CONFLICT ON CONSTRAINT b_key DO NOTHING;
-- @end

-- @case constraint_missing error
INSERT INTO constraint_items VALUES(1,1,1) ON CONFLICT ON CONSTRAINT missing DO NOTHING;
-- @end

-- @case constraint_index_drop error
DROP INDEX b_key CASCADE;
-- @end

-- @case constraint_index_collision error
CREATE INDEX b_key ON constraint_items(c);
-- @end

-- @case constraint_index_if_not_exists ok
CREATE INDEX IF NOT EXISTS b_key ON constraint_items(c);
-- @end

-- @case fk_parent ok
CREATE TABLE parent(a int); CREATE UNIQUE INDEX parent_key ON parent(a);
-- @end

-- @case fk_child ok
CREATE TABLE child(a int REFERENCES parent(a));
-- @end

-- @case foreign_key_index_link rows
SELECT conname,contype,conindid::regclass::text FROM pg_constraint WHERE conrelid='child'::regclass;
-- @end

-- @case fk_populate ok
INSERT INTO parent VALUES(1); INSERT INTO child VALUES(1);
-- @end

-- @case fk_missing_parent error
INSERT INTO child VALUES(2);
-- @end

-- @case fk_index_restrict error
DROP INDEX parent_key;
-- @end

-- @case fk_index_cascade ok
DROP INDEX parent_key CASCADE;
-- @end

-- @case fk_after_drop ok
INSERT INTO child VALUES(2); INSERT INTO parent VALUES(1);
-- @end

-- @case fk_partial_rejected error
CREATE TABLE invalid_child(a int REFERENCES partial_items(a));
-- @end

-- @case predicate_volatile error
CREATE UNIQUE INDEX invalid_predicate ON items(a) WHERE random()>0;
-- @end

-- @case predicate_subquery error
CREATE UNIQUE INDEX invalid_predicate ON items(a) WHERE (SELECT true);
-- @end

-- @case predicate_boolean error
CREATE UNIQUE INDEX invalid_predicate ON items(a) WHERE a;
-- @end

-- @case predicate_function ok
CREATE FUNCTION positive(int) RETURNS boolean LANGUAGE SQL IMMUTABLE RETURN $1>0;
-- @end

-- @case predicate_function_index ok
CREATE UNIQUE INDEX function_key ON partial_items(b) WHERE positive(b);
-- @end

-- @case predicate_function_restrict error
DROP FUNCTION positive(int);
-- @end

-- @case predicate_function_rename ok
ALTER FUNCTION positive(int) RENAME TO renamed_positive;
-- @end

-- @case predicate_function_duplicate error
INSERT INTO partial_items VALUES(10,1,false);
-- @end

-- @case predicate_function_cascade ok
DROP FUNCTION renamed_positive(int) CASCADE;
-- @end

-- @case predicate_function_after_drop rows
SELECT indexname FROM pg_indexes WHERE indexname='function_key';
-- @end

-- @case rename_partial_column ok
ALTER TABLE partial_items RENAME COLUMN b TO score;
-- @end

-- @case rename_partial_table ok
ALTER TABLE partial_items RENAME TO renamed_items;
-- @end

-- @case renamed_partial_duplicate error
INSERT INTO renamed_items VALUES(1,4,true);
-- @end

-- @case renamed_partial_catalog rows
SELECT replace(pg_get_indexdef('positive_key'::regclass),current_schema()||'.','fixture.'),replace(pg_get_indexdef('covering'::regclass),current_schema()||'.','fixture.');
-- @end

-- @case definition_missing_oid rows
SELECT replace(pg_get_indexdef(0),current_schema()||'.','fixture.'),replace(pg_get_indexdef(0,1,false),current_schema()||'.','fixture.');
-- @end

-- @case definition_nulls rows
SELECT replace(pg_get_indexdef(NULL::oid),current_schema()||'.','fixture.'),replace(pg_get_indexdef(NULL::oid,1,false),current_schema()||'.','fixture.'),replace(pg_get_indexdef(1::oid,NULL,false),current_schema()||'.','fixture.'),replace(pg_get_indexdef(1::oid,0,NULL),current_schema()||'.','fixture.');
-- @end

-- @case definition_wrong_arity error
SELECT replace(pg_get_indexdef(0::oid,true),current_schema()||'.','fixture.');
-- @end

-- @case definition_routine_metadata rows
SELECT oid,proname,proargtypes::text,prorettype,proisstrict,provolatile,proparallel,prosrc FROM pg_proc WHERE proname='pg_get_indexdef' ORDER BY oid;
-- @end

-- @case drop_partial_indexes ok
DROP INDEX positive_key,covering;
-- @end

-- @case after_drop_duplicate ok
INSERT INTO renamed_items VALUES(1,4,true),(1,4,true);
-- @end

-- @case final_state rows
SELECT * FROM renamed_items ORDER BY a,score,active;
-- @end

-- @case literal_predicate_index ok
CREATE UNIQUE INDEX lower_key ON items(a) WHERE lower(b)='one';
-- @end

-- @case literal_predicate_definition rows
SELECT replace(pg_get_indexdef('lower_key'::regclass),current_schema()||'.','fixture.'),replace(pg_get_indexdef('lower_key'::regclass,0,true),current_schema()||'.','fixture.');
-- @end

-- @case constraint_collision_source ok
CREATE TABLE source(a int); CREATE INDEX candidate_pkey ON source(a);
-- @end

-- @case constraint_collision_allocation ok
CREATE TABLE candidate(a int PRIMARY KEY);
-- @end

-- @case constraint_collision_name rows
SELECT replace(pg_get_indexdef('candidate_pkey1'::regclass),current_schema()||'.','fixture.');
-- @end

-- @case constraint_collision_explicit error
CREATE TABLE conflicting(a int CONSTRAINT candidate_pkey1 UNIQUE);
-- @end

-- @case constraint_collision_alter error
ALTER TABLE source ADD CONSTRAINT candidate_pkey1 UNIQUE(a);
-- @end

-- @case partition_constraint_parent ok
CREATE TABLE partitioned(a int PRIMARY KEY) PARTITION BY RANGE(a);
-- @end

-- @case partition_constraint_child ok
CREATE TABLE low PARTITION OF partitioned FOR VALUES FROM (0) TO (10);
-- @end

-- @case partition_constraint_indexes rows
SELECT c.relname,c.relispartition,i.indisprimary,replace(pg_get_indexdef(c.oid),current_schema()||'.','fixture.') FROM pg_class c JOIN pg_index i ON i.indexrelid=c.oid WHERE i.indrelid IN ('partitioned'::regclass,'low'::regclass) ORDER BY c.relname;
-- @end

-- @case inference_scope_table ok
CREATE TABLE inference_base(a int CONSTRAINT positive CHECK(a>0),b int,c text);
-- @end

-- @case inference_scope_index ok
CREATE UNIQUE INDEX inference_active ON inference_base(a) WHERE b>0;
-- @end

-- @case inference_scope_view ok
CREATE VIEW inference_view AS SELECT a AS key,b AS amount,c AS label,b>0 AS active FROM inference_base;
-- @end

-- @case inference_view_insert ok
INSERT INTO inference_view(key,amount,label) VALUES(1,2,'first') ON CONFLICT(key) WHERE active DO NOTHING;
-- @end

-- @case inference_view_alias_update ok
INSERT INTO inference_view AS v(key,amount,label) VALUES(1,3,'updated') ON CONFLICT(key) WHERE v.active DO UPDATE SET label=excluded.label;
-- @end

-- @case inference_alias_volatile ok
INSERT INTO inference_base AS x VALUES(1,4,'unused') ON CONFLICT(a) WHERE x.b>0 AND random()>0 DO NOTHING;
-- @end

-- @case inference_scope_rows rows
SELECT * FROM inference_base ORDER BY a;
-- @end

-- @case inference_missing_view_column error
INSERT INTO inference_view(key,amount,label) SELECT 1,2,'x' WHERE false ON CONFLICT(key) WHERE missing DO NOTHING;
-- @end

-- @case inference_non_index_constraint error
INSERT INTO inference_base SELECT 1,1,'x' WHERE false ON CONFLICT ON CONSTRAINT positive DO NOTHING;
-- @end

-- @case inference_missing_constraint error
INSERT INTO inference_base SELECT 1,1,'x' WHERE false ON CONFLICT ON CONSTRAINT missing DO NOTHING;
-- @end

-- @case inference_missing_column error
INSERT INTO inference_base SELECT 1,1,'x' WHERE false ON CONFLICT(missing) DO NOTHING;
-- @end

-- @case inference_non_boolean error
INSERT INTO inference_base SELECT 1,1,'x' WHERE false ON CONFLICT(a) WHERE a DO NOTHING;
-- @end

-- @case inference_subquery error
INSERT INTO inference_base SELECT 1,1,'x' WHERE false ON CONFLICT(a) WHERE (SELECT true) DO NOTHING;
-- @end

-- @case inference_full_index ok
CREATE UNIQUE INDEX inference_full ON inference_base(c);
-- @end

-- @case inference_non_boolean_full_index ok
INSERT INTO inference_base SELECT 1,1,'x' WHERE false ON CONFLICT(c) WHERE a DO NOTHING;
-- @end
