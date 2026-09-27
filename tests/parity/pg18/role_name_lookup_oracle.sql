-- PostgreSQL 18.4 role-name lookup reference. All fixture state is rolled back.
BEGIN;
CREATE ROLE uqa_role_name_oracle;
CREATE TEMP TABLE uqa_role_name_oid AS SELECT oid FROM pg_roles WHERE rolname = 'uqa_role_name_oracle';
SELECT 'known|' || pg_catalog.pg_get_userbyid(oid) FROM uqa_role_name_oid;
SELECT 'unknown|' || pg_get_userbyid(0) || '|' || pg_get_userbyid(4294967295::oid) || '|' || pg_get_userbyid(-1);
SELECT 'null-type|' || (pg_get_userbyid(NULL) IS NULL)::text || '|' || pg_typeof(pg_get_userbyid(0))::text;
SELECT 'catalog|' || oid::text || '|' || prorettype::text || '|' || proargtypes::text || '|' || proisstrict::text || '|' || provolatile::text || '|' || proparallel::text || '|' || proleakproof::text || '|' || prosrc FROM pg_proc WHERE proname = 'pg_get_userbyid';
ALTER ROLE uqa_role_name_oracle RENAME TO uqa_role_name_renamed;
SELECT 'renamed|' || pg_get_userbyid(oid) FROM uqa_role_name_oid;
DROP ROLE uqa_role_name_renamed;
SELECT 'dropped|' || (pg_get_userbyid(oid) = 'unknown (OID=' || oid::text || ')')::text FROM uqa_role_name_oid;
ROLLBACK;
