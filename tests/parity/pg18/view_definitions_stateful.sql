-- PostgreSQL 18.4 view-definition reconstruction and catalog lifecycle.

-- @case create_schema ok
CREATE SCHEMA __UQA_STATEFUL_SCHEMA__;
-- @end

-- @case create_t ok
CREATE TABLE t(a int, b text);
-- @end

-- @case create_u ok
CREATE TABLE u(a int, c int);
-- @end

-- @case populate_t ok
INSERT INTO t VALUES (1,'one'),(2,'two'),(3,NULL);
-- @end

-- @case populate_u ok
INSERT INTO u VALUES (1,10),(2,20),(4,40);
-- @end

-- @case create_simple ok
CREATE VIEW simple AS SELECT t.a AS a,t.b AS x,1 AS one,NULL::int AS n,-1 AS neg,a*2+3 AS calc FROM t;
-- @end

-- @case definition_simple rows
SELECT pg_get_viewdef('simple'::regclass) AS definition,pg_get_viewdef('simple'::regclass,true) AS pretty,pg_get_viewdef('simple'::regclass,40) AS wrapped;
-- @end

-- @case create_filter ok
CREATE VIEW filter AS SELECT a,b,a+1 AS next_a,'hi' AS greeting FROM t WHERE a>0 AND b IS NOT NULL ORDER BY a DESC;
-- @end

-- @case definition_filter rows
SELECT pg_get_viewdef('filter'::regclass) AS definition,pg_get_viewdef('filter'::regclass,true) AS pretty,pg_get_viewdef('filter'::regclass,40) AS wrapped;
-- @end

-- @case create_joined ok
CREATE VIEW joined AS SELECT t.a,t.b,u.c FROM t JOIN u ON t.a=u.a WHERE u.c>0;
-- @end

-- @case definition_joined rows
SELECT pg_get_viewdef('joined'::regclass) AS definition,pg_get_viewdef('joined'::regclass,true) AS pretty,pg_get_viewdef('joined'::regclass,40) AS wrapped;
-- @end

-- @case create_using_join ok
CREATE VIEW using_join AS SELECT * FROM t JOIN u USING(a);
-- @end

-- @case definition_using_join rows
SELECT pg_get_viewdef('using_join'::regclass) AS definition,pg_get_viewdef('using_join'::regclass,true) AS pretty,pg_get_viewdef('using_join'::regclass,40) AS wrapped;
-- @end

-- @case create_full_join ok
CREATE VIEW full_join AS SELECT * FROM t FULL JOIN u USING(a);
-- @end

-- @case definition_full_join rows
SELECT pg_get_viewdef('full_join'::regclass) AS definition,pg_get_viewdef('full_join'::regclass,true) AS pretty,pg_get_viewdef('full_join'::regclass,40) AS wrapped;
-- @end

-- @case create_cte ok
CREATE VIEW cte AS WITH x(k) AS (SELECT a FROM t WHERE a>0) SELECT k FROM x;
-- @end

-- @case definition_cte rows
SELECT pg_get_viewdef('cte'::regclass) AS definition,pg_get_viewdef('cte'::regclass,true) AS pretty,pg_get_viewdef('cte'::regclass,40) AS wrapped;
-- @end

-- @case create_sub ok
CREATE VIEW sub AS SELECT q.a,(SELECT max(u.c) FROM u WHERE u.a=q.a) AS c FROM (SELECT a FROM t) q WHERE EXISTS (SELECT FROM u WHERE u.a=q.a);
-- @end

-- @case definition_sub rows
SELECT pg_get_viewdef('sub'::regclass) AS definition,pg_get_viewdef('sub'::regclass,true) AS pretty,pg_get_viewdef('sub'::regclass,40) AS wrapped;
-- @end

-- @case create_values_v ok
CREATE VIEW values_v AS VALUES (1,'x'),(2,'y');
-- @end

-- @case definition_values_v rows
SELECT pg_get_viewdef('values_v'::regclass) AS definition,pg_get_viewdef('values_v'::regclass,true) AS pretty,pg_get_viewdef('values_v'::regclass,40) AS wrapped;
-- @end

-- @case create_union_v ok
CREATE VIEW union_v AS SELECT a FROM t UNION ALL SELECT a FROM u ORDER BY a LIMIT 2 OFFSET 1;
-- @end

-- @case definition_union_v rows
SELECT pg_get_viewdef('union_v'::regclass) AS definition,pg_get_viewdef('union_v'::regclass,true) AS pretty,pg_get_viewdef('union_v'::regclass,40) AS wrapped;
-- @end

-- @case create_group_v ok
CREATE VIEW group_v AS SELECT a,count(*) AS n,sum(a) AS total FROM t GROUP BY a HAVING count(*)>1 ORDER BY a;
-- @end

-- @case definition_group_v rows
SELECT pg_get_viewdef('group_v'::regclass) AS definition,pg_get_viewdef('group_v'::regclass,true) AS pretty,pg_get_viewdef('group_v'::regclass,40) AS wrapped;
-- @end

-- @case create_funcs ok
CREATE VIEW funcs AS SELECT coalesce(b,'x') AS text,a BETWEEN 1 AND 4 AS range,a IN (1,2) AS any,a NOT IN (1,2) AS no,a::text AS casted,ARRAY[a,2] AS arr,CASE WHEN a>0 THEN 'positive' ELSE 'other' END AS label FROM t;
-- @end

-- @case definition_funcs rows
SELECT pg_get_viewdef('funcs'::regclass) AS definition,pg_get_viewdef('funcs'::regclass,true) AS pretty,pg_get_viewdef('funcs'::regclass,40) AS wrapped;
-- @end

-- @case create_precedence ok
CREATE VIEW precedence AS SELECT (a+1)*2 AS x,a-(a-1) AS y,NOT (a>0 OR a<10) AS z FROM t;
-- @end

-- @case definition_precedence rows
SELECT pg_get_viewdef('precedence'::regclass) AS definition,pg_get_viewdef('precedence'::regclass,true) AS pretty,pg_get_viewdef('precedence'::regclass,40) AS wrapped;
-- @end

-- @case create_aliases ok
CREATE VIEW aliases AS SELECT x.id AS key,x.label FROM t AS x(id,label);
-- @end

-- @case definition_aliases rows
SELECT pg_get_viewdef('aliases'::regclass) AS definition,pg_get_viewdef('aliases'::regclass,true) AS pretty,pg_get_viewdef('aliases'::regclass,40) AS wrapped;
-- @end

-- @case create_distinct_v ok
CREATE VIEW distinct_v AS SELECT DISTINCT ON (a) a,b FROM t ORDER BY a,b DESC NULLS LAST;
-- @end

-- @case definition_distinct_v rows
SELECT pg_get_viewdef('distinct_v'::regclass) AS definition,pg_get_viewdef('distinct_v'::regclass,true) AS pretty,pg_get_viewdef('distinct_v'::regclass,40) AS wrapped;
-- @end

-- @case create_window_v ok
CREATE VIEW window_v AS SELECT a,row_number() OVER (ORDER BY a) AS rn,sum(a) OVER (PARTITION BY b ORDER BY a ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) AS total FROM t;
-- @end

-- @case definition_window_v rows
SELECT pg_get_viewdef('window_v'::regclass) AS definition,pg_get_viewdef('window_v'::regclass,true) AS pretty,pg_get_viewdef('window_v'::regclass,40) AS wrapped;
-- @end

-- @case create_aggregate_v ok
CREATE VIEW aggregate_v AS SELECT count(DISTINCT a) FILTER (WHERE a>0) AS n,string_agg(b,',' ORDER BY a DESC) AS labels FROM t;
-- @end

-- @case definition_aggregate_v rows
SELECT pg_get_viewdef('aggregate_v'::regclass) AS definition,pg_get_viewdef('aggregate_v'::regclass,true) AS pretty,pg_get_viewdef('aggregate_v'::regclass,40) AS wrapped;
-- @end

-- @case create_function_v ok
CREATE VIEW function_v AS SELECT n FROM generate_series(1,3) AS s(n);
-- @end

-- @case definition_function_v rows
SELECT pg_get_viewdef('function_v'::regclass) AS definition,pg_get_viewdef('function_v'::regclass,true) AS pretty,pg_get_viewdef('function_v'::regclass,40) AS wrapped;
-- @end

-- @case create_empty_v ok
CREATE VIEW empty_v AS SELECT FROM t;
-- @end

-- @case definition_empty_v rows
SELECT pg_get_viewdef('empty_v'::regclass) AS definition,pg_get_viewdef('empty_v'::regclass,true) AS pretty,pg_get_viewdef('empty_v'::regclass,40) AS wrapped;
-- @end

-- @case create_constants ok
CREATE VIEW constants AS SELECT true AS flag,1::bigint AS wide,2.5::numeric AS decimal,'2024-01-01'::date AS day;
-- @end

-- @case definition_constants rows
SELECT pg_get_viewdef('constants'::regclass) AS definition,pg_get_viewdef('constants'::regclass,true) AS pretty,pg_get_viewdef('constants'::regclass,40) AS wrapped;
-- @end

-- @case overloads rows
SELECT oid,proargtypes,prorettype,proisstrict,provolatile,proparallel,prosrc FROM pg_proc WHERE proname='pg_get_viewdef' ORDER BY oid;
-- @end

-- @case text_overloads rows
SELECT pg_get_viewdef('simple')=pg_get_viewdef('simple'::regclass) AS plain,pg_get_viewdef('simple'::text,true)=pg_get_viewdef('simple'::regclass,true) AS pretty,pg_catalog.pg_get_viewdef('simple'::regclass,false)=pg_get_viewdef('simple') AS qualified;
-- @end

-- @case nulls rows
SELECT pg_get_viewdef(NULL::text) AS name_null,pg_get_viewdef(NULL::oid) AS oid_null,pg_get_viewdef('simple'::regclass,NULL::boolean) AS pretty_null,pg_get_viewdef('simple'::regclass,NULL::integer) AS wrap_null,pg_get_viewdef('missing',NULL::boolean) AS strict;
-- @end

-- @case non_views rows
SELECT pg_get_viewdef('t') AS table_def,pg_get_viewdef('t'::regclass) AS table_oid_def,pg_get_viewdef(0::oid) AS zero,pg_get_viewdef('pg_class') AS catalog_def;
-- @end

-- @case missing_name error
SELECT pg_get_viewdef('missing');
-- @end

-- @case missing_schema error
SELECT pg_get_viewdef('__UQA_SCHEMA_PROBE__.v');
-- @end

-- @case numeric_name error
SELECT pg_get_viewdef('1');
-- @end

-- @case invalid_name error
SELECT pg_get_viewdef('');
-- @end

-- @case too_many_names error
SELECT pg_get_viewdef('a.b.c.d');
-- @end

-- @case invalid_overload error
SELECT pg_get_viewdef('simple'::text,10);
-- @end

-- @case materialized ok
CREATE MATERIALIZED VIEW mv AS SELECT a,b FROM t WITH NO DATA;
-- @end

-- @case materialized_definition rows
SELECT pg_get_viewdef('mv') AS definition;
-- @end

-- @case catalogs rows
SELECT pg_get_viewdef('simple')=definition AS matches FROM pg_views WHERE viewname='simple' AND schemaname=current_schema();
-- @end

-- @case matview_catalog rows
SELECT pg_get_viewdef('mv')=definition AS matches FROM pg_matviews WHERE matviewname='mv' AND schemaname=current_schema();
-- @end

-- @case information_catalog rows
SELECT pg_get_viewdef('simple')=view_definition AS matches FROM information_schema.views WHERE table_name='simple' AND table_schema=current_schema();
-- @end

-- @case create_f ok
CREATE FUNCTION f(int) RETURNS int LANGUAGE SQL RETURN $1+1;
-- @end

-- @case create_routines_v ok
CREATE VIEW routines_v AS SELECT f(a),abs(a) AS absolute FROM t;
-- @end

-- @case definition_routines_v rows
SELECT pg_get_viewdef('routines_v'::regclass) AS definition,pg_get_viewdef('routines_v'::regclass,true) AS pretty,pg_get_viewdef('routines_v'::regclass,40) AS wrapped;
-- @end

-- @case create_natural_v ok
CREATE VIEW natural_v AS SELECT t.a,t.b FROM t NATURAL JOIN (VALUES(1)) AS u(a);
-- @end

-- @case definition_natural_v rows
SELECT pg_get_viewdef('natural_v'::regclass) AS definition,pg_get_viewdef('natural_v'::regclass,true) AS pretty,pg_get_viewdef('natural_v'::regclass,40) AS wrapped;
-- @end

-- @case create_patterns_v ok
CREATE VIEW patterns_v AS SELECT b LIKE 'x%' AS likes,b NOT LIKE 'y%' AS not_likes,b ILIKE 'x%' AS ilikes,b||a::text AS joined FROM t;
-- @end

-- @case definition_patterns_v rows
SELECT pg_get_viewdef('patterns_v'::regclass) AS definition,pg_get_viewdef('patterns_v'::regclass,true) AS pretty,pg_get_viewdef('patterns_v'::regclass,40) AS wrapped;
-- @end

-- @case create_cast_literals ok
CREATE VIEW cast_literals AS SELECT 1::int AS a,1::bigint AS b,'1'::int AS c,2.5::numeric AS d,-1::bigint AS f,'1'::numeric AS n,'-1'::numeric AS negative,'1.0'::numeric AS scaled,123456789012345678901::numeric AS big,'-2.5'::numeric AS neg_fraction,'-1.0'::numeric AS neg_scaled,'1e20'::numeric AS exponential,'-2147483649'::bigint AS negative_big,'2147483648'::bigint AS positive_big;
-- @end

-- @case definition_cast_literals rows
SELECT pg_get_viewdef('cast_literals'::regclass) AS definition,pg_get_viewdef('cast_literals'::regclass,true) AS pretty,pg_get_viewdef('cast_literals'::regclass,40) AS wrapped;
-- @end

-- @case dynamic_string_delimiters rows
SELECT string_agg(value,delimiter ORDER BY n) AS result FROM (VALUES (1,'a','!'),(2,'b',':'::text),(3,NULL,'?'),(4,'c',NULL),(5,'d','/')) AS data(n,value,delimiter);
-- @end

-- @case distinct_string_delimiters rows
SELECT string_agg(DISTINCT value,delimiter ORDER BY value,delimiter) AS result FROM (VALUES ('a','!'),('a','!'),('a','?'),('b',NULL)) AS data(value,delimiter);
-- @end

-- @case string_bytea rows
SELECT encode(string_agg(value,delimiter ORDER BY n),'hex') AS result FROM (VALUES(1,decode('61','hex'),decode('00','hex')),(2,decode('62','hex'),decode('ff','hex'))) AS data(n,value,delimiter);
-- @end

-- @case view_return_rule_definitions rows
SELECT c.relname,c.relhasrules,pg_get_ruledef(r.oid,true) AS definition FROM pg_rewrite r JOIN pg_class c ON c.oid=r.ev_class JOIN pg_namespace n ON n.oid=c.relnamespace WHERE r.rulename='_RETURN' AND c.relname IN ('simple','mv') AND n.nspname=current_schema() ORDER BY c.relname;
-- @end

-- @case create_quoted_cte ok
CREATE VIEW quoted_cte AS WITH "Chosen.CTE"("Key.Name") AS (SELECT a FROM t) SELECT "Key.Name" FROM "Chosen.CTE";
-- @end

-- @case definition_quoted_cte rows
SELECT pg_get_viewdef('quoted_cte') AS definition,pg_get_viewdef('quoted_cte',true) AS pretty,pg_get_viewdef('quoted_cte'::regclass,40) AS wrapped;
-- @end

-- @case quoted_cte_materialized rows
WITH "Q.CTE"(n) AS MATERIALIZED (VALUES(1),(2)) SELECT sum(a.n+b.n)::text AS result FROM "Q.CTE" a CROSS JOIN "Q.CTE" b;
-- @end

-- @case quoted_cte_not_materialized rows
WITH "Q.CTE"(n) AS NOT MATERIALIZED (VALUES(1),(2)) SELECT sum(a.n+b.n)::text AS result FROM "Q.CTE" a CROSS JOIN "Q.CTE" b;
-- @end

-- @case quoted_cte_recursive rows
WITH RECURSIVE "Q.""CTE"(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM "Q.""CTE" WHERE n<3) SELECT sum(n)::text AS result FROM "Q.""CTE";
-- @end

-- @case quoted_cte_nested_shadowing rows
WITH "Q.CTE"(n) AS (VALUES(7)) SELECT a.n::text AS outer_value,b.n::text AS inner_value FROM "Q.CTE" a CROSS JOIN (WITH "Q.CTE"(n) AS (VALUES(99)) SELECT n FROM "Q.CTE") b;
-- @end

-- @case create_full_alias ok
CREATE VIEW full_alias AS SELECT j.a,j.b,j.c FROM (t FULL JOIN u USING(a)) j;
-- @end

-- @case definition_full_alias rows
SELECT pg_get_viewdef('full_alias') AS definition,pg_get_viewdef('full_alias',true) AS pretty,pg_get_viewdef('full_alias'::regclass,40) AS wrapped;
-- @end

-- @case rename_source_column ok
ALTER TABLE t RENAME COLUMN a TO source_a;
-- @end

-- @case renamed_aliased_join_definition rows
SELECT pg_get_viewdef('full_alias') AS definition,pg_get_viewdef('full_alias',true) AS pretty;
-- @end


-- @case rename_table ok
ALTER TABLE t RENAME TO renamed_t;
-- @end

-- @case renamed_definition rows
SELECT pg_get_viewdef('simple') AS definition;
-- @end

-- @case rename_view ok
ALTER VIEW simple RENAME TO renamed_view;
-- @end

-- @case recreate_old_name ok
CREATE VIEW simple AS SELECT -100 AS a;
-- @end

-- @case renamed_view_definition rows
SELECT pg_get_viewdef('renamed_view') AS definition,pg_get_viewdef('simple') AS new_definition;
-- @end
