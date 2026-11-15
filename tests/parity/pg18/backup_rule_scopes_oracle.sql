-- PostgreSQL 18 reference for stored DISTINCT ON aliases and quoted relation scopes.
CREATE TABLE items(id integer);
CREATE TABLE target(a integer, b integer);
CREATE TABLE outer_items(id integer);
CREATE TABLE "a.b"(id integer);
CREATE TABLE "My Table"(id integer);
CREATE TABLE "a""b"(id integer);
CREATE TABLE scope_log(kind text, value integer);
INSERT INTO target VALUES(1,11),(1,12),(2,12);
INSERT INTO outer_items VALUES(5),(0);
INSERT INTO "a.b" VALUES(7);
INSERT INTO "My Table" VALUES(8);
INSERT INTO "a""b" VALUES(9);
CREATE RULE aliased AS ON INSERT TO items DO ALSO INSERT INTO scope_log SELECT 'alias', b FROM (SELECT DISTINCT ON (b) a AS b FROM target ORDER BY b) AS chosen;
CREATE RULE qualified AS ON INSERT TO items DO ALSO INSERT INTO scope_log SELECT 'input', b FROM (SELECT DISTINCT ON (target.b) a AS b FROM target ORDER BY target.b, a) AS chosen;
CREATE RULE correlated AS ON INSERT TO items DO ALSO INSERT INTO scope_log SELECT 'correlated', 42 FROM outer_items AS b WHERE EXISTS(SELECT 1 FROM "a.b" WHERE b.id>0);
CREATE RULE dotted AS ON INSERT TO items DO ALSO INSERT INTO scope_log SELECT 'dotted', "a.b".id FROM "a.b";
CREATE RULE spaced AS ON INSERT TO items DO ALSO INSERT INTO scope_log SELECT 'spaced', "My Table".id FROM "My Table";
CREATE RULE escaped AS ON INSERT TO items DO ALSO INSERT INTO scope_log SELECT 'escaped', "a""b".id FROM "a""b";
-- capture boundary
INSERT INTO items VALUES(1);
SELECT string_agg(kind || ':' || value::text, E'\n' ORDER BY kind, value) AS observations FROM scope_log;
SELECT string_agg(reference, E'\n' ORDER BY reference)
FROM (
    SELECT DISTINCT r.rulename || ':' || c.relname || '.' || a.attname AS reference
    FROM pg_depend d
    JOIN pg_rewrite r ON d.classid='pg_rewrite'::regclass AND d.objid=r.oid
    JOIN pg_class owner ON owner.oid=r.ev_class
    JOIN pg_namespace owner_ns ON owner_ns.oid=owner.relnamespace
    JOIN pg_class c ON d.refclassid='pg_class'::regclass AND d.refobjid=c.oid
    JOIN pg_attribute a ON a.attrelid=c.oid AND a.attnum=d.refobjsubid
    WHERE owner_ns.nspname='public' AND owner.relname='items' AND d.deptype='n'
      AND d.refobjsubid>0 AND c.relname NOT IN ('items','scope_log')
) entries;
