-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Captured with PostgreSQL 18.4 (postgres:18.4-alpine).
\set ON_ERROR_STOP on
BEGIN;
CREATE TEMP TABLE backup_parent(a integer, b integer, UNIQUE(a,b));
SAVEPOINT local_repeat;
CREATE TEMP TABLE backup_child(x integer, FOREIGN KEY(x,x) REFERENCES backup_parent(a,b));
\echo local_repeat :SQLSTATE
ROLLBACK TO local_repeat;
SAVEPOINT target_repeat;
\set ON_ERROR_STOP off
CREATE TEMP TABLE backup_child(x integer, FOREIGN KEY(x,x) REFERENCES backup_parent(a,a));
\echo target_repeat :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO target_repeat;
SAVEPOINT action_repeat;
CREATE TEMP TABLE backup_child(x integer, y integer, FOREIGN KEY(x,y) REFERENCES backup_parent(a,b) ON DELETE SET NULL(x,x));
\echo action_repeat :SQLSTATE
ROLLBACK TO action_repeat;
CREATE DOMAIN pg_temp.backup_domain AS integer NOT NULL CHECK(VALUE > 0);
CREATE TEMP TABLE backup_partitioned(x integer, y integer, FOREIGN KEY(x,y) REFERENCES backup_parent(a,b)) PARTITION BY RANGE(x);
CREATE TEMP TABLE backup_partition PARTITION OF backup_partitioned FOR VALUES FROM (0) TO (100);
SELECT 'constraint_identity_rows', count(*) = count(DISTINCT oid), count(*) FILTER (WHERE conrelid <> 0) > 0, count(*) FILTER (WHERE contypid <> 0) > 0 FROM pg_constraint WHERE connamespace = pg_my_temp_schema();
SELECT 'partition_constraint_rows', parent.oid <> child.oid, child.conparentid = parent.oid FROM pg_constraint parent JOIN pg_constraint child ON child.conparentid = parent.oid WHERE parent.conrelid = 'backup_partitioned'::regclass AND child.conrelid = 'backup_partition'::regclass AND parent.contype = 'f';
CREATE TEMP TABLE backup_targets(id integer PRIMARY KEY) PARTITION BY RANGE(id);
CREATE TEMP TABLE backup_target_branch PARTITION OF backup_targets FOR VALUES FROM (0) TO (100) PARTITION BY RANGE(id);
CREATE TEMP TABLE backup_target_leaf PARTITION OF backup_target_branch FOR VALUES FROM (0) TO (100);
CREATE TEMP TABLE backup_refs(id integer REFERENCES backup_targets(id)) PARTITION BY RANGE(id);
CREATE TEMP TABLE backup_ref_partition PARTITION OF backup_refs FOR VALUES FROM (0) TO (100);
SELECT 'declaring_foreign_key_rows', count(*), count(DISTINCT confrelid) FROM pg_constraint WHERE conrelid = 'backup_refs'::regclass AND contype = 'f';
SELECT 'inherited_foreign_key_rows', count(*), bool_and(confrelid = 'backup_targets'::regclass AND conparentid <> 0) FROM pg_constraint WHERE conrelid = 'backup_ref_partition'::regclass AND contype = 'f';
CREATE TEMP TABLE backup_action_parent(x integer, y integer, CONSTRAINT root_action FOREIGN KEY(x,y) REFERENCES backup_parent(a,b) ON DELETE SET NULL(x)) PARTITION BY RANGE(x);
CREATE TEMP TABLE backup_action_child(x integer, y integer, CONSTRAINT attached_action FOREIGN KEY(x,y) REFERENCES backup_parent(a,b) ON DELETE SET NULL(y));
ALTER TABLE backup_action_parent ATTACH PARTITION backup_action_child FOR VALUES FROM(0) TO(100);
SELECT 'attached_family_definition', child.conparentid = parent.oid, child.conname <> parent.conname, child.confdelsetcols <> parent.confdelsetcols, child.confrelid = parent.confrelid AND child.conkey = parent.conkey AND child.confkey = parent.confkey AND child.condeferrable = parent.condeferrable AND child.condeferred = parent.condeferred AND child.confupdtype = parent.confupdtype AND child.confdeltype = parent.confdeltype AND child.confmatchtype = parent.confmatchtype AND child.conenforced = parent.conenforced FROM pg_constraint parent JOIN pg_constraint child ON child.conparentid = parent.oid WHERE parent.conrelid = 'backup_action_parent'::regclass AND child.conrelid = 'backup_action_child'::regclass AND parent.contype = 'f';
ROLLBACK;
