CREATE TABLE predicate_items (id integer PRIMARY KEY, v integer NOT NULL, body text NOT NULL);
INSERT INTO predicate_items SELECT i, 0, repeat('x', 2000) FROM generate_series(1, 100) AS ids(i);
CREATE SEQUENCE predicate_ticks;
PREPARE predicate_update(jsonb, integer) AS
UPDATE predicate_items AS target SET v=target.v+$2
FROM jsonb_array_elements($1) AS source(value)
WHERE target.id=(source.value->>'id')::integer
  AND nextval('predicate_ticks')>0;
EXECUTE predicate_update('[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5}]', 7);
SELECT count(*), sum(v), currval('predicate_ticks') FROM predicate_items;
BEGIN;
EXECUTE predicate_update('[{"id":1},{"id":2}]', 11);
SELECT sum(v) FROM predicate_items;
ROLLBACK;
SELECT count(*), sum(v), currval('predicate_ticks') FROM predicate_items;
EXECUTE predicate_update('[]', 99);
SELECT count(*), sum(v), currval('predicate_ticks') FROM predicate_items;
UPDATE predicate_items AS target SET v=target.v+source.delta
FROM (VALUES (1,2),(3,4),(NULL::integer,99)) AS source(id,delta)
WHERE target.id=source.id AND CASE WHEN source.id=1 THEN true ELSE 12/(source.id-1)>0 END;
SELECT id,v FROM predicate_items WHERE v<>0 ORDER BY id;
