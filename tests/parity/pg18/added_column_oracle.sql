\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned
SET client_min_messages = warning;
BEGIN;

CREATE FUNCTION pg_temp.added_column_state(command text) RETURNS text LANGUAGE plpgsql AS $oracle$
DECLARE
    primary_message text;
    exception_detail text;
BEGIN
    EXECUTE command;
    RETURN '00000';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS primary_message = MESSAGE_TEXT, exception_detail = PG_EXCEPTION_DETAIL;
    RETURN SQLSTATE || '|' || primary_message || '|' || exception_detail;
END
$oracle$;

CREATE TEMP TABLE left_t (id INTEGER PRIMARY KEY, v INTEGER);
INSERT INTO left_t VALUES (1, 1);
ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT 'key1';
SELECT 'added|' || id || '|' || v || '|' || k FROM left_t;
SELECT 'duplicate-insert|' || pg_temp.added_column_state('INSERT INTO left_t (id, v) VALUES (2, 2)');
INSERT INTO left_t VALUES (2, 2, 'key2');
SELECT 'stored|' || id || '|' || k FROM left_t ORDER BY id;

DROP TABLE left_t;
CREATE TEMP TABLE left_t (id INTEGER PRIMARY KEY, v INTEGER);
INSERT INTO left_t VALUES (1, 1), (2, 2);
SELECT 'failed-add|' || pg_temp.added_column_state('ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT ''key1''');
SELECT 'columns-after-failure|' || string_agg(attname, ',' ORDER BY attnum)
FROM pg_attribute WHERE attrelid = 'left_t'::regclass AND attnum > 0 AND NOT attisdropped;
ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE;
SELECT 'nulls-after-failure|' || count(*) FROM left_t WHERE k IS NULL;

DROP TABLE left_t;
CREATE TEMP TABLE left_t (id INTEGER PRIMARY KEY, v INTEGER);
INSERT INTO left_t VALUES (1, 1);
SAVEPOINT before_column;
ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT 'key1';
ROLLBACK TO before_column;
SELECT 'columns-after-rollback|' || string_agg(attname, ',' ORDER BY attnum)
FROM pg_attribute WHERE attrelid = 'left_t'::regclass AND attnum > 0 AND NOT attisdropped;
ALTER TABLE left_t ADD COLUMN k TEXT UNIQUE DEFAULT 'replacement';
SELECT 'after-rollback|' || k FROM left_t;

CREATE TEMP TABLE labels (id INTEGER PRIMARY KEY, tenant TEXT, slug TEXT);
INSERT INTO labels VALUES (1, 'a', 'one'), (2, 'a', 'one');
SELECT 'failed-constraint|' || pg_temp.added_column_state('ALTER TABLE labels ADD CONSTRAINT labels_tenant_slug_key UNIQUE (tenant, slug)');
SELECT 'failed-unnamed-constraint|' || pg_temp.added_column_state('ALTER TABLE labels ADD UNIQUE (tenant, slug)');
SELECT 'keys-after-constraint-failure|' || string_agg(conname, ',' ORDER BY conname)
FROM pg_constraint WHERE conrelid = 'labels'::regclass AND contype IN ('p', 'u');
ROLLBACK;
