\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_index_alpha CASCADE;
DROP SCHEMA IF EXISTS uqa_index_beta CASCADE;
DROP SCHEMA IF EXISTS uqa_index_hidden CASCADE;
DROP SCHEMA IF EXISTS "uqa_index.dot" CASCADE;
DROP ROLE IF EXISTS uqa_index_caller;

CREATE ROLE uqa_index_caller;
CREATE SCHEMA uqa_index_alpha;
CREATE SCHEMA uqa_index_beta;
CREATE SCHEMA uqa_index_hidden;
CREATE SCHEMA "uqa_index.dot";
REVOKE ALL ON SCHEMA uqa_index_hidden FROM PUBLIC;

CREATE OR REPLACE FUNCTION pg_temp.index_namespace_probe(label text, role_name text, path text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    PERFORM set_config('search_path', path, true);
    EXECUTE command;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE;
    RESET ROLE;
    RESET search_path;
    RETURN label || '|' || state;
END
$oracle$;

CREATE TABLE uqa_index_alpha.items(id integer);
CREATE TABLE uqa_index_beta.items(id integer);
CREATE INDEX shared_idx ON uqa_index_alpha.items(id);
CREATE INDEX shared_idx ON uqa_index_beta.items(id);
SELECT 'same-local|' || string_agg(schemaname || '.' || indexname, ',' ORDER BY schemaname)
  FROM pg_indexes
 WHERE schemaname IN ('uqa_index_alpha', 'uqa_index_beta')
   AND indexname = 'shared_idx';
SELECT 'distinct-oids|' || ('uqa_index_alpha.shared_idx'::regclass::oid <> 'uqa_index_beta.shared_idx'::regclass::oid);
SELECT 'regclass-text|' || 'uqa_index_alpha.shared_idx'::regclass::text || ',' || 'uqa_index_beta.shared_idx'::regclass::text;

CREATE TABLE uqa_index_alpha.occupied_name(id integer);
SELECT pg_temp.index_namespace_probe('duplicate-index', 'postgres', 'pg_catalog', 'CREATE INDEX shared_idx ON uqa_index_alpha.items(id)');
SELECT pg_temp.index_namespace_probe('table-name-collision', 'postgres', 'pg_catalog', 'CREATE INDEX occupied_name ON uqa_index_alpha.items(id)');
SELECT pg_temp.index_namespace_probe('index-name-collision', 'postgres', 'pg_catalog', 'CREATE TABLE uqa_index_alpha.shared_idx(id integer)');
SELECT pg_temp.index_namespace_probe('duplicate-index-if-not-exists', 'postgres', 'pg_catalog', 'CREATE INDEX IF NOT EXISTS shared_idx ON uqa_index_alpha.items(id)');
SELECT pg_temp.index_namespace_probe('table-name-if-not-exists', 'postgres', 'pg_catalog', 'CREATE INDEX IF NOT EXISTS occupied_name ON uqa_index_alpha.items(id)');

CREATE INDEX ON uqa_index_alpha.items(id);
CREATE INDEX ON uqa_index_alpha.items(id);
CREATE INDEX ON uqa_index_beta.items(id);
SELECT 'default-names|' || string_agg(schemaname || '.' || indexname, ',' ORDER BY schemaname, indexname)
  FROM pg_indexes
 WHERE schemaname IN ('uqa_index_alpha', 'uqa_index_beta')
   AND indexname LIKE 'items_id_idx%';

SELECT pg_temp.index_namespace_probe('drop-search-path', 'postgres', 'uqa_index_beta,uqa_index_alpha,pg_catalog', 'DROP INDEX shared_idx');
SELECT 'drop-survivor|' || string_agg(schemaname || '.' || indexname, ',' ORDER BY schemaname)
  FROM pg_indexes
 WHERE schemaname IN ('uqa_index_alpha', 'uqa_index_beta')
   AND indexname = 'shared_idx';

CREATE TABLE uqa_index_alpha.shadow_idx(id integer);
CREATE INDEX shadow_idx ON uqa_index_beta.items(id);
SELECT pg_temp.index_namespace_probe('drop-wrong-kind-first', 'postgres', 'uqa_index_alpha,uqa_index_beta,pg_catalog', 'DROP INDEX shadow_idx');
SELECT pg_temp.index_namespace_probe('drop-missing-index', 'postgres', 'pg_catalog', 'DROP INDEX uqa_index_alpha.absent_idx');
SELECT pg_temp.index_namespace_probe('drop-missing-index-if-exists', 'postgres', 'pg_catalog', 'DROP INDEX IF EXISTS uqa_index_alpha.absent_idx');
SELECT pg_temp.index_namespace_probe('drop-missing-schema', 'postgres', 'pg_catalog', 'DROP INDEX uqa_index_absent.absent_idx');
SELECT pg_temp.index_namespace_probe('drop-missing-schema-if-exists', 'postgres', 'pg_catalog', 'DROP INDEX IF EXISTS uqa_index_absent.absent_idx');

CREATE TABLE uqa_index_hidden.items(id integer);
CREATE INDEX hidden_idx ON uqa_index_hidden.items(id);
SELECT pg_temp.index_namespace_probe('drop-hidden-qualified', 'uqa_index_caller', 'pg_catalog', 'DROP INDEX uqa_index_hidden.hidden_idx');
SELECT pg_temp.index_namespace_probe('drop-hidden-if-exists', 'uqa_index_caller', 'pg_catalog', 'DROP INDEX IF EXISTS uqa_index_hidden.absent_idx');
GRANT USAGE, CREATE ON SCHEMA uqa_index_alpha TO uqa_index_caller;
SET ROLE uqa_index_caller;
CREATE TABLE uqa_index_alpha.caller_items(id integer);
CREATE INDEX visible_drop_idx ON uqa_index_alpha.caller_items(id);
RESET ROLE;
CREATE INDEX visible_drop_idx ON uqa_index_hidden.items(id);
SELECT pg_temp.index_namespace_probe('drop-skips-hidden-schema', 'uqa_index_caller', 'uqa_index_hidden,uqa_index_alpha,pg_catalog', 'DROP INDEX visible_drop_idx');
SELECT 'hidden-survivor|' || string_agg(schemaname || '.' || indexname, ',' ORDER BY schemaname)
  FROM pg_indexes
 WHERE indexname = 'visible_drop_idx';

CREATE TABLE "uqa_index.dot".items(id integer);
CREATE INDEX "shared.dot" ON "uqa_index.dot".items(id);
SELECT 'quoted-identity|' || '"uqa_index.dot"."shared.dot"'::regclass::text;

DROP SCHEMA uqa_index_alpha CASCADE;
DROP SCHEMA uqa_index_beta CASCADE;
DROP SCHEMA uqa_index_hidden CASCADE;
DROP SCHEMA "uqa_index.dot" CASCADE;
DROP OWNED BY uqa_index_caller;
DROP ROLE uqa_index_caller;
