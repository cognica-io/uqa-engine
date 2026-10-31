-- PostgreSQL 18 source descriptor names, row types, collations and catalog arrays.
SELECT version();
BEGIN;
CREATE SCHEMA source_schema_owner; SET search_path=source_schema_owner,pg_catalog; CREATE TABLE items(id integer,v text COLLATE "C"); INSERT INTO items VALUES(1,'alpha'),(2,'beta'); CREATE VIEW items_view AS SELECT id,v FROM items; CREATE MATERIALIZED VIEW items_mv AS SELECT id,v FROM items; CREATE SEQUENCE items_seq;
SELECT pg_typeof(t)::text||':'||t::text AS result FROM items AS t(k,label) ORDER BY t.k;
SELECT pg_typeof(v)::text||':'||v::text AS result FROM items_view AS v(k,label) ORDER BY v.k;
SELECT pg_typeof(m)::text||':'||m::text AS result FROM items_mv AS m(k,label) ORDER BY m.k;
WITH c AS (SELECT id,v FROM items) SELECT c.k::text||':'||c.label||':'||pg_collation_for(c.label) AS result FROM c AS c(k,label) ORDER BY k;
SELECT s::text||':'||pg_typeof(s.n)::text AS result FROM (SELECT 1::bigint AS id,'x'::text AS v) AS s(n,label);
SELECT last_value::text||':'||is_called::text||':'||pg_typeof(last_value)::text AS result FROM items_seq;
SELECT pg_typeof(relname)::text||':'||pg_typeof(relacl)::text AS result FROM pg_catalog.pg_class WHERE relname='items' AND relnamespace='source_schema_owner'::regnamespace;
SELECT pg_typeof(table_name)::text||':'||column_name::text AS result FROM information_schema.columns WHERE table_schema='source_schema_owner' AND table_name='items' ORDER BY ordinal_position;
ROLLBACK;
