-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Independent PostgreSQL 18 reference for physical nullable JOIN metadata.
SELECT version();
SELECT l.id::text || ':' || coalesce(r::text,'ABSENT') AS result FROM (VALUES(1),(2)) AS l(id) LEFT JOIN (VALUES(NULL::integer,NULL::text)) AS r(n,v) ON l.id=1 ORDER BY l.id;
SELECT a.id::text || ':' || coalesce(b::text,'ABSENT') || ':' || coalesce(c::text,'ABSENT') AS result FROM (VALUES(1),(2),(3)) AS a(id) LEFT JOIN (VALUES(2)) AS b(id) ON a.id=b.id LEFT JOIN (VALUES(NULL::integer)) AS c(n) ON a.id<>3 ORDER BY a.id;
SELECT id::text || ':' || coalesce(l::text,'ABSENT') || ':' || coalesce(r::text,'ABSENT') AS result FROM (VALUES(1::integer),(2)) AS l(id) FULL JOIN (VALUES(2::bigint),(3)) AS r(id) USING(id) ORDER BY id;
SELECT j.x::text || ':' || j::text AS result FROM ((VALUES(1),(2)) AS l(id) LEFT JOIN (VALUES(1,NULL::text)) AS r(id,v) ON l.id=r.id) AS j(x,y,z) ORDER BY j.x;
SELECT l.id::text || ':' || coalesce(r::text,'ABSENT') AS result FROM (VALUES(1),(2)) AS l(id) LEFT JOIN LATERAL (SELECT NULL::integer AS n WHERE l.id=1) AS r ON true ORDER BY l.id;
SELECT l.id::text || ':' || coalesce(r::text,'ABSENT') AS result FROM (VALUES(1),(2)) AS l(id) LEFT JOIN LATERAL (SELECT NULL::integer AS n) AS r ON l.id=1 ORDER BY l.id;
