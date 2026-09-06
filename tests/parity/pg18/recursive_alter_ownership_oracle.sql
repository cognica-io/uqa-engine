\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_recursive_alter_ownership CASCADE;
DROP ROLE IF EXISTS uqa_recursive_parent_owner;
DROP ROLE IF EXISTS uqa_recursive_child_owner;
CREATE ROLE uqa_recursive_parent_owner;
CREATE ROLE uqa_recursive_child_owner;
CREATE SCHEMA uqa_recursive_alter_ownership;
GRANT USAGE, CREATE ON SCHEMA uqa_recursive_alter_ownership TO uqa_recursive_parent_owner, uqa_recursive_child_owner;

CREATE FUNCTION pg_temp.recursive_owner_probe(label text, role_name text, command text)
RETURNS text LANGUAGE plpgsql AS $oracle$
DECLARE
    state text;
    message text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    EXECUTE command;
    RESET ROLE;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
    RESET ROLE;
    RETURN label || '|' || state || '|' || message;
END
$oracle$;

CREATE TABLE uqa_recursive_alter_ownership.parent(id integer);
CREATE TABLE uqa_recursive_alter_ownership.child(extra integer, mismatched text, later integer, CONSTRAINT positive CHECK (id > 0)) INHERITS (uqa_recursive_alter_ownership.parent);
ALTER TABLE uqa_recursive_alter_ownership.child ALTER COLUMN id SET NOT NULL;
INSERT INTO uqa_recursive_alter_ownership.child(id) VALUES (1);
ALTER TABLE uqa_recursive_alter_ownership.parent OWNER TO uqa_recursive_parent_owner;
ALTER TABLE uqa_recursive_alter_ownership.child OWNER TO uqa_recursive_child_owner;

SELECT pg_temp.recursive_owner_probe('new-column', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD COLUMN fresh integer');
SELECT pg_temp.recursive_owner_probe('merged-column', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD COLUMN extra integer');
SELECT pg_temp.recursive_owner_probe('owner-before-merged-type', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD COLUMN mismatched integer');
SELECT pg_temp.recursive_owner_probe('merged-check', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD CONSTRAINT positive CHECK (id > 0)');
SELECT pg_temp.recursive_owner_probe('owner-before-check-conflict', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD CONSTRAINT positive CHECK (id < 0) NOT VALID');
SELECT pg_temp.recursive_owner_probe('merged-named-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD CONSTRAINT required NOT NULL id');
SELECT pg_temp.recursive_owner_probe('merged-set-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ALTER COLUMN id SET NOT NULL');
SELECT pg_temp.recursive_owner_probe('only-column', 'uqa_recursive_parent_owner', 'ALTER TABLE ONLY uqa_recursive_alter_ownership.parent ADD COLUMN only_added integer');
GRANT ALL ON uqa_recursive_alter_ownership.child TO uqa_recursive_parent_owner;
SELECT pg_temp.recursive_owner_probe('all-grants-are-not-ownership', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD COLUMN extra integer');
GRANT uqa_recursive_child_owner TO uqa_recursive_parent_owner WITH INHERIT FALSE, SET TRUE;
SELECT pg_temp.recursive_owner_probe('set-without-inherit', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD COLUMN extra integer');
REVOKE uqa_recursive_child_owner FROM uqa_recursive_parent_owner;

SELECT 'parent-after-denials|' || string_agg(attname, ',' ORDER BY attnum) FROM pg_attribute WHERE attrelid = 'uqa_recursive_alter_ownership.parent'::regclass AND attnum > 0 AND NOT attisdropped;
SELECT 'child-extra-after-denials|' || attislocal || '|' || attinhcount FROM pg_attribute WHERE attrelid = 'uqa_recursive_alter_ownership.child'::regclass AND attname = 'extra';
SELECT 'rows-after-denials|' || count(*) FROM uqa_recursive_alter_ownership.parent;

CREATE TABLE uqa_recursive_alter_ownership.grand_parent(id integer);
CREATE TABLE uqa_recursive_alter_ownership.grand_child(id integer NOT NULL, extra integer, CONSTRAINT grand_positive CHECK (id > 0)) INHERITS (uqa_recursive_alter_ownership.grand_parent);
CREATE TABLE uqa_recursive_alter_ownership.grandchild() INHERITS (uqa_recursive_alter_ownership.grand_child);
ALTER TABLE uqa_recursive_alter_ownership.grand_parent OWNER TO uqa_recursive_parent_owner;
ALTER TABLE uqa_recursive_alter_ownership.grand_child OWNER TO uqa_recursive_parent_owner;
ALTER TABLE uqa_recursive_alter_ownership.grandchild OWNER TO uqa_recursive_child_owner;
SELECT pg_temp.recursive_owner_probe('unaffected-grandchild-column', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.grand_parent ADD COLUMN extra integer');
SELECT pg_temp.recursive_owner_probe('unaffected-grandchild-check', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.grand_parent ADD CONSTRAINT grand_positive CHECK (id > 0)');
SELECT pg_temp.recursive_owner_probe('unaffected-grandchild-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.grand_parent ALTER COLUMN id SET NOT NULL');
SELECT 'unchanged-grandchild-extra|' || attislocal || '|' || attinhcount FROM pg_attribute WHERE attrelid = 'uqa_recursive_alter_ownership.grandchild'::regclass AND attname = 'extra';

CREATE TABLE uqa_recursive_alter_ownership.propagation_parent(id integer);
CREATE TABLE uqa_recursive_alter_ownership.propagation_child() INHERITS (uqa_recursive_alter_ownership.propagation_parent);
CREATE TABLE uqa_recursive_alter_ownership.propagation_grandchild(id integer NOT NULL, extra integer) INHERITS (uqa_recursive_alter_ownership.propagation_child);
ALTER TABLE uqa_recursive_alter_ownership.propagation_parent OWNER TO uqa_recursive_parent_owner;
ALTER TABLE uqa_recursive_alter_ownership.propagation_child OWNER TO uqa_recursive_parent_owner;
ALTER TABLE uqa_recursive_alter_ownership.propagation_grandchild OWNER TO uqa_recursive_child_owner;
SELECT pg_temp.recursive_owner_probe('affected-grandchild-column', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.propagation_parent ADD COLUMN extra integer');
SELECT pg_temp.recursive_owner_probe('affected-grandchild-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.propagation_parent ALTER COLUMN id SET NOT NULL');
SELECT 'parent-after-grandchild-denial|' || string_agg(attname, ',' ORDER BY attnum) FROM pg_attribute WHERE attrelid = 'uqa_recursive_alter_ownership.propagation_parent'::regclass AND attnum > 0 AND NOT attisdropped;

CREATE TABLE uqa_recursive_alter_ownership.diamond_parent(id integer);
CREATE TABLE uqa_recursive_alter_ownership.diamond_left(id integer NOT NULL, extra integer) INHERITS (uqa_recursive_alter_ownership.diamond_parent);
CREATE TABLE uqa_recursive_alter_ownership.diamond_right() INHERITS (uqa_recursive_alter_ownership.diamond_parent);
CREATE TABLE uqa_recursive_alter_ownership.diamond_leaf() INHERITS (uqa_recursive_alter_ownership.diamond_left, uqa_recursive_alter_ownership.diamond_right);
ALTER TABLE uqa_recursive_alter_ownership.diamond_parent OWNER TO uqa_recursive_parent_owner;
ALTER TABLE uqa_recursive_alter_ownership.diamond_left OWNER TO uqa_recursive_parent_owner;
ALTER TABLE uqa_recursive_alter_ownership.diamond_right OWNER TO uqa_recursive_parent_owner;
ALTER TABLE uqa_recursive_alter_ownership.diamond_leaf OWNER TO uqa_recursive_child_owner;
SELECT pg_temp.recursive_owner_probe('diamond-column', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.diamond_parent ADD COLUMN extra integer');
SELECT pg_temp.recursive_owner_probe('diamond-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.diamond_parent ALTER COLUMN id SET NOT NULL');

CREATE TABLE uqa_recursive_alter_ownership.partition_parent(id integer) PARTITION BY RANGE(id);
CREATE TABLE uqa_recursive_alter_ownership.partition_child PARTITION OF uqa_recursive_alter_ownership.partition_parent FOR VALUES FROM (0) TO (100);
ALTER TABLE uqa_recursive_alter_ownership.partition_child ADD CONSTRAINT partition_positive CHECK (id > 0);
ALTER TABLE uqa_recursive_alter_ownership.partition_child ALTER COLUMN id SET NOT NULL;
ALTER TABLE uqa_recursive_alter_ownership.partition_parent OWNER TO uqa_recursive_parent_owner;
ALTER TABLE uqa_recursive_alter_ownership.partition_child OWNER TO uqa_recursive_child_owner;
SELECT pg_temp.recursive_owner_probe('partition-check', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.partition_parent ADD CONSTRAINT partition_positive CHECK (id > 0)');
SELECT pg_temp.recursive_owner_probe('partition-named-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.partition_parent ADD CONSTRAINT required NOT NULL id');
SELECT pg_temp.recursive_owner_probe('partition-set-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.partition_parent ALTER COLUMN id SET NOT NULL');

SET ROLE uqa_recursive_parent_owner;
BEGIN;
SAVEPOINT protected_change;
\set ON_ERROR_STOP off
ALTER TABLE uqa_recursive_alter_ownership.parent ADD COLUMN extra integer;
\echo savepoint-error :SQLSTATE
SELECT 1;
\echo failed-transaction :SQLSTATE
\set ON_ERROR_STOP on
ROLLBACK TO protected_change;
COMMIT;
RESET ROLE;
SELECT 'parent-after-savepoint|' || string_agg(attname, ',' ORDER BY attnum) FROM pg_attribute WHERE attrelid = 'uqa_recursive_alter_ownership.parent'::regclass AND attnum > 0 AND NOT attisdropped;

GRANT uqa_recursive_child_owner TO uqa_recursive_parent_owner WITH INHERIT TRUE, SET FALSE;
SELECT pg_temp.recursive_owner_probe('inherited-column', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD COLUMN extra integer');
SELECT pg_temp.recursive_owner_probe('inherited-check', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD CONSTRAINT positive CHECK (id > 0)');
SELECT pg_temp.recursive_owner_probe('inherited-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ALTER COLUMN id SET NOT NULL');
SELECT pg_temp.recursive_owner_probe('inherited-partition-check', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.partition_parent ADD CONSTRAINT partition_positive CHECK (id > 0)');
SELECT pg_temp.recursive_owner_probe('inherited-partition-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.partition_parent ALTER COLUMN id SET NOT NULL');
SELECT pg_temp.recursive_owner_probe('inherited-diamond-column', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.diamond_parent ADD COLUMN extra integer');
SELECT 'diamond-leaf-extra|' || attinhcount FROM pg_attribute WHERE attrelid = 'uqa_recursive_alter_ownership.diamond_leaf'::regclass AND attname = 'extra';
SELECT 'child-extra-after-inherit|' || attislocal || '|' || attinhcount FROM pg_attribute WHERE attrelid = 'uqa_recursive_alter_ownership.child'::regclass AND attname = 'extra';
REVOKE uqa_recursive_child_owner FROM uqa_recursive_parent_owner;
SELECT pg_temp.recursive_owner_probe('revoked-child-owner', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD COLUMN later integer');
SELECT 'parent-after-revoke|' || string_agg(attname, ',' ORDER BY attnum) FROM pg_attribute WHERE attrelid = 'uqa_recursive_alter_ownership.parent'::regclass AND attnum > 0 AND NOT attisdropped;
SELECT pg_temp.recursive_owner_probe('unchanged-root-column', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ADD COLUMN IF NOT EXISTS id integer');
SELECT pg_temp.recursive_owner_probe('unchanged-only-root-column', 'uqa_recursive_parent_owner', 'ALTER TABLE ONLY uqa_recursive_alter_ownership.parent ADD COLUMN IF NOT EXISTS id text');
SELECT pg_temp.recursive_owner_probe('unchanged-root-not-null', 'uqa_recursive_parent_owner', 'ALTER TABLE uqa_recursive_alter_ownership.parent ALTER COLUMN id SET NOT NULL');

DROP SCHEMA uqa_recursive_alter_ownership CASCADE;
DROP ROLE uqa_recursive_parent_owner;
DROP ROLE uqa_recursive_child_owner;
