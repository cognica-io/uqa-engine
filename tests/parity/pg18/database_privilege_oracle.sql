\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP DATABASE IF EXISTS uqa_database_privilege_oracle;
DROP DATABASE IF EXISTS "12345";
DROP ROLE IF EXISTS uqa_database_privilege_delegate;
DROP ROLE IF EXISTS uqa_database_privilege_member;
DROP ROLE IF EXISTS uqa_database_privilege_reader;
DROP ROLE IF EXISTS uqa_database_privilege_outsider;
DROP ROLE IF EXISTS uqa_database_privilege_owner;

CREATE ROLE uqa_database_privilege_owner;
CREATE ROLE uqa_database_privilege_reader;
CREATE ROLE uqa_database_privilege_outsider;
CREATE ROLE uqa_database_privilege_member INHERIT;
CREATE ROLE uqa_database_privilege_delegate;
GRANT uqa_database_privilege_owner TO uqa_database_privilege_member;
CREATE DATABASE uqa_database_privilege_oracle OWNER uqa_database_privilege_owner;
CREATE DATABASE "12345" OWNER uqa_database_privilege_owner;

CREATE OR REPLACE FUNCTION pg_temp.database_privilege_probe(label text, command text)
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

SELECT 'catalog|' || p.oid || '|' || p.oid::regprocedure || '|' || p.prosrc || '|' || p.proisstrict || '|' || p.provolatile::text || '|' || p.proparallel::text || '|' || p.proleakproof || '|' || p.prorettype || '|' || p.proargtypes::text FROM pg_catalog.pg_proc AS p WHERE p.proname = 'has_database_privilege' ORDER BY p.oid;
SELECT 'initial-catalog|' || datdba::regrole || '|' || coalesce(datacl::text, 'NULL') FROM pg_catalog.pg_database WHERE datname = 'uqa_database_privilege_oracle';

SET ROLE uqa_database_privilege_outsider;
SELECT 'default-outsider|' || has_database_privilege('uqa_database_privilege_oracle', 'CONNECT') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'CREATE') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'TEMP') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'TEMPORARY') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'CONNECT WITH GRANT OPTION') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'TEMPORARY WITH GRANT OPTION');
RESET ROLE;

GRANT CREATE ON DATABASE uqa_database_privilege_oracle TO uqa_database_privilege_reader WITH GRANT OPTION;
GRANT TEMPORARY ON DATABASE uqa_database_privilege_oracle TO uqa_database_privilege_reader;
REVOKE CONNECT, TEMPORARY ON DATABASE uqa_database_privilege_oracle FROM PUBLIC;
SELECT 'granted-catalog|' || datdba::regrole || '|' || datacl::text FROM pg_catalog.pg_database WHERE datname = 'uqa_database_privilege_oracle';

SET ROLE uqa_database_privilege_reader;
SELECT 'reader-current-name|' || has_database_privilege('uqa_database_privilege_oracle', 'CONNECT') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'CREATE') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'CREATE WITH GRANT OPTION') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'TEMPORARY') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'TEMPORARY WITH GRANT OPTION');
SELECT 'reader-current-id|' || has_database_privilege((SELECT oid FROM pg_catalog.pg_database WHERE datname = 'uqa_database_privilege_oracle'), 'TEMP');
SELECT 'reader-name-name|' || has_database_privilege('uqa_database_privilege_reader', 'uqa_database_privilege_oracle', 'CREATE');
SELECT 'reader-name-id|' || has_database_privilege('uqa_database_privilege_reader', (SELECT oid FROM pg_catalog.pg_database WHERE datname = 'uqa_database_privilege_oracle'), 'CREATE WITH GRANT OPTION');
SELECT 'reader-id-name|' || has_database_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'uqa_database_privilege_reader'), 'uqa_database_privilege_oracle', 'TEMPORARY');
SELECT 'reader-id-id|' || has_database_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'uqa_database_privilege_reader'), (SELECT oid FROM pg_catalog.pg_database WHERE datname = 'uqa_database_privilege_oracle'), 'CONNECT');
SELECT 'reader-any-list|' || has_database_privilege('uqa_database_privilege_oracle', 'CONNECT, CREATE WITH GRANT OPTION');
SELECT 'numeric-database-name|' || has_database_privilege('12345', 'TEMP');
GRANT CREATE ON DATABASE uqa_database_privilege_oracle TO uqa_database_privilege_delegate;
RESET ROLE;

SELECT pg_temp.database_privilege_probe('dependent-restrict', 'REVOKE GRANT OPTION FOR CREATE ON DATABASE uqa_database_privilege_oracle FROM uqa_database_privilege_reader RESTRICT');
REVOKE GRANT OPTION FOR CREATE ON DATABASE uqa_database_privilege_oracle FROM uqa_database_privilege_reader CASCADE;
SELECT 'delegate-after-cascade|' || has_database_privilege('uqa_database_privilege_delegate', 'uqa_database_privilege_oracle', 'CREATE');

SET ROLE uqa_database_privilege_owner;
SELECT 'owner-before-revoke|' || has_database_privilege('uqa_database_privilege_oracle', 'CONNECT WITH GRANT OPTION, CREATE WITH GRANT OPTION, TEMPORARY WITH GRANT OPTION');
REVOKE ALL PRIVILEGES ON DATABASE uqa_database_privilege_oracle FROM uqa_database_privilege_owner;
SELECT 'owner-after-revoke|' || has_database_privilege('uqa_database_privilege_oracle', 'CONNECT WITH GRANT OPTION') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'CREATE WITH GRANT OPTION') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'TEMPORARY WITH GRANT OPTION');
RESET ROLE;

SET ROLE uqa_database_privilege_member;
SELECT 'inherited-owner|' || has_database_privilege('uqa_database_privilege_oracle', 'CONNECT WITH GRANT OPTION') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'CREATE WITH GRANT OPTION') || '|' || has_database_privilege('uqa_database_privilege_oracle', 'TEMPORARY WITH GRANT OPTION');
RESET ROLE;

SET ROLE uqa_database_privilege_outsider;
SELECT 'outsider-after-revoke|' || has_database_privilege('uqa_database_privilege_oracle', 'CONNECT, CREATE, TEMPORARY');
SELECT 'outsider-missing-database-id|' || coalesce(has_database_privilege(4294967290::oid, 'CONNECT')::text, 'NULL');
RESET ROLE;

SELECT 'superuser|' || has_database_privilege('uqa_database_privilege_oracle', 'CONNECT WITH GRANT OPTION, CREATE WITH GRANT OPTION, TEMPORARY WITH GRANT OPTION');
SELECT 'missing-database-id|' || coalesce(has_database_privilege(4294967290::oid, 'CONNECT')::text, 'NULL');
SELECT 'missing-role-id|' || coalesce(has_database_privilege(4294967289::oid, 'uqa_database_privilege_oracle', 'CONNECT')::text, 'NULL');
SELECT 'missing-role-and-database-id|' || coalesce(has_database_privilege(4294967289::oid, 4294967290::oid, 'CONNECT')::text, 'NULL');
SELECT 'null-current-name|' || coalesce(has_database_privilege(NULL::text, 'CONNECT')::text, 'NULL');
SELECT 'null-current-id|' || coalesce(has_database_privilege(NULL::oid, 'CONNECT')::text, 'NULL');
SELECT 'null-role|' || coalesce(has_database_privilege(NULL::name, 'uqa_database_privilege_oracle', 'CONNECT')::text, 'NULL');
SELECT pg_temp.database_privilege_probe('missing-database-name', 'SELECT has_database_privilege(''uqa_database_privilege_missing'', ''CONNECT'')');
SELECT pg_temp.database_privilege_probe('missing-role-name', 'SELECT has_database_privilege(''uqa_database_privilege_missing'', ''uqa_database_privilege_oracle'', ''CONNECT'')');
SELECT pg_temp.database_privilege_probe('invalid-privilege', 'SELECT has_database_privilege(''uqa_database_privilege_oracle'', ''SELECT'')');
SELECT pg_temp.database_privilege_probe('all-privilege', 'SELECT has_database_privilege(''uqa_database_privilege_oracle'', ''ALL'')');
SELECT pg_temp.database_privilege_probe('missing-role-database-invalid', 'SELECT has_database_privilege(''uqa_database_privilege_missing'', ''uqa_database_privilege_missing'', ''SELECT'')');
SELECT pg_temp.database_privilege_probe('missing-database-invalid', 'SELECT has_database_privilege(''uqa_database_privilege_missing'', ''SELECT'')');
SELECT pg_temp.database_privilege_probe('missing-database-id-invalid', 'SELECT has_database_privilege(4294967290::oid, ''SELECT'')');
SELECT pg_temp.database_privilege_probe('missing-role-id-invalid', 'SELECT has_database_privilege(4294967289::oid, ''uqa_database_privilege_oracle'', ''SELECT'')');
SELECT pg_temp.database_privilege_probe('missing-role-id-missing-database', 'SELECT has_database_privilege(4294967289::oid, ''uqa_database_privilege_missing'', ''CONNECT'')');

DROP DATABASE uqa_database_privilege_oracle;
DROP DATABASE "12345";
DROP ROLE uqa_database_privilege_delegate;
DROP ROLE uqa_database_privilege_member;
DROP ROLE uqa_database_privilege_reader;
DROP ROLE uqa_database_privilege_outsider;
DROP ROLE uqa_database_privilege_owner;
