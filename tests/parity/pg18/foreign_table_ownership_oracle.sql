\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_foreign_ownership_oracle CASCADE;
DROP SERVER IF EXISTS uqa_foreign_ownership_server CASCADE;
DROP ROLE IF EXISTS uqa_foreign_schema_owner;
DROP ROLE IF EXISTS uqa_foreign_owner_member;
DROP ROLE IF EXISTS uqa_foreign_owner_source;
DROP ROLE IF EXISTS uqa_foreign_owner_target;
DROP ROLE IF EXISTS uqa_foreign_owner_no_create;
DROP ROLE IF EXISTS uqa_foreign_owner_no_set;
DROP ROLE IF EXISTS uqa_foreign_owner_no_server;
DROP ROLE IF EXISTS uqa_foreign_owner_outsider;

CREATE ROLE uqa_foreign_owner_source;
CREATE ROLE uqa_foreign_schema_owner;
CREATE ROLE uqa_foreign_owner_member INHERIT;
CREATE ROLE uqa_foreign_owner_target;
CREATE ROLE uqa_foreign_owner_no_create;
CREATE ROLE uqa_foreign_owner_no_set;
CREATE ROLE uqa_foreign_owner_no_server;
CREATE ROLE uqa_foreign_owner_outsider;
GRANT uqa_foreign_owner_source TO uqa_foreign_owner_member;
GRANT uqa_foreign_owner_target, uqa_foreign_owner_no_create, uqa_foreign_owner_no_server TO uqa_foreign_owner_source;
GRANT uqa_foreign_owner_no_set TO uqa_foreign_owner_source WITH SET FALSE;
CREATE SCHEMA uqa_foreign_ownership_oracle AUTHORIZATION uqa_foreign_schema_owner;
GRANT USAGE ON SCHEMA uqa_foreign_ownership_oracle TO uqa_foreign_owner_member, uqa_foreign_owner_no_create, uqa_foreign_owner_outsider;
GRANT USAGE, CREATE ON SCHEMA uqa_foreign_ownership_oracle TO uqa_foreign_owner_source, uqa_foreign_owner_target, uqa_foreign_owner_no_set, uqa_foreign_owner_no_server;
CREATE SERVER uqa_foreign_ownership_server FOREIGN DATA WRAPPER postgres_fdw;
GRANT USAGE ON FOREIGN SERVER uqa_foreign_ownership_server TO uqa_foreign_owner_source, uqa_foreign_owner_target, uqa_foreign_owner_no_create, uqa_foreign_owner_no_set;

CREATE OR REPLACE FUNCTION pg_temp.foreign_owner_probe(label text, role_name text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
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

SET ROLE uqa_foreign_owner_source;
CREATE FOREIGN TABLE uqa_foreign_ownership_oracle.items(id integer) SERVER uqa_foreign_ownership_server;
CREATE FOREIGN TABLE uqa_foreign_ownership_oracle.rollback_items(id integer) SERVER uqa_foreign_ownership_server;
CREATE FOREIGN TABLE uqa_foreign_ownership_oracle.schema_drop_items(id integer) SERVER uqa_foreign_ownership_server;
RESET ROLE;

SELECT 'initial-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_foreign_ownership_oracle' AND c.relname = 'items';

SELECT pg_temp.foreign_owner_probe('outsider-owner', 'uqa_foreign_owner_outsider', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_outsider');
SELECT pg_temp.foreign_owner_probe('outsider-drop', 'uqa_foreign_owner_outsider', 'DROP FOREIGN TABLE uqa_foreign_ownership_oracle.items');
SELECT pg_temp.foreign_owner_probe('schema-owner-drop', 'uqa_foreign_schema_owner', 'DROP FOREIGN TABLE uqa_foreign_ownership_oracle.schema_drop_items');
SELECT pg_temp.foreign_owner_probe('member-noop-owner', 'uqa_foreign_owner_member', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_source');
SELECT pg_temp.foreign_owner_probe('alter-table-spelling', 'uqa_foreign_owner_source', 'ALTER TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_source');
SELECT pg_temp.foreign_owner_probe('missing-owner', 'uqa_foreign_owner_source', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_missing');
SELECT pg_temp.foreign_owner_probe('owner-without-create', 'uqa_foreign_owner_source', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_no_create');
SELECT pg_temp.foreign_owner_probe('owner-without-set', 'uqa_foreign_owner_source', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_no_set');
SELECT pg_temp.foreign_owner_probe('owner-without-server', 'uqa_foreign_owner_source', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_no_server');
SELECT pg_temp.foreign_owner_probe('owner-transfer', 'uqa_foreign_owner_source', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_target');

SELECT 'transferred-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_foreign_ownership_oracle' AND c.relname = 'items';

REVOKE uqa_foreign_owner_target FROM uqa_foreign_owner_source;
SELECT pg_temp.foreign_owner_probe('former-owner-transfer', 'uqa_foreign_owner_source', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_source');
SELECT pg_temp.foreign_owner_probe('new-owner-noop', 'uqa_foreign_owner_target', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_target');

BEGIN;
ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.rollback_items OWNER TO uqa_foreign_owner_target;
SELECT 'transaction-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_foreign_ownership_oracle' AND c.relname = 'rollback_items';
ROLLBACK;
SELECT 'rollback-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_foreign_ownership_oracle' AND c.relname = 'rollback_items';

SELECT pg_temp.foreign_owner_probe('owner-drop-dependent', 'postgres', 'DROP ROLE uqa_foreign_owner_target');
SELECT pg_temp.foreign_owner_probe('superuser-transfer-no-create', 'postgres', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_no_create');
SELECT pg_temp.foreign_owner_probe('superuser-transfer-no-server', 'postgres', 'ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO uqa_foreign_owner_no_server');
SELECT 'superuser-transferred-owner|' || pg_get_userbyid(c.relowner)
FROM pg_class AS c
JOIN pg_namespace AS n ON n.oid = c.relnamespace
WHERE n.nspname = 'uqa_foreign_ownership_oracle' AND c.relname = 'items';
ALTER FOREIGN TABLE uqa_foreign_ownership_oracle.items OWNER TO postgres;
REVOKE ALL ON SCHEMA uqa_foreign_ownership_oracle FROM uqa_foreign_owner_target;
REVOKE USAGE ON FOREIGN SERVER uqa_foreign_ownership_server FROM uqa_foreign_owner_target;
SELECT pg_temp.foreign_owner_probe('owner-drop-released', 'postgres', 'DROP ROLE uqa_foreign_owner_target');

DROP SCHEMA uqa_foreign_ownership_oracle CASCADE;
DROP SERVER uqa_foreign_ownership_server;
DROP ROLE uqa_foreign_schema_owner;
DROP ROLE uqa_foreign_owner_member;
DROP ROLE uqa_foreign_owner_source;
DROP ROLE uqa_foreign_owner_no_create;
DROP ROLE uqa_foreign_owner_no_set;
DROP ROLE uqa_foreign_owner_no_server;
DROP ROLE uqa_foreign_owner_outsider;
