\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_table_privilege_oracle CASCADE;
DROP ROLE IF EXISTS uqa_table_acl_reader;
DROP ROLE IF EXISTS uqa_table_acl_delegate;
DROP ROLE IF EXISTS uqa_table_acl_outsider;
DROP ROLE IF EXISTS uqa_table_acl_owner;
DROP ROLE IF EXISTS uqa_table_acl_new_owner;

CREATE ROLE uqa_table_acl_owner;
CREATE ROLE uqa_table_acl_new_owner;
CREATE ROLE uqa_table_acl_delegate;
CREATE ROLE uqa_table_acl_reader;
CREATE ROLE uqa_table_acl_outsider;
CREATE SCHEMA uqa_table_privilege_oracle AUTHORIZATION uqa_table_acl_owner;
GRANT USAGE ON SCHEMA uqa_table_privilege_oracle TO uqa_table_acl_new_owner, uqa_table_acl_delegate, uqa_table_acl_reader, uqa_table_acl_outsider;
GRANT CREATE ON SCHEMA uqa_table_privilege_oracle TO uqa_table_acl_new_owner;

CREATE OR REPLACE FUNCTION pg_temp.table_privilege_probe(label text, command text)
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

SET ROLE uqa_table_acl_owner;
CREATE TABLE uqa_table_privilege_oracle.items(id integer PRIMARY KEY, value integer);
CREATE SEQUENCE uqa_table_privilege_oracle.ids;
INSERT INTO uqa_table_privilege_oracle.items VALUES (1, 10), (2, 20);
SELECT 'default|' || (relacl IS NULL) || '|' || relowner::regrole FROM pg_catalog.pg_class WHERE oid = 'uqa_table_privilege_oracle.items'::regclass;
GRANT SELECT ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_delegate WITH GRANT OPTION;
GRANT INSERT ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_outsider;
RESET ROLE;

SET ROLE uqa_table_acl_delegate;
GRANT SELECT ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_reader;
RESET ROLE;
SELECT 'chain-catalog|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_table_privilege_oracle.items'::regclass;

SET ROLE uqa_table_acl_reader;
SELECT pg_temp.table_privilege_probe('select-granted', 'SELECT value FROM uqa_table_privilege_oracle.items');
SELECT pg_temp.table_privilege_probe('insert-denied', 'INSERT INTO uqa_table_privilege_oracle.items VALUES (3, 30)');
RESET ROLE;

SET ROLE uqa_table_acl_outsider;
SELECT pg_temp.table_privilege_probe('insert-constant-returning', 'INSERT INTO uqa_table_privilege_oracle.items VALUES (3, 30) RETURNING 1');
SELECT pg_temp.table_privilege_probe('insert-column-returning-denied', 'INSERT INTO uqa_table_privilege_oracle.items VALUES (4, 40) RETURNING id');
RESET ROLE;

SET ROLE uqa_table_acl_owner;
GRANT SELECT ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_outsider;
RESET ROLE;
SET ROLE uqa_table_acl_outsider;
SELECT pg_temp.table_privilege_probe('insert-column-returning-granted', 'INSERT INTO uqa_table_privilege_oracle.items VALUES (4, 40) RETURNING id');
RESET ROLE;

SET ROLE uqa_table_acl_owner;
REVOKE INSERT, SELECT ON TABLE uqa_table_privilege_oracle.items FROM uqa_table_acl_outsider;
GRANT UPDATE ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_outsider;
RESET ROLE;
SET ROLE uqa_table_acl_outsider;
SELECT pg_temp.table_privilege_probe('update-constant', 'UPDATE uqa_table_privilege_oracle.items SET value = 50');
SELECT pg_temp.table_privilege_probe('update-read-denied', 'UPDATE uqa_table_privilege_oracle.items SET value = value + 1');
RESET ROLE;

SET ROLE uqa_table_acl_owner;
REVOKE UPDATE ON TABLE uqa_table_privilege_oracle.items FROM uqa_table_acl_outsider;
GRANT DELETE ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_outsider;
RESET ROLE;
SET ROLE uqa_table_acl_outsider;
SELECT pg_temp.table_privilege_probe('delete-read-denied', 'DELETE FROM uqa_table_privilege_oracle.items WHERE id = 1');
SELECT pg_temp.table_privilege_probe('delete-without-read', 'DELETE FROM uqa_table_privilege_oracle.items');
RESET ROLE;

SET ROLE uqa_table_acl_owner;
INSERT INTO uqa_table_privilege_oracle.items VALUES (1, 10);
REVOKE DELETE ON TABLE uqa_table_privilege_oracle.items FROM uqa_table_acl_outsider;
GRANT TRUNCATE ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_outsider;
RESET ROLE;
SET ROLE uqa_table_acl_outsider;
SELECT pg_temp.table_privilege_probe('truncate-granted', 'TRUNCATE TABLE uqa_table_privilege_oracle.items');
RESET ROLE;

SET ROLE uqa_table_acl_owner;
REVOKE TRUNCATE ON TABLE uqa_table_privilege_oracle.items FROM uqa_table_acl_outsider;
GRANT MAINTAIN ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_outsider;
RESET ROLE;
SET ROLE uqa_table_acl_outsider;
SELECT pg_temp.table_privilege_probe('analyze-granted', 'ANALYZE uqa_table_privilege_oracle.items');
RESET ROLE;

SET ROLE uqa_table_acl_owner;
REVOKE ALL PRIVILEGES ON TABLE uqa_table_privilege_oracle.items FROM uqa_table_acl_owner;
SELECT 'implicit-owner|' || has_table_privilege('uqa_table_privilege_oracle.items', 'SELECT WITH GRANT OPTION, MAINTAIN WITH GRANT OPTION');
RESET ROLE;

SELECT pg_temp.table_privilege_probe('dependent-restrict', 'SET ROLE uqa_table_acl_owner; REVOKE GRANT OPTION FOR SELECT ON TABLE uqa_table_privilege_oracle.items FROM uqa_table_acl_delegate RESTRICT');
SET ROLE uqa_table_acl_owner;
REVOKE GRANT OPTION FOR SELECT ON TABLE uqa_table_privilege_oracle.items FROM uqa_table_acl_delegate CASCADE;
RESET ROLE;
SELECT 'cascade|' || has_table_privilege('uqa_table_acl_delegate', 'uqa_table_privilege_oracle.items', 'SELECT') || '|' || has_table_privilege('uqa_table_acl_delegate', 'uqa_table_privilege_oracle.items', 'SELECT WITH GRANT OPTION') || '|' || has_table_privilege('uqa_table_acl_reader', 'uqa_table_privilege_oracle.items', 'SELECT');

SET ROLE uqa_table_acl_owner;
GRANT SELECT ON ALL TABLES IN SCHEMA uqa_table_privilege_oracle TO uqa_table_acl_reader;
RESET ROLE;
SELECT 'all-tables-schema|' || has_table_privilege('uqa_table_acl_reader', 'uqa_table_privilege_oracle.items', 'SELECT') || '|' || has_sequence_privilege('uqa_table_acl_reader', 'uqa_table_privilege_oracle.ids', 'SELECT');
SET ROLE uqa_table_acl_owner;
GRANT SELECT, UPDATE ON TABLE uqa_table_privilege_oracle.ids TO uqa_table_acl_reader;
RESET ROLE;
SELECT 'sequence-table-target|' || has_table_privilege('uqa_table_acl_reader', 'uqa_table_privilege_oracle.ids', 'SELECT, UPDATE') || '|' || has_table_privilege('uqa_table_acl_reader', 'uqa_table_privilege_oracle.ids'::regclass, 'DELETE') || '|' || has_sequence_privilege('uqa_table_acl_reader', 'uqa_table_privilege_oracle.ids', 'SELECT, UPDATE');
SELECT 'sequence-catalog|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_table_privilege_oracle.ids'::regclass;

SELECT 'explicit-overloads|' || has_table_privilege('uqa_table_acl_reader', 'uqa_table_privilege_oracle.items', 'SELECT') || '|' || has_table_privilege('uqa_table_acl_reader', 'uqa_table_privilege_oracle.items'::regclass, 'SELECT') || '|' || has_table_privilege('uqa_table_acl_reader'::regrole::oid, 'uqa_table_privilege_oracle.items', 'SELECT') || '|' || has_table_privilege('uqa_table_acl_reader'::regrole::oid, 'uqa_table_privilege_oracle.items'::regclass, 'SELECT');
SET ROLE uqa_table_acl_reader;
SELECT 'current-overloads|' || has_table_privilege('uqa_table_privilege_oracle.items', 'SELECT') || '|' || has_table_privilege('uqa_table_privilege_oracle.items'::regclass, 'SELECT');
RESET ROLE;
SELECT 'missing-oids|' || coalesce(has_table_privilege(4294967290::oid, 'SELECT')::text, 'NULL') || '|' || has_table_privilege(4294967289::oid, 'uqa_table_privilege_oracle.items', 'SELECT');
SELECT 'nulls|' || coalesce(has_table_privilege(NULL::text, 'SELECT')::text, 'NULL') || '|' || coalesce(has_table_privilege(NULL::oid, 'SELECT')::text, 'NULL');
SELECT pg_temp.table_privilege_probe('numeric-table-name', 'SELECT has_table_privilege(''4294967290'', ''SELECT'')');
SELECT pg_temp.table_privilege_probe('invalid-inquiry-privilege', 'SELECT has_table_privilege(''uqa_table_privilege_oracle.items'', ''USAGE'')');
SELECT pg_temp.table_privilege_probe('missing-target-before-role', 'GRANT SELECT ON TABLE uqa_table_privilege_oracle.missing TO uqa_table_acl_missing');
SELECT pg_temp.table_privilege_probe('missing-role-before-invalid-privilege', 'GRANT USAGE ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_missing');
SELECT pg_temp.table_privilege_probe('invalid-grant-privilege', 'GRANT USAGE ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_outsider');
SELECT 'information-schema|' || count(*) FROM information_schema.tables WHERE table_schema = 'uqa_table_privilege_oracle' AND table_name = 'items';
SELECT 'catalog|' || p.oid || '|' || p.oid::regprocedure || '|' || p.prosrc || '|' || p.proisstrict || '|' || p.provolatile::text || '|' || p.proparallel::text || '|' || p.proleakproof || '|' || p.prorettype || '|' || p.proargtypes::text FROM pg_catalog.pg_proc AS p WHERE p.proname = 'has_table_privilege' ORDER BY p.oid;

SET ROLE uqa_table_acl_owner;
GRANT ALL PRIVILEGES ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_outsider;
GRANT ALL PRIVILEGES ON TABLE uqa_table_privilege_oracle.items TO uqa_table_acl_new_owner WITH GRANT OPTION;
RESET ROLE;
GRANT uqa_table_acl_new_owner TO uqa_table_acl_owner WITH INHERIT FALSE, SET TRUE;
SET ROLE uqa_table_acl_owner;
ALTER TABLE uqa_table_privilege_oracle.items OWNER TO uqa_table_acl_new_owner;
RESET ROLE;
SELECT 'transfer|' || relowner::regrole || '|' || relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_table_privilege_oracle.items'::regclass;

DROP SCHEMA uqa_table_privilege_oracle CASCADE;
DROP ROLE uqa_table_acl_reader;
DROP ROLE uqa_table_acl_delegate;
DROP ROLE uqa_table_acl_outsider;
DROP ROLE uqa_table_acl_owner;
DROP ROLE uqa_table_acl_new_owner;
