\set VERBOSITY sqlstate
SELECT version();
\echo row_keys
SELECT coalesce(l.id::text,'_') || ':' || coalesce(r.id::text,'_') AS result FROM (VALUES(1,NULL::text),(2,'b'),(3,'c')) AS l(id,label) FULL JOIN (VALUES(1,NULL::text),(2,'b'),(4,'d')) AS r(id,label) ON ROW(r.id,r.label)=ROW(l.id,l.label) ORDER BY l.id NULLS LAST,r.id NULLS LAST;
\echo using_types
SELECT id::text || ':' || pg_typeof(id)::text AS result FROM (VALUES(1::integer),(2)) AS l(id) FULL JOIN (VALUES(2::bigint),(3)) AS r(id) USING(id) ORDER BY id;
\echo lateral_scope
SELECT l.id::text || ':' || coalesce(r.n::text,'_') AS result FROM (VALUES(1),(2)) AS l(id) LEFT JOIN LATERAL (SELECT l.id+1 AS n) AS r ON r.n>2 ORDER BY l.id;
\echo pattern
SELECT l.v AS result FROM (VALUES('a_x'),('abx')) AS l(v) JOIN (VALUES('a%')) AS r(p) ON l.v LIKE r.p AND l.v LIKE 'a#_%' ESCAPE '#' ORDER BY l.v;
\echo membership
SELECT l.id::text AS result FROM (VALUES(1),(2),(3)) AS l(id) JOIN (VALUES(1),(2),(3)) AS r(id) ON l.id=r.id AND l.id=ANY(ARRAY[1,NULL,3]) ORDER BY l.id;
\echo datetime
SELECT coalesce(l.d::text,'_') || ':' || coalesce(r.t::date::text,'_') AS result FROM (VALUES(DATE '2024-01-01'),(DATE '2024-01-02')) AS l(d) FULL JOIN (VALUES(TIMESTAMP '2024-01-02 00:00:00'),(TIMESTAMP '2024-01-03 00:00:00')) AS r(t) ON l.d=r.t ORDER BY l.d NULLS LAST,r.t NULLS LAST;
\echo nonjoinable_full
SELECT l.id FROM (VALUES(1)) AS l(id) FULL JOIN (VALUES(2)) AS r(id) ON l.id>r.id;
\echo narrowing_cast
SELECT l.id FROM (VALUES(32768)) AS l(id) JOIN (VALUES(1::smallint)) AS r(id) ON l.id::smallint=r.id;
