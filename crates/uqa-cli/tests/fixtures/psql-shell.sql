-- Independently executed with PostgreSQL 18.4 psql (Docker, 2026-10-17).
\timing off
\timing off
\x auto
\x off
\pset format csv
SELECT 1 AS x, 2 AS x;
SELECT '' AS empty, NULL AS null;
\set value 41
SELECT :value + 1 AS answer \gset r_
\echo :r_answer
\if :{?r_answer}
\echo active
\else
\echo inactive
\endif
\set n ''
\echo :{?n}
\unset n
\echo :{?n}
\set AUTOCOMMIT off
CREATE TEMP TABLE client_txn(value int);
INSERT INTO client_txn VALUES (1);
ROLLBACK;
SELECT to_regclass('client_txn') IS NULL AS rolled_back;
