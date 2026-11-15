-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Captured with PostgreSQL 18.4; image sha256:9a8afca54e7861fd90fab5fdf4c42477a6b1cb7d293595148e674e0a3181de15.
\set ON_ERROR_STOP on
\pset format unaligned
\pset tuples_only on
BEGIN;
SET LOCAL client_min_messages = warning;
CREATE SCHEMA uqa_backup_partition_reference;
SET LOCAL search_path = uqa_backup_partition_reference, public;
CREATE TYPE temperature AS ENUM ('cold', 'warm', 'hot');
CREATE TABLE enum_parent (id integer, label temperature) PARTITION BY RANGE (label);
CREATE TABLE enum_low PARTITION OF enum_parent FOR VALUES FROM (MINVALUE) TO ('warm');
CREATE TABLE enum_other PARTITION OF enum_parent DEFAULT;
INSERT INTO enum_parent VALUES (1, 'cold'), (2, 'warm'), (3, 'hot');
ALTER TYPE temperature RENAME VALUE 'warm' TO 'mild';
SELECT 'enum-bound', pg_get_expr(relpartbound, oid) FROM pg_class WHERE oid = 'enum_low'::regclass;
SELECT 'enum-row', id, label, tableoid::regclass FROM enum_parent ORDER BY id;
CREATE DOMAIN positive_key AS integer CHECK (VALUE > 0);
CREATE TABLE domain_parent (id positive_key) PARTITION BY RANGE (id);
CREATE TABLE domain_low PARTITION OF domain_parent FOR VALUES FROM (1) TO (5);
CREATE TABLE domain_other PARTITION OF domain_parent DEFAULT;
INSERT INTO domain_parent VALUES (2);
ALTER DOMAIN positive_key ADD CONSTRAINT later_minimum CHECK (VALUE > 10) NOT VALID;
INSERT INTO domain_parent VALUES (20);
SELECT 'domain-bound', pg_get_expr(relpartbound, oid) FROM pg_class WHERE oid = 'domain_low'::regclass;
SELECT 'domain-row', id, tableoid::regclass FROM domain_parent ORDER BY id;
CREATE FUNCTION original_key(integer) RETURNS integer LANGUAGE sql IMMUTABLE RETURN $1;
CREATE TABLE function_parent (id integer) PARTITION BY RANGE (original_key(id));
CREATE TABLE function_low PARTITION OF function_parent FOR VALUES FROM (0) TO (10);
CREATE TABLE function_other PARTITION OF function_parent DEFAULT;
ALTER FUNCTION original_key(integer) VOLATILE;
INSERT INTO function_parent VALUES (4), (12);
SELECT 'function-volatility', provolatile FROM pg_proc WHERE oid = 'original_key(integer)'::regprocedure;
SELECT 'function-row', id, tableoid::regclass FROM function_parent ORDER BY id;
CREATE DOMAIN unsupported_key AS json;
DO $verify$
DECLARE key_type text; strategy text;
BEGIN
    FOREACH key_type IN ARRAY ARRAY['json','xml','unsupported_key'] LOOP
        FOREACH strategy IN ARRAY ARRAY['LIST','RANGE'] LOOP
            BEGIN
                EXECUTE format('CREATE TABLE rejected_key(id %s) PARTITION BY %s(id)',key_type,strategy);
                RAISE EXCEPTION 'partition declaration unexpectedly accepted';
            EXCEPTION WHEN undefined_object THEN
                IF SQLSTATE <> '42704' THEN RAISE; END IF;
            END;
        END LOOP;
    END LOOP;
    BEGIN
        EXECUTE 'CREATE TABLE rejected_key(id json,n integer) PARTITION BY RANGE(id,(random()+n))';
        RAISE EXCEPTION 'partition declaration unexpectedly accepted';
    EXCEPTION WHEN undefined_object THEN NULL;
    END;
    BEGIN
        EXECUTE 'CREATE TABLE rejected_key(id json,n integer) PARTITION BY RANGE((random()+n),id)';
        RAISE EXCEPTION 'partition declaration unexpectedly accepted';
    EXCEPTION WHEN invalid_object_definition THEN NULL;
    END;
END;
$verify$;
ROLLBACK;
