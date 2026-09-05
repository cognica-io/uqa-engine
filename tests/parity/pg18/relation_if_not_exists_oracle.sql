\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_relation_ifne CASCADE;
DROP SERVER IF EXISTS uqa_relation_ifne_server CASCADE;
DROP FOREIGN DATA WRAPPER IF EXISTS uqa_relation_ifne_fdw CASCADE;

CREATE SCHEMA uqa_relation_ifne;
CREATE FOREIGN DATA WRAPPER uqa_relation_ifne_fdw;
CREATE SERVER uqa_relation_ifne_server FOREIGN DATA WRAPPER uqa_relation_ifne_fdw;
CREATE TABLE uqa_relation_ifne.existing_table(marker integer);
CREATE TABLE uqa_relation_ifne.existing_serial_table(marker integer);
CREATE TABLE uqa_relation_ifne.existing_foreign_target(marker integer);
CREATE TABLE uqa_relation_ifne.existing_foreign_serial_target(marker integer);
CREATE SEQUENCE uqa_relation_ifne.existing_sequence;

CREATE OR REPLACE FUNCTION pg_temp.relation_ifne_probe(label text, command text)
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

SELECT pg_temp.relation_ifne_probe('table-existing-missing-type', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.existing_table(id uqa_relation_ifne.missing_type)');
SELECT pg_temp.relation_ifne_probe('table-existing-duplicate-column', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.existing_table(id integer, id text)');
SELECT pg_temp.relation_ifne_probe('table-existing-multiple-primary-keys', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.existing_table(id integer PRIMARY KEY, alternate integer PRIMARY KEY)');
SELECT pg_temp.relation_ifne_probe('table-existing-invalid-check', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.existing_table(id integer CHECK (missing_column > 0))');
SELECT pg_temp.relation_ifne_probe('table-existing-missing-parent', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.existing_table(id integer) INHERITS (uqa_relation_ifne.missing_parent)');
SELECT pg_temp.relation_ifne_probe('table-existing-missing-typed-row', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.existing_table OF uqa_relation_ifne.missing_type');
SELECT pg_temp.relation_ifne_probe('table-existing-missing-tablespace', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.existing_table(id integer) TABLESPACE uqa_relation_ifne_missing_tablespace');
SELECT pg_temp.relation_ifne_probe('table-existing-cross-kind', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.existing_sequence(id uqa_relation_ifne.missing_type)');
SELECT pg_temp.relation_ifne_probe('table-existing-serial', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.existing_serial_table(id serial)');
SELECT 'table-existing-serial-side-effect|' || (to_regclass('uqa_relation_ifne.existing_serial_table_id_seq') IS NULL)::text;
SELECT pg_temp.relation_ifne_probe('table-absent-missing-type', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.absent_type(id uqa_relation_ifne.missing_type)');
SELECT pg_temp.relation_ifne_probe('table-absent-duplicate-column', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.absent_duplicate(id integer, id text)');
SELECT pg_temp.relation_ifne_probe('table-absent-multiple-primary-keys', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.absent_primary_keys(id integer PRIMARY KEY, alternate integer PRIMARY KEY)');
SELECT pg_temp.relation_ifne_probe('table-absent-invalid-check', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.absent_check(id integer CHECK (missing_column > 0))');
SELECT pg_temp.relation_ifne_probe('table-absent-missing-parent', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.absent_parent(id integer) INHERITS (uqa_relation_ifne.missing_parent)');
SELECT pg_temp.relation_ifne_probe('table-absent-missing-typed-row', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.absent_typed OF uqa_relation_ifne.missing_type');
SELECT pg_temp.relation_ifne_probe('table-absent-valid', 'CREATE TABLE IF NOT EXISTS uqa_relation_ifne.created_table(id integer)');

SELECT pg_temp.relation_ifne_probe('foreign-existing-primary-key', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.existing_foreign_target(id integer PRIMARY KEY) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-existing-unique', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.existing_foreign_target(id integer UNIQUE) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-existing-foreign-key', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.existing_foreign_target(id integer REFERENCES uqa_relation_ifne.missing_parent(id)) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-existing-missing-type', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.existing_foreign_target(id uqa_relation_ifne.missing_type) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-existing-duplicate-column', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.existing_foreign_target(id integer, id text) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-existing-invalid-check', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.existing_foreign_target(id integer CHECK (missing_column > 0)) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-existing-missing-server', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.existing_foreign_target(id integer) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-existing-cross-kind', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.existing_sequence(id integer PRIMARY KEY) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-existing-serial', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.existing_foreign_serial_target(id serial) SERVER uqa_relation_ifne_missing_server');
SELECT 'foreign-existing-serial-side-effect|' || (to_regclass('uqa_relation_ifne.existing_foreign_serial_target_id_seq') IS NULL)::text;
SELECT pg_temp.relation_ifne_probe('foreign-absent-primary-key', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.absent_foreign_primary_key(id integer PRIMARY KEY) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-absent-unique', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.absent_foreign_unique(id integer UNIQUE) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-absent-foreign-key', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.absent_foreign_key(id integer REFERENCES uqa_relation_ifne.missing_parent(id)) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-absent-missing-type', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.absent_foreign_type(id uqa_relation_ifne.missing_type) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-absent-duplicate-column', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.absent_foreign_duplicate(id integer, id text) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-absent-invalid-check', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.absent_foreign_check(id integer CHECK (missing_column > 0)) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-absent-missing-server', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.absent_foreign_server(id integer) SERVER uqa_relation_ifne_missing_server');
SELECT pg_temp.relation_ifne_probe('foreign-absent-valid', 'CREATE FOREIGN TABLE IF NOT EXISTS uqa_relation_ifne.created_foreign_table(id integer) SERVER uqa_relation_ifne_server');

DROP SCHEMA uqa_relation_ifne CASCADE;
DROP SERVER uqa_relation_ifne_server CASCADE;
DROP FOREIGN DATA WRAPPER uqa_relation_ifne_fdw CASCADE;
