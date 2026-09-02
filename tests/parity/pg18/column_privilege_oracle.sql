\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_column_privilege_oracle CASCADE;
DROP ROLE IF EXISTS uqa_column_acl_reader;
DROP ROLE IF EXISTS uqa_column_acl_delegate;
DROP ROLE IF EXISTS uqa_column_acl_outsider;
DROP ROLE IF EXISTS uqa_column_acl_tablewide;
DROP ROLE IF EXISTS uqa_column_acl_owner;
DROP ROLE IF EXISTS uqa_column_acl_new_owner;

CREATE ROLE uqa_column_acl_owner;
CREATE ROLE uqa_column_acl_new_owner;
CREATE ROLE uqa_column_acl_delegate;
CREATE ROLE uqa_column_acl_reader;
CREATE ROLE uqa_column_acl_outsider;
CREATE ROLE uqa_column_acl_tablewide;
CREATE SCHEMA uqa_column_privilege_oracle AUTHORIZATION uqa_column_acl_owner;
GRANT USAGE ON SCHEMA uqa_column_privilege_oracle TO uqa_column_acl_new_owner, uqa_column_acl_delegate, uqa_column_acl_reader, uqa_column_acl_outsider, uqa_column_acl_tablewide;
GRANT CREATE ON SCHEMA uqa_column_privilege_oracle TO uqa_column_acl_new_owner, uqa_column_acl_reader;

CREATE OR REPLACE FUNCTION pg_temp.column_privilege_probe(label text, command text)
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

SET ROLE uqa_column_acl_owner;
CREATE TABLE uqa_column_privilege_oracle.items(a integer, b integer DEFAULT 2, c integer DEFAULT 3);
CREATE TABLE uqa_column_privilege_oracle.parents(id integer PRIMARY KEY, other integer UNIQUE);
CREATE SEQUENCE uqa_column_privilege_oracle.ids;
INSERT INTO uqa_column_privilege_oracle.items VALUES (1, 2, 3);
SELECT 'defaults|' || (c.relacl IS NULL) || '|' || bool_and(a.attacl IS NULL) FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_attribute AS a ON a.attrelid = c.oid AND a.attnum > 0 WHERE c.oid = 'uqa_column_privilege_oracle.items'::regclass GROUP BY c.relacl;
REVOKE SELECT(c) ON uqa_column_privilege_oracle.items FROM uqa_column_acl_outsider;
SELECT 'noop-revoke|' || (attacl IS NULL) FROM pg_catalog.pg_attribute WHERE attrelid = 'uqa_column_privilege_oracle.items'::regclass AND attname = 'c';
GRANT SELECT(a), INSERT(b), UPDATE(c), REFERENCES(a) ON uqa_column_privilege_oracle.items TO uqa_column_acl_reader;
GRANT SELECT(a) ON uqa_column_privilege_oracle.items TO uqa_column_acl_delegate WITH GRANT OPTION;
GRANT SELECT ON uqa_column_privilege_oracle.items TO uqa_column_acl_tablewide;
GRANT REFERENCES(id) ON uqa_column_privilege_oracle.parents TO uqa_column_acl_reader;
RESET ROLE;

SET ROLE uqa_column_acl_delegate;
GRANT SELECT(a) ON uqa_column_privilege_oracle.items TO uqa_column_acl_outsider;
RESET ROLE;
SELECT 'catalog|' || c.relacl::text || '|' || string_agg(a.attname || '=' || coalesce(a.attacl::text, 'NULL'), ',' ORDER BY a.attnum) FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_attribute AS a ON a.attrelid = c.oid AND a.attnum > 0 WHERE c.oid = 'uqa_column_privilege_oracle.items'::regclass GROUP BY c.relacl::text;
SELECT 'implication|' || has_table_privilege('uqa_column_acl_reader', 'uqa_column_privilege_oracle.items', 'SELECT') || '|' || has_column_privilege('uqa_column_acl_reader', 'uqa_column_privilege_oracle.items', 'a', 'SELECT') || '|' || has_column_privilege('uqa_column_acl_reader', 'uqa_column_privilege_oracle.items', 'b', 'SELECT') || '|' || has_column_privilege('uqa_column_acl_tablewide', 'uqa_column_privilege_oracle.items', 'b', 'SELECT');
SELECT 'system|' || has_column_privilege('uqa_column_acl_reader', 'uqa_column_privilege_oracle.items', 'tableoid', 'SELECT') || '|' || has_column_privilege('uqa_column_acl_tablewide', 'uqa_column_privilege_oracle.items', 'tableoid', 'SELECT');
SELECT 'attnums|' || string_agg(n || '=' || coalesce(has_column_privilege('uqa_column_acl_tablewide', 'uqa_column_privilege_oracle.items', n::smallint, 'SELECT')::text, 'NULL'), ',' ORDER BY n) FROM generate_series(-7, 4) AS n;

SET ROLE uqa_column_acl_reader;
SELECT pg_temp.column_privilege_probe('select-a', 'SELECT a FROM uqa_column_privilege_oracle.items');
SELECT pg_temp.column_privilege_probe('select-b-denied', 'SELECT b FROM uqa_column_privilege_oracle.items');
SELECT pg_temp.column_privilege_probe('select-constant', 'SELECT 1 FROM uqa_column_privilege_oracle.items');
SELECT pg_temp.column_privilege_probe('select-star-denied', 'SELECT * FROM uqa_column_privilege_oracle.items');
SELECT pg_temp.column_privilege_probe('select-tableoid-denied', 'SELECT tableoid FROM uqa_column_privilege_oracle.items');
SELECT pg_temp.column_privilege_probe('insert-b', 'INSERT INTO uqa_column_privilege_oracle.items(b) VALUES (4)');
SELECT pg_temp.column_privilege_probe('insert-a-denied', 'INSERT INTO uqa_column_privilege_oracle.items(a) VALUES (4)');
SELECT pg_temp.column_privilege_probe('update-c', 'UPDATE uqa_column_privilege_oracle.items SET c = 8');
SELECT pg_temp.column_privilege_probe('update-read-c-denied', 'UPDATE uqa_column_privilege_oracle.items SET c = c + 1');
SELECT pg_temp.column_privilege_probe('references-id', 'CREATE TABLE uqa_column_privilege_oracle.children(parent_id integer REFERENCES uqa_column_privilege_oracle.parents(id))');
SELECT pg_temp.column_privilege_probe('references-other-denied', 'CREATE TABLE uqa_column_privilege_oracle.denied(parent_id integer REFERENCES uqa_column_privilege_oracle.parents(other))');
SELECT 'visible-columns|' || string_agg(column_name, ',' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_schema = 'uqa_column_privilege_oracle' AND table_name = 'items';
SELECT 'visible-grants|' || string_agg(grantee || ':' || column_name || ':' || privilege_type || ':' || is_grantable, ',' ORDER BY grantee, column_name, privilege_type) FROM information_schema.column_privileges WHERE table_schema = 'uqa_column_privilege_oracle' AND table_name = 'items';
SELECT 'role-grants|' || string_agg(grantee || ':' || column_name || ':' || privilege_type, ',' ORDER BY grantee, column_name, privilege_type) FROM information_schema.role_column_grants WHERE table_schema = 'uqa_column_privilege_oracle' AND table_name = 'items';
RESET ROLE;

SELECT pg_temp.column_privilege_probe('dependent-restrict', 'SET ROLE uqa_column_acl_owner; REVOKE GRANT OPTION FOR SELECT(a) ON uqa_column_privilege_oracle.items FROM uqa_column_acl_delegate RESTRICT');
SET ROLE uqa_column_acl_owner;
REVOKE GRANT OPTION FOR SELECT(a) ON uqa_column_privilege_oracle.items FROM uqa_column_acl_delegate CASCADE;
RESET ROLE;
SELECT 'cascade|' || has_column_privilege('uqa_column_acl_delegate', 'uqa_column_privilege_oracle.items', 'a', 'SELECT') || '|' || has_column_privilege('uqa_column_acl_delegate', 'uqa_column_privilege_oracle.items', 'a', 'SELECT WITH GRANT OPTION') || '|' || has_column_privilege('uqa_column_acl_outsider', 'uqa_column_privilege_oracle.items', 'a', 'SELECT');

SET ROLE uqa_column_acl_owner;
GRANT SELECT ON SEQUENCE uqa_column_privilege_oracle.ids TO uqa_column_acl_reader;
RESET ROLE;
SELECT 'sequence|' || has_column_privilege('uqa_column_acl_reader', 'uqa_column_privilege_oracle.ids', 'last_value', 'SELECT') || '|' || has_column_privilege('uqa_column_acl_reader', 'uqa_column_privilege_oracle.ids', 3::smallint, 'SELECT') || '|' || coalesce(has_column_privilege('uqa_column_acl_reader', 'uqa_column_privilege_oracle.ids', 4::smallint, 'SELECT')::text, 'NULL');
SELECT 'missing-oids|' || coalesce(has_column_privilege('uqa_column_acl_reader', 4294967290::oid, 'a', 'SELECT')::text, 'NULL') || '|' || has_column_privilege(4294967289::oid, 'uqa_column_privilege_oracle.items', 'a', 'SELECT');
SELECT 'nulls|' || coalesce(has_column_privilege('uqa_column_privilege_oracle.items', NULL::text, 'SELECT')::text, 'NULL') || '|' || coalesce(has_column_privilege('uqa_column_privilege_oracle.items', NULL::smallint, 'SELECT')::text, 'NULL');
SELECT pg_temp.column_privilege_probe('missing-column', 'SELECT has_column_privilege(''uqa_column_privilege_oracle.items'', ''missing'', ''SELECT'')');
SELECT pg_temp.column_privilege_probe('invalid-privilege', 'SELECT has_column_privilege(''uqa_column_privilege_oracle.items'', ''a'', ''DELETE'')');
SELECT 'catalog-function|' || p.oid || '|' || p.oid::regprocedure || '|' || p.prosrc || '|' || p.proisstrict || '|' || p.provolatile::text || '|' || p.proparallel::text || '|' || p.proleakproof || '|' || p.prorettype || '|' || p.proargtypes::text FROM pg_catalog.pg_proc AS p WHERE p.proname = 'has_column_privilege' ORDER BY p.oid;

GRANT uqa_column_acl_new_owner TO uqa_column_acl_owner WITH INHERIT FALSE, SET TRUE;
SET ROLE uqa_column_acl_owner;
ALTER TABLE uqa_column_privilege_oracle.items OWNER TO uqa_column_acl_new_owner;
RESET ROLE;
SELECT 'transfer|' || c.relowner::regrole || '|' || string_agg(a.attname || '=' || coalesce(a.attacl::text, 'NULL'), ',' ORDER BY a.attnum) FROM pg_catalog.pg_class AS c JOIN pg_catalog.pg_attribute AS a ON a.attrelid = c.oid AND a.attnum > 0 WHERE c.oid = 'uqa_column_privilege_oracle.items'::regclass GROUP BY c.relowner;

DROP SCHEMA uqa_column_privilege_oracle CASCADE;
DROP ROLE uqa_column_acl_reader;
DROP ROLE uqa_column_acl_delegate;
DROP ROLE uqa_column_acl_outsider;
DROP ROLE uqa_column_acl_tablewide;
DROP ROLE uqa_column_acl_owner;
DROP ROLE uqa_column_acl_new_owner;
