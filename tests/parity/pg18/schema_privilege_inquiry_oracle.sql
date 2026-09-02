\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_schema_privilege_oracle CASCADE;
DROP SCHEMA IF EXISTS "12345" CASCADE;
DROP ROLE IF EXISTS uqa_schema_privilege_member;
DROP ROLE IF EXISTS uqa_schema_privilege_reader;
DROP ROLE IF EXISTS uqa_schema_privilege_outsider;
DROP ROLE IF EXISTS uqa_schema_privilege_owner;

CREATE ROLE uqa_schema_privilege_owner;
CREATE ROLE uqa_schema_privilege_reader;
CREATE ROLE uqa_schema_privilege_outsider;
CREATE ROLE uqa_schema_privilege_member INHERIT;
CREATE SCHEMA uqa_schema_privilege_oracle AUTHORIZATION uqa_schema_privilege_owner;
CREATE SCHEMA "12345" AUTHORIZATION uqa_schema_privilege_owner;
GRANT USAGE ON SCHEMA uqa_schema_privilege_oracle TO uqa_schema_privilege_reader;
GRANT CREATE ON SCHEMA uqa_schema_privilege_oracle TO uqa_schema_privilege_reader WITH GRANT OPTION;
GRANT USAGE ON SCHEMA "12345" TO uqa_schema_privilege_reader;
GRANT uqa_schema_privilege_owner TO uqa_schema_privilege_member;

CREATE OR REPLACE FUNCTION pg_temp.schema_privilege_probe(label text, command text)
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

SELECT 'catalog|' || p.oid || '|' || p.oid::regprocedure || '|' || p.prosrc || '|' || p.proisstrict || '|' || p.provolatile::text || '|' || p.proparallel::text || '|' || p.proleakproof || '|' || p.prorettype || '|' || p.proargtypes::text FROM pg_catalog.pg_proc AS p WHERE p.proname = 'has_schema_privilege' ORDER BY p.oid;

SET ROLE uqa_schema_privilege_owner;
SELECT 'owner-current|' || has_schema_privilege('uqa_schema_privilege_oracle', 'USAGE WITH GRANT OPTION') || '|' || has_schema_privilege('uqa_schema_privilege_oracle', 'CREATE WITH GRANT OPTION');
RESET ROLE;

SET ROLE uqa_schema_privilege_reader;
SELECT 'reader-current-name|' || has_schema_privilege('uqa_schema_privilege_oracle', 'USAGE') || '|' || has_schema_privilege('uqa_schema_privilege_oracle', 'USAGE WITH GRANT OPTION') || '|' || has_schema_privilege('uqa_schema_privilege_oracle', 'CREATE') || '|' || has_schema_privilege('uqa_schema_privilege_oracle', 'CREATE WITH GRANT OPTION');
SELECT 'reader-current-id|' || has_schema_privilege((SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'uqa_schema_privilege_oracle'), 'USAGE');
SELECT 'reader-name-name|' || has_schema_privilege('uqa_schema_privilege_reader', 'uqa_schema_privilege_oracle', 'USAGE');
SELECT 'reader-name-id|' || has_schema_privilege('uqa_schema_privilege_reader', (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'uqa_schema_privilege_oracle'), 'CREATE WITH GRANT OPTION');
SELECT 'reader-id-name|' || has_schema_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'uqa_schema_privilege_reader'), 'uqa_schema_privilege_oracle', 'USAGE');
SELECT 'reader-id-id|' || has_schema_privilege((SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'uqa_schema_privilege_reader'), (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'uqa_schema_privilege_oracle'), 'CREATE');
SELECT 'reader-any-list|' || has_schema_privilege('uqa_schema_privilege_oracle', 'USAGE WITH GRANT OPTION, CREATE WITH GRANT OPTION');
SELECT 'numeric-schema-name|' || has_schema_privilege('12345', 'USAGE');
SELECT 'reader-missing-schema-id|' || coalesce(has_schema_privilege(4294967290::oid, 'USAGE')::text, 'NULL');
SELECT 'reader-explicit-missing-schema-id|' || coalesce(has_schema_privilege('uqa_schema_privilege_reader', 4294967290::oid, 'USAGE')::text, 'NULL');
RESET ROLE;

SET ROLE uqa_schema_privilege_member;
SELECT 'inherited-owner|' || has_schema_privilege('uqa_schema_privilege_oracle', 'USAGE WITH GRANT OPTION') || '|' || has_schema_privilege('uqa_schema_privilege_oracle', 'CREATE WITH GRANT OPTION');
RESET ROLE;

SET ROLE uqa_schema_privilege_outsider;
SELECT 'outsider|' || has_schema_privilege('uqa_schema_privilege_oracle', 'USAGE, CREATE');
SELECT 'outsider-missing-schema-id|' || coalesce(has_schema_privilege(4294967290::oid, 'USAGE')::text, 'NULL');
SELECT 'system-defaults|' || has_schema_privilege('public', 'USAGE') || '|' || has_schema_privilege('public', 'CREATE') || '|' || has_schema_privilege('pg_catalog', 'USAGE') || '|' || has_schema_privilege('pg_catalog', 'CREATE') || '|' || has_schema_privilege('information_schema', 'USAGE') || '|' || has_schema_privilege('information_schema', 'CREATE') || '|' || has_schema_privilege('ag_catalog', 'USAGE') || '|' || has_schema_privilege('ag_catalog', 'CREATE');
SELECT pg_temp.schema_privilege_probe('temporary-alias-outsider', 'SELECT has_schema_privilege(''pg_temp'', ''USAGE'')');
SELECT 'temporary-outsider|' || has_schema_privilege((SELECT nspname FROM pg_catalog.pg_namespace WHERE oid = pg_my_temp_schema()), 'USAGE') || '|' || has_schema_privilege(pg_my_temp_schema(), 'CREATE') || '|' || has_schema_privilege(pg_my_temp_schema(), 'USAGE WITH GRANT OPTION') || '|' || has_schema_privilege(pg_my_temp_schema(), 'CREATE WITH GRANT OPTION');
RESET ROLE;

SELECT 'superuser|' || has_schema_privilege('uqa_schema_privilege_oracle', 'USAGE WITH GRANT OPTION, CREATE WITH GRANT OPTION');
SELECT pg_temp.schema_privilege_probe('temporary-alias-superuser', 'SELECT has_schema_privilege(''pg_temp'', ''USAGE'')');
SELECT 'temporary-superuser|' || has_schema_privilege((SELECT nspname FROM pg_catalog.pg_namespace WHERE oid = pg_my_temp_schema()), 'USAGE WITH GRANT OPTION') || '|' || has_schema_privilege(pg_my_temp_schema(), 'CREATE WITH GRANT OPTION');
SELECT 'missing-schema-id|' || coalesce(has_schema_privilege(4294967290::oid, 'USAGE')::text, 'NULL');
SELECT 'missing-role-id|' || coalesce(has_schema_privilege(4294967289::oid, 'uqa_schema_privilege_oracle', 'USAGE')::text, 'NULL');
SELECT 'missing-role-and-schema-id|' || coalesce(has_schema_privilege(4294967289::oid, 4294967290::oid, 'USAGE')::text, 'NULL');
SELECT 'null-current-name|' || coalesce(has_schema_privilege(NULL::text, 'USAGE')::text, 'NULL');
SELECT 'null-current-id|' || coalesce(has_schema_privilege(NULL::oid, 'USAGE')::text, 'NULL');
SELECT 'null-role|' || coalesce(has_schema_privilege(NULL::name, 'uqa_schema_privilege_oracle', 'USAGE')::text, 'NULL');
SELECT pg_temp.schema_privilege_probe('missing-schema-name', 'SELECT has_schema_privilege(''uqa_schema_privilege_missing'', ''USAGE'')');
SELECT pg_temp.schema_privilege_probe('missing-role-name', 'SELECT has_schema_privilege(''uqa_schema_privilege_missing'', ''uqa_schema_privilege_oracle'', ''USAGE'')');
SELECT pg_temp.schema_privilege_probe('invalid-privilege', 'SELECT has_schema_privilege(''uqa_schema_privilege_oracle'', ''SELECT'')');
SELECT pg_temp.schema_privilege_probe('all-privilege', 'SELECT has_schema_privilege(''uqa_schema_privilege_oracle'', ''ALL'')');
SELECT pg_temp.schema_privilege_probe('missing-role-schema-invalid', 'SELECT has_schema_privilege(''uqa_schema_privilege_missing'', ''uqa_schema_privilege_missing'', ''SELECT'')');
SELECT pg_temp.schema_privilege_probe('missing-schema-invalid', 'SELECT has_schema_privilege(''uqa_schema_privilege_missing'', ''SELECT'')');
SELECT pg_temp.schema_privilege_probe('missing-schema-id-invalid', 'SELECT has_schema_privilege(4294967290::oid, ''SELECT'')');
SELECT pg_temp.schema_privilege_probe('missing-role-id-invalid', 'SELECT has_schema_privilege(4294967289::oid, ''uqa_schema_privilege_oracle'', ''SELECT'')');
SELECT pg_temp.schema_privilege_probe('missing-role-id-missing-schema', 'SELECT has_schema_privilege(4294967289::oid, ''uqa_schema_privilege_missing'', ''USAGE'')');

DROP SCHEMA uqa_schema_privilege_oracle CASCADE;
DROP SCHEMA "12345" CASCADE;
DROP ROLE uqa_schema_privilege_member;
DROP ROLE uqa_schema_privilege_reader;
DROP ROLE uqa_schema_privilege_outsider;
DROP ROLE uqa_schema_privilege_owner;
