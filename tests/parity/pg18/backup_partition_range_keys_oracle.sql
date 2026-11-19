-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- PostgreSQL 18.4, image sha256:9a8afca54e7861fd90fab5fdf4c42477a6b1cb7d293595148e674e0a3181de15.
\set ON_ERROR_STOP on
\pset format unaligned
\pset tuples_only on
BEGIN;
SET LOCAL client_min_messages = warning;
CREATE SCHEMA uqa_backup_range_key_reference;
SET LOCAL search_path = uqa_backup_range_key_reference, public;
CREATE TEMP TABLE observations (kind text, key_type text, targets text);
DO $verify$
DECLARE key_type text; small text; middle text; large text; targets text; left_value text; right_value text;
BEGIN
    FOREACH key_type IN ARRAY ARRAY['int4range','int4multirange','numrange','nummultirange'] LOOP
        small := '[2,3)'; middle := '[5,6)'; large := '[10,11)';
        IF key_type LIKE '%multirange' THEN
            small := '{' || small || '}'; middle := '{' || middle || '}'; large := '{' || large || '}';
        END IF;
        EXECUTE format('CREATE TABLE parent(id integer,value %s) PARTITION BY RANGE(value)',key_type);
        BEGIN
            EXECUTE format('CREATE TABLE invalid PARTITION OF parent FOR VALUES FROM (%L) TO (%L)',large,small);
            RAISE EXCEPTION 'inverted interval accepted';
        EXCEPTION WHEN invalid_object_definition THEN NULL;
        END;
        EXECUTE format('CREATE TABLE inside PARTITION OF parent FOR VALUES FROM (%L) TO (%L)',small,large);
        CREATE TABLE outside PARTITION OF parent DEFAULT;
        EXECUTE format('INSERT INTO parent VALUES(1,%L),(2,%L),(3,%L)',small,middle,large);
        EXECUTE 'SELECT string_agg(tableoid::regclass::text,'','' ORDER BY id) FROM parent' INTO targets;
        INSERT INTO observations VALUES ('range',key_type,targets);
        BEGIN
            EXECUTE format('CREATE TABLE invalid PARTITION OF parent FOR VALUES FROM (%L) TO (%L)',middle,large);
            RAISE EXCEPTION 'overlapping interval accepted';
        EXCEPTION WHEN invalid_object_definition THEN NULL;
        END;
        DROP TABLE parent CASCADE;
    END LOOP;
    FOREACH key_type IN ARRAY ARRAY['numrange','nummultirange'] LOOP
        left_value := '[1.0,3.00)'; right_value := '[1.00,3.0)';
        IF key_type LIKE '%multirange' THEN
            left_value := '{' || left_value || '}'; right_value := '{' || right_value || '}';
        END IF;
        EXECUTE format('CREATE TABLE parent(id integer,value %s) PARTITION BY LIST(value)',key_type);
        EXECUTE format('CREATE TABLE inside PARTITION OF parent FOR VALUES IN(%L,%L)',left_value,right_value);
        CREATE TABLE outside PARTITION OF parent DEFAULT;
        EXECUTE format('INSERT INTO parent VALUES(1,%L),(2,%L)',left_value,right_value);
        EXECUTE 'SELECT string_agg(tableoid::regclass::text,'','' ORDER BY id) FROM parent' INTO targets;
        INSERT INTO observations VALUES ('list',key_type,targets);
        BEGIN
            EXECUTE format('CREATE TABLE invalid PARTITION OF parent FOR VALUES IN(%L)',right_value);
            RAISE EXCEPTION 'equivalent LIST value accepted';
        EXCEPTION WHEN invalid_object_definition THEN NULL;
        END;
        DROP TABLE parent CASCADE;
    END LOOP;
END;
$verify$;
SELECT kind,key_type,targets FROM observations ORDER BY kind,key_type;
ROLLBACK;
