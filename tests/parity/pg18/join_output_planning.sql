-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Independent PostgreSQL 18 reference for JOIN output coercion and namespaces.
SELECT version();
SELECT merged.id::text || ':' || pg_typeof(merged.id)::text || ':' || coalesce(l.id::text,'_') || ':' || coalesce(r.id::text,'_') AS result FROM (VALUES(1::integer),(2)) AS l(id) FULL JOIN (VALUES(2::bigint),(3)) AS r(id) USING(id) AS merged ORDER BY merged.id;
SELECT id::text || ':' || pg_typeof(id)::text || ':' || label || ':' || extra AS result FROM (VALUES(1::smallint,'a'),(2,'b')) AS l(id,label) NATURAL RIGHT JOIN (VALUES(2::bigint,'x'),(3,'y')) AS r(id,extra) WHERE label IS NOT NULL ORDER BY id;
SELECT j::text || ':' || pg_typeof(j.k)::text AS result FROM ((VALUES(1::integer,'a'),(2,'b')) AS l(id,label) FULL JOIN (VALUES(2::bigint,'x'),(3,'y')) AS r(id,extra) USING(id)) AS j(k,v,w) ORDER BY j.k;
SELECT j.x::text || ':' || j.y::text AS result FROM ((VALUES(2),(1)) AS l(id) CROSS JOIN (VALUES(7)) AS r(id)) AS j(x,y) ORDER BY j.x;
SELECT merged.id::text || ':' || r.n::text AS result FROM (VALUES(1::integer),(2)) AS l(id) LEFT JOIN LATERAL (SELECT l.id::bigint AS id,l.id+10 AS n) AS r USING(id) AS merged ORDER BY merged.id;

SELECT k::text || ':' || coalesce(j::text,'ABSENT') AS result FROM ((VALUES(1::integer,'a'),(2,'b')) AS l(id,label) FULL JOIN (VALUES(2::bigint,'x'),(3,'y')) AS r(id,extra) USING(id)) AS j(k,v,w) FULL JOIN (VALUES(2::bigint),(4)) AS t(k) USING(k) ORDER BY k;
SELECT ROW(j.*)::text AS result FROM ((VALUES(1::integer,'a'),(2,'b')) AS l(id,label) FULL JOIN (VALUES(2::bigint,'x'),(3,'y')) AS r(id,extra) USING(id)) AS j(k,v,w) ORDER BY j.k;
