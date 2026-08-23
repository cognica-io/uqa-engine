-- PG18 differential probes: one SELECT per line, no DDL required.
-- Lines starting with -- are skipped.
-- arithmetic
SELECT 7 / 2
SELECT -7 / 2
SELECT 7 % 3
SELECT -7 % 3
SELECT 7.0 / 2
SELECT 2 ^ 10
SELECT 2 ^ 0.5, pg_typeof(2 ^ 0.5)
SELECT |/ 16.0
SELECT 10 / 4.0
SELECT round(2.5)
SELECT round(3.5)
SELECT round(-2.5)
SELECT round(2.5::float8)
SELECT round(3.5::float8)
SELECT round(1.2345, 2)
SELECT trunc(1.9)
SELECT trunc(-1.9)
SELECT ceil(1.1)
SELECT floor(-1.1)
SELECT abs(-4.5)
SELECT sign(-3)
SELECT mod(9, 4)
SELECT mod(-9, 4)
SELECT div(9, 4)
SELECT gcd(12, 18)
SELECT lcm(4, 6)
SELECT factorial(5)
SELECT sqrt(2)
SELECT cbrt(27)
SELECT exp(1)
SELECT ln(1)
SELECT log(100)
SELECT log(2, 8)
SELECT pi()
SELECT power(2, -1)
SELECT power(2.0000000000000000000000000000000000000000::numeric, 0.5::numeric)
SELECT power((-2)::numeric, 'NaN'::numeric), power((-2)::numeric, 'Infinity'::numeric), power((-2)::numeric, '-Infinity'::numeric)
SELECT 5 // this is invalid syntax test
SELECT 1 / 0
SELECT 1.0 / 0
SELECT 0.1 + 0.2
SELECT 0.1::float8 + 0.2::float8
SELECT 2147483647 + 1
SELECT 9223372036854775807 + 1
SELECT 10::numeric / 3
SELECT 1e10
SELECT 1.5e-3
-- strings
SELECT 'a' || 'b'
SELECT 'a' || NULL
SELECT concat('a', NULL, 'b')
SELECT concat_ws(',', 'a', NULL, 'b')
SELECT length('hello')
SELECT char_length('hello')
SELECT octet_length('hello')
SELECT bit_length('abc')
SELECT upper('MiXeD')
SELECT lower('MiXeD')
SELECT initcap('hello world foo')
SELECT substring('hello' from 2 for 3)
SELECT substring('hello', 2, 3)
SELECT substring('hello', 2)
SELECT substring('hello', -1, 3)
SELECT substr('hello', 0, 3)
SELECT position('ll' in 'hello')
SELECT strpos('hello', 'll')
SELECT overlay('hello' placing 'XX' from 2 for 3)
SELECT trim('  pad  ')
SELECT trim(both 'x' from 'xxpadxx')
SELECT ltrim('xxpad', 'x')
SELECT rtrim('padxx', 'x')
SELECT btrim('xxpadxx', 'x')
SELECT lpad('7', 3, '0')
SELECT rpad('7', 3, '0')
SELECT lpad('hello', 3)
SELECT repeat('ab', 3)
SELECT reverse('abc')
SELECT left('hello', 2)
SELECT left('hello', -2)
SELECT right('hello', 2)
SELECT right('hello', -2)
SELECT split_part('a,b,c', ',', 2)
SELECT split_part('a,b,c', ',', -1)
SELECT string_to_array('a,b,c', ',')
SELECT ascii('A')
SELECT chr(66)
SELECT md5('abc')
SELECT translate('12345', '14', 'ax')
SELECT replace('aaa', 'a', 'b')
SELECT starts_with('hello', 'he')
SELECT format('%s-%s', 1, 'a')
SELECT quote_ident('select')
SELECT quote_literal('O''Reilly')
SELECT 'abc' LIKE 'a%'
SELECT 'abc' LIKE 'A%'
SELECT 'abc' ILIKE 'A%'
SELECT 'a_b' LIKE 'a!_b' ESCAPE '!'
SELECT 'a%b' LIKE 'a!%b' ESCAPE '!'
SELECT 'a_b' LIKE 'a\_b'
SELECT 'a_b' LIKE 'a\_b' ESCAPE ''
SELECT 'a_b' LIKE 'a!_b' ESCAPE NULL
SELECT 'a%b' LIKE 'a💣%b' ESCAPE '💣'
SELECT 'a_b' NOT LIKE 'a!_b' ESCAPE '!'
SELECT 'A_B' ILIKE 'a!_b' ESCAPE '!'
SELECT 'a%b' ILIKE 'aX%b' ESCAPE 'X'
SELECT 'a_b' SIMILAR TO 'a!_b' ESCAPE '!'
SELECT 'a%b' SIMILAR TO 'a!%b' ESCAPE '!'
SELECT 'a_b' SIMILAR TO 'a\_b'
SELECT 'a_b' SIMILAR TO 'a\_b' ESCAPE ''
SELECT 'a_b' SIMILAR TO 'a!_b' ESCAPE NULL
SELECT 'a%b' SIMILAR TO 'a💣%b' ESCAPE '💣'
SELECT '5' SIMILAR TO '!d' ESCAPE '!'
SELECT 'a|b' SIMILAR TO 'a!|b' ESCAPE '!'
SELECT chr(8) SIMILAR TO '!b' ESCAPE '!'
SELECT chr(92) SIMILAR TO '!B' ESCAPE '!'
SELECT '[' SIMILAR TO '[[]'
SELECT 'a' SIMILAR TO '[^^]'
SELECT '^' SIMILAR TO '[!^]' ESCAPE '!'
SELECT ']' SIMILAR TO '[]a]'
SELECT 'b' SIMILAR TO '[^a]'
SELECT '5' SIMILAR TO '[[:digit:]]'
SELECT 'a' LIKE 'a' ESCAPE 'xx'
SELECT 'a' SIMILAR TO 'a' ESCAPE 'xx'
SELECT NULL::text LIKE 'a' ESCAPE 'xx'
SELECT 'a' LIKE NULL::text ESCAPE 'xx'
SELECT 'a' LIKE 'a!' ESCAPE '!'
SELECT 'ab' LIKE 'a!' ESCAPE '!'
SELECT 'a' SIMILAR TO 'a!' ESCAPE '!'
SELECT 'abc' SIMILAR TO 'a(b|c)c'
SELECT 'abc' ~ 'a.c'
SELECT 'abc' ~* 'A.C'
SELECT 'abc' !~ 'x'
SELECT regexp_replace('foo123bar', '[0-9]+', 'X')
SELECT regexp_replace('foo123bar456', '[0-9]+', 'X', 'g')
SELECT regexp_replace('a b', 'a b', 'X', 1, 0, 'x')
SELECT regexp_instr('a b', 'a b', 1, 1, 0, 'x')
SELECT regexp_substr('ab', 'a b', 1, 1, 'x')
SELECT regexp_like(E'a\nb', 'a.b')
SELECT regexp_like(E'a\nb', 'a.b', 'n')
SELECT regexp_like(' ', '[ ]', 'x')
SELECT regexp_like('a+', 'a+', 'b')
SELECT regexp_like('aa', 'a+', 'e'), regexp_like('a+', 'a+', 'e')
SELECT regexp_like('aa', $re$a\{1,\}$re$, 'b')
SELECT (regexp_match('foo123', '[0-9]+'))[1]
SELECT regexp_count('a1b2c3', '[0-9]')
SELECT regexp_like('hello', 'ell')
SELECT to_hex(255)
SELECT encode('abc'::bytea, 'base64')
SELECT decode('YWJj', 'base64')
-- null semantics
SELECT ''::text, NULL, 'NULL'::text
SELECT NULL IS NULL
SELECT NULL = NULL
SELECT NULL IS DISTINCT FROM NULL
SELECT 1 IS DISTINCT FROM NULL
SELECT 1 IS DISTINCT FROM 2
SELECT coalesce(NULL, NULL, 3)
SELECT nullif(5, 5)
SELECT nullif(5, 6)
SELECT greatest(1, NULL, 3)
SELECT least(1, NULL, 3)
SELECT greatest(NULL, NULL)
SELECT NULL + 1
SELECT NULL AND true
SELECT NULL AND false
SELECT NULL OR true
SELECT NULL OR false
SELECT NOT NULL
SELECT 3 IN (1, 2, NULL)
SELECT 3 NOT IN (1, 2, NULL)
SELECT 1 IN (1, NULL)
-- booleans and comparisons
SELECT true AND false
SELECT true::int
SELECT 1::boolean
SELECT 0::boolean
SELECT 'yes'::boolean
SELECT 'off'::boolean
SELECT 't'::boolean
SELECT 2 BETWEEN 1 AND 3
SELECT 2 BETWEEN 3 AND 1
SELECT 2 BETWEEN SYMMETRIC 3 AND 1
SELECT (1, 2) < (1, 3)
SELECT (1, 2) = (1, 2)
SELECT CASE WHEN 1 = 1 THEN 'yes' ELSE 'no' END
SELECT CASE 3 WHEN 1 THEN 'one' WHEN 3 THEN 'three' ELSE 'other' END
-- casts
SELECT '5'::int
SELECT '  5 '::int
SELECT '5.9'::int
SELECT 5.9::int
SELECT 5.4::int
SELECT -5.5::int
SELECT 5.5::int
SELECT 6.5::int
SELECT '5.9'::float8
SELECT 1::text
SELECT 1.50::text
SELECT 1.5::float8::text
SELECT 'abc'::char(2)
SELECT 'ab'::varchar(1)
SELECT 123::varchar(2)
SELECT ''::int
SELECT 'abc'::int
SELECT '{1,2,3}'::int[]
SELECT '2024-01-15'::date
SELECT '15:30:00'::time
SELECT '2024-01-15 10:30:00'::timestamp
SELECT (-1::smallint)::oid
SELECT (-1::integer)::oid
SELECT (-1::bigint)::oid
SELECT '-1'::xid
SELECT 1::xid
SELECT encode((-1::smallint)::bytea, 'hex')
SELECT encode((-1::integer)::bytea, 'hex')
SELECT encode((-1::bigint)::bytea, 'hex')
SELECT true::bytea
SELECT encode('\\x6162'::bytea, 'hex')
-- date and time
SELECT date '2024-01-31' + 1
SELECT date '2024-03-01' - date '2024-02-01'
SELECT date '2024-01-31' + interval '1 month'
SELECT date '2024-01-31' + interval '1 day'
SELECT timestamp '2024-01-15 10:30:00' + interval '90 minutes'
SELECT interval '1 day' + interval '3 hours'
SELECT interval '25 hours'
SELECT interval '1.5 days'
SELECT extract(year from date '2024-06-15')
SELECT extract(month from date '2024-06-15')
SELECT extract(dow from date '2024-06-16')
SELECT extract(isodow from date '2024-06-16')
SELECT extract(doy from date '2024-02-01')
SELECT extract(quarter from date '2024-06-15')
SELECT extract(week from date '2024-01-04')
SELECT extract(epoch from timestamp '1970-01-01 00:01:00')
SELECT extract(hour from time '13:45:00')
SELECT date_part('year', date '2024-06-15')
SELECT date_trunc('month', timestamp '2024-06-15 10:30:00')
SELECT date_trunc('hour', timestamp '2024-06-15 10:30:45')
SELECT make_date(2024, 2, 29)
SELECT make_date(2023, 2, 29)
SELECT make_interval(days => 10)
SELECT make_timestamp(2024, 1, 15, 10, 30, 0)
SELECT to_char(date '2024-06-15', 'YYYY-MM-DD')
SELECT to_char(timestamp '2024-06-15 13:05:00', 'HH24:MI')
SELECT to_char(1234.5, '9999.99')
SELECT to_char(-0.04::numeric, '9.9'), to_char(-0.04::float8, '9.9')
SELECT to_char('NaN'::numeric, '99999999.99'), to_char('Infinity'::numeric, '99999999.99'), to_char('-Infinity'::numeric, '99999999.99')
SELECT to_date('15-06-2024', 'DD-MM-YYYY')
SELECT age(date '2024-06-15', date '2023-01-10')
SELECT date '2024-02-30'
SELECT isfinite(date '2024-01-01')
SELECT date '2024-01-15' - interval '1 week'
-- json / jsonb
SELECT '{"a": 1}'::jsonb
SELECT '{"b":2,"a":1}'::jsonb
SELECT '{"a":1,"a":2}'::jsonb
SELECT '{"a": 1}'::json
SELECT '{"b":2,"a":1}'::json
SELECT '{"a": {"b": 2}}'::jsonb -> 'a'
SELECT '{"a": {"b": 2}}'::jsonb -> 'a' -> 'b'
SELECT '{"a": {"b": 2}}'::jsonb ->> 'a'
SELECT '[1,2,3]'::jsonb -> 0
SELECT '[1,2,3]'::jsonb -> -1
SELECT '[1,2,3]'::jsonb ->> 1
SELECT '{"a": {"b": 2}}'::jsonb #> '{a,b}'
SELECT '{"a": {"b": 2}}'::jsonb #>> '{a,b}'
SELECT '{"a":1,"b":2}'::jsonb ? 'a'
SELECT '{"a":1,"b":2}'::jsonb ?| array['x','b']
SELECT '{"a":1,"b":2}'::jsonb ?& array['a','b']
SELECT '{"a":1}'::jsonb @> '{"a":1}'
SELECT '{"a":1,"b":2}'::jsonb @> '{"a":1}'
SELECT '{"a":1}'::jsonb @> '{"a":1.0}'::jsonb
SELECT '{"a":1}'::jsonb <@ '{"a":1,"b":2}'
SELECT '{"a":1}'::jsonb || '{"b":2}'::jsonb
SELECT '{"a":1,"b":2}'::jsonb - 'a'
SELECT '[1,2,3]'::jsonb - 0
SELECT jsonb_set('{"a":1}'::jsonb, '{b}', '2')
SELECT jsonb_build_object('a', 1, 'b', NULL)
SELECT jsonb_build_array(1, 'x', NULL)
SELECT jsonb_typeof('123'::jsonb)
SELECT jsonb_typeof('"x"'::jsonb)
SELECT jsonb_typeof('null'::jsonb)
SELECT jsonb_array_length('[1,2,3]'::jsonb)
SELECT jsonb_strip_nulls('{"a":1,"b":null}'::jsonb)
SELECT jsonb_pretty('{"a":1}'::jsonb)
SELECT to_jsonb('text'::text)
SELECT to_jsonb(1.5)
SELECT '{"a": [1, 2]}'::jsonb #- '{a,0}'
SELECT jsonb_extract_path('{"a":{"b":1}}'::jsonb, 'a', 'b')
SELECT jsonb_object_keys('{"b":1,"a":2}'::jsonb)
SELECT json_array_length('[1,2,3]'::json)
-- arrays
SELECT ARRAY[1, 2, 3]
SELECT ARRAY[1, 2, 3][2]
SELECT (ARRAY[1, 2, 3])[0]
SELECT (ARRAY[1, 2, 3])[4]
SELECT (ARRAY[1, 2, 3])[1:2]
SELECT (ARRAY[1, 2, 3])[2:]
SELECT array_length(ARRAY[1,2,3], 1)
SELECT array_length(ARRAY[]::int[], 1)
SELECT cardinality(ARRAY[1,2,3])
SELECT array_append(ARRAY[1,2], 3)
SELECT array_prepend(0, ARRAY[1,2])
SELECT array_cat(ARRAY[1], ARRAY[2,3])
SELECT ARRAY[1,2] || ARRAY[3]
SELECT ARRAY[1,2] || 3
SELECT array_position(ARRAY['a','b','c'], 'b')
SELECT array_positions(ARRAY[1,2,1], 1)
SELECT array_remove(ARRAY[1,2,1], 1)
SELECT array_replace(ARRAY[1,2,1], 1, 9)
SELECT array_to_string(ARRAY[1,2,3], '-')
SELECT array_to_string(ARRAY[1,NULL,3], '-', 'N')
SELECT 2 = ANY(ARRAY[1,2,3])
SELECT 5 = ANY(ARRAY[1,2,3])
SELECT 5 <> ALL(ARRAY[1,2,3])
SELECT ARRAY[1,2] && ARRAY[2,3]
SELECT ARRAY[1,2,3] @> ARRAY[2]
SELECT ARRAY[2] <@ ARRAY[1,2,3]
SELECT array_upper(ARRAY[1,2,3], 1)
SELECT array_lower(ARRAY[1,2,3], 1)
SELECT array_fill(7, ARRAY[3])
SELECT trim_array(ARRAY[1,2,3], 1)
SELECT array_sample(ARRAY[1], 1)
-- array transforms
SELECT array_sort(ARRAY[3,NULL,1,2]), array_reverse(ARRAY[3,NULL,1,2])
SELECT array_sort(ARRAY[3,NULL,1,2], true), array_sort(ARRAY[3,NULL,1,2], false, true)
SELECT array_sort(ARRAY[3,1], 'true')
SELECT pg_catalog.array_sort(ARRAY[3,1]), pg_catalog.array_reverse(ARRAY[1,2])
SELECT array_sort(descending => true, "array" => ARRAY[3,1])
SELECT array_sort(ARRAY[3,NULL,1], nulls_first => true, descending => false)
SELECT pg_typeof(array_sort(ARRAY[2::smallint,1::smallint])), pg_typeof(array_reverse(ARRAY[2::bigint,1::bigint]))
SELECT array_sort((SELECT ARRAY[2::bigint,1::bigint])), array_reverse((SELECT ARRAY[1::bigint,2::bigint]))
SELECT array_sort('[0:2]={3,1,2}'::int[]), array_reverse('[0:2]={3,1,2}'::int[]), array_dims(array_sort('[0:2]={3,1,2}'::int[]))
SELECT array_sort('{{2,9},{1,8},{2,7}}'::int[]), array_reverse('{{2,9},{1,8},{2,7}}'::int[])
SELECT array_sort(ARRAY['{"b":1}'::jsonb,'{"a":1}'::jsonb])
SELECT array_sort(NULL)
SELECT array_reverse('{}')
SELECT array_sort(ARRAY[1], 1)
SELECT array_reverse(ARRAY[1], true)
SELECT array_sort(ARRAY[2,1], 'not-a-boolean')
SELECT array_sort(ARRAY['{}'::json,'{}'::json])
SELECT array_sort(ARRAY[ARRAY['{}'::json],ARRAY['{}'::json]])
SELECT array_sort(ARRAY[ROW(1,'{}'::json),ROW(1,'{}'::json)])
-- aggregates over VALUES
SELECT sum(x) FROM (VALUES (1), (2), (3)) AS t(x)
SELECT avg(x) FROM (VALUES (1), (2)) AS t(x)
SELECT avg(x) FROM (VALUES (1.0), (2.0)) AS t(x)
SELECT count(*) FROM (VALUES (1), (NULL)) AS t(x)
SELECT count(x) FROM (VALUES (1), (NULL)) AS t(x)
SELECT sum(x) FROM (VALUES (NULL::int)) AS t(x)
SELECT min(x) FROM (VALUES (3), (1), (2)) AS t(x)
SELECT max(x) FROM (VALUES ('a'), ('c'), ('b')) AS t(x)
SELECT string_agg(x, ',') FROM (VALUES ('a'), ('b')) AS t(x)
SELECT string_agg(x, ',' ORDER BY x DESC) FROM (VALUES ('a'), ('b')) AS t(x)
SELECT array_agg(x ORDER BY x) FROM (VALUES (2), (1)) AS t(x)
SELECT count(DISTINCT x) FROM (VALUES (1), (1), (2)) AS t(x)
SELECT bool_and(x) FROM (VALUES (true), (false)) AS t(x)
SELECT bool_or(x) FROM (VALUES (true), (false)) AS t(x)
SELECT variance(x) FROM (VALUES (1), (2), (3)) AS t(x)
SELECT var_pop(x) FROM (VALUES (1), (2), (3)) AS t(x)
SELECT stddev_samp(x) FROM (VALUES (1), (2), (3)) AS t(x)
SELECT var_pop(x), stddev_pop(x) FROM (VALUES (1::numeric), (1::numeric)) AS t(x)
SELECT var_pop(x), stddev_pop(x) FROM (VALUES ('Infinity'::numeric), (1::numeric)) AS t(x)
SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY x) FROM (VALUES (1), (2), (3), (4)) AS t(x)
SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY x) FROM (VALUES (1), (2), (3), (4)) AS t(x)
SELECT mode() WITHIN GROUP (ORDER BY x) FROM (VALUES (1), (1), (2)) AS t(x)
SELECT json_agg(x) FROM (VALUES (1), (2)) AS t(x)
SELECT jsonb_object_agg(k, v) FROM (VALUES ('a', 1), ('b', 2)) AS t(k, v)
-- named windows
SELECT grp, x, row_number() OVER w AS rn, sum(x) OVER w AS running FROM (VALUES ('a', 2), ('a', 1), ('b', 3)) AS t(grp, x) WINDOW w AS (PARTITION BY grp ORDER BY x) ORDER BY grp, x
SELECT grp, x, sum(x) OVER w2 FROM (VALUES ('a', 2), ('a', 1), ('b', 3)) AS t(grp, x) WINDOW w AS (PARTITION BY grp), w2 AS (w ORDER BY x ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) ORDER BY grp, x
SELECT sum(x) OVER missing FROM (VALUES (1)) AS t(x)
SELECT sum(x) OVER w FROM (VALUES (1)) AS t(x) WINDOW w AS (), w AS ()
SELECT sum(x) OVER w2 FROM (VALUES (1)) AS t(x) WINDOW w AS (), w2 AS (w PARTITION BY x)
SELECT sum(x) OVER w2 FROM (VALUES (1)) AS t(x) WINDOW w AS (ORDER BY x), w2 AS (w ORDER BY x)
SELECT sum(x) OVER (w) FROM (VALUES (1)) AS t(x) WINDOW w AS (ORDER BY x ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)
-- integer base conversion
SELECT to_bin(0), to_bin(42), to_bin(-42), to_bin((-42)::bigint)
SELECT to_bin((-2147483648)::integer)
SELECT to_bin((-9223372036854775807 - 1)::bigint)
SELECT to_oct(0), to_oct(42), to_oct(-42), to_oct((-42)::bigint)
SELECT to_oct((-2147483648)::integer)
SELECT to_oct((-9223372036854775807 - 1)::bigint)
SELECT pg_typeof(to_bin(42)), pg_typeof(to_oct(42::bigint))
SELECT pg_catalog.to_bin(42), pg_catalog.to_oct(42)
SELECT to_bin(NULL::integer), to_oct(NULL::bigint)
SELECT to_bin((SELECT (-1)::bigint)), to_oct((SELECT (-1)::bigint))
SELECT to_bin('42')
SELECT to_oct(NULL)
SELECT to_bin(1::smallint)
SELECT to_oct(1::smallint)
SELECT to_bin((SELECT 1::smallint))
SELECT to_oct((SELECT '42'))
SELECT to_bin('42'::text)
SELECT to_oct(1::numeric)
SELECT to_bin()
SELECT to_oct(1, 2)
SELECT to_bin(value => 1)
-- random ranges
SELECT random(5, 5), pg_typeof(random(5, 5)), random(5::bigint, 5::bigint), pg_typeof(random(5::bigint, 5::bigint)), random(5.00::numeric, 5.000::numeric), pg_typeof(random(5.00::numeric, 5.000::numeric))
SELECT random(max => 7, min => 7), random(8, max => 8)
SELECT pg_typeof(random(1::smallint, 2::integer)), pg_typeof(random(1::integer, 2::bigint)), pg_typeof(random(1::bigint, 2::numeric))
SELECT pg_typeof(random((SELECT 1::integer), (SELECT 2::bigint)))
SELECT random(NULL::integer, 2), random(1::bigint, NULL::bigint), random(NULL::numeric, 1::numeric)
SELECT setseed(0.25), random(), random(1, 10), random(1::bigint, 10::bigint), random('-12345678901234567890.12345'::numeric, '98765432109876543210.9'::numeric), random()
SELECT random(1::smallint, 2::smallint)
SELECT random('1', '2')
SELECT random(1::real, 2::real)
SELECT random(1, min => 2)
SELECT random(max => 2, 1)
SELECT random(min => 1, min => 2)
SELECT random(9, 1)
SELECT random('NaN'::numeric, 1::numeric)
SELECT random(0::numeric, 'Infinity'::numeric)
-- UUID extraction
SELECT uuid_extract_version('00000000-0000-0000-0000-000000000000')
SELECT uuid_extract_version('00000000-0000-f000-8000-000000000000')
SELECT uuid_extract_version('00000000-0000-7000-c000-000000000000')
SELECT uuid_extract_timestamp('a8098c1a-f86e-11da-bd1a-00112444be1e')
SELECT uuid_extract_timestamp('00000000-0001-7fff-8000-000000000000')
SELECT uuid_extract_timestamp('00000000-0000-4000-8000-000000000000')
SELECT pg_typeof(uuid_extract_version('00000000-0000-7000-8000-000000000000'))
SELECT pg_typeof(uuid_extract_timestamp('00000000-0000-7000-8000-000000000000'))
SELECT uuid_extract_version((SELECT '00000000-0000-7000-8000-000000000000'::uuid))
SELECT uuid_extract_timestamp((SELECT '00000000-0001-7000-8000-000000000000'::uuid))
SELECT uuid_extract_version((SELECT '00000000-0000-7000-8000-000000000000'))
SELECT uuid_extract_version(1)
SELECT uuid_extract_timestamp('not-a-uuid')
-- rows / set returning / misc
SELECT * FROM generate_series(1, 3)
SELECT generate_series(1, 3)
SELECT abs(generate_series(-2, 0))
SELECT generate_series(1, 2), generate_series(10, 12)
SELECT generate_series(1, generate_series(1, 2))
SELECT id, generate_series(1, id) AS n FROM (VALUES (2), (1)) AS t(id) ORDER BY id, n
SELECT generate_series(1, 5) AS n LIMIT 2
SELECT generate_series(1, CASE WHEN id = 1 THEN 2 ELSE 10 / (id - id) END) AS n FROM (VALUES (1), (2)) AS t(id) LIMIT 2
SELECT jsonb_object_keys('{"b":1,"a":2}'::jsonb)
SELECT count(*), generate_series(1, 2) FROM (VALUES (1), (2)) AS t(x)
SELECT count(*) + generate_series(1, 2) FROM (VALUES (1), (2)) AS t(x)
SELECT row_number() OVER (ORDER BY x), generate_series(1, 2) FROM (VALUES (1), (2)) AS t(x)
SELECT 1 ORDER BY generate_series(1, 2)
SELECT DISTINCT ON (generate_series(1, 2)) 1 ORDER BY generate_series(1, 2)
SELECT generate_series(1, 2) AS x ORDER BY generate_series(10, 12)
SELECT count(*) FROM (VALUES (1), (2)) AS t(x) ORDER BY generate_series(1, 2)
SELECT row_number() OVER () FROM (VALUES (1), (2)) AS t(x) ORDER BY generate_series(1, 2)
SELECT generate_series(1, 2), count(*) FROM (VALUES (1), (2)) AS t(x) GROUP BY generate_series(1, 2) ORDER BY 1
SELECT generate_series(1, 2), count(*) FROM (VALUES (1)) AS t(x) GROUP BY generate_series(1, 3) ORDER BY 1
SELECT count(*) FROM (SELECT a, count(*) FROM (VALUES (1), (1), (2)) AS v(a) GROUP BY DISTINCT a) AS grouped
SELECT count(*) FROM (SELECT a, count(*) FROM (VALUES (1), (1), (2)) AS v(a) GROUP BY DISTINCT GROUPING SETS ((a), (a), ())) AS grouped
SELECT count(*) FROM (SELECT a, count(*) FROM (VALUES (1), (1), (2)) AS v(a) GROUP BY ALL GROUPING SETS ((a), (a), ())) AS grouped
SELECT count(*) FROM (SELECT a, b, count(*) FROM (VALUES (1, 10), (1, 20), (2, 10)) AS v(a, b) GROUP BY DISTINCT GROUPING SETS ((a, b), (b, a))) AS grouped
SELECT count(*) FROM (SELECT a, count(*) FROM (VALUES (1), (1), (2)) AS v(a) GROUP BY DISTINCT ROLLUP(a, a)) AS grouped
SELECT count(*) FROM (SELECT a, count(*) FROM (VALUES (1), (1), (2)) AS v(a) GROUP BY DISTINCT CUBE(a, a)) AS grouped
SELECT count(*) FROM (SELECT a, count(*) FROM (VALUES (1), (1), (2)) AS v(a) GROUP BY ALL CUBE(a, a)) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES (1), (2)) AS v(a) GROUP BY DISTINCT GROUPING SETS ((a + 1), (a + 1.0), (a + 1.00))) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES (1), (2)) AS v(a) GROUP BY DISTINCT GROUPING SETS ((a + 1), (((a + 1))))) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES (1)) AS v(a) WHERE false GROUP BY DISTINCT GROUPING SETS ((), ())) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES (1, 10), (2, 20)) AS v(a, b) GROUP BY DISTINCT GROUPING SETS ((ROW(a, b)), (ROW(b, a)))) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES (1), (2)) AS v(a) GROUP BY DISTINCT GROUPING SETS ((a + 1), (a + 1::integer))) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES (1::bigint), (2::bigint)) AS v(a) GROUP BY DISTINCT GROUPING SETS ((a + 1), (a + 1::bigint))) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES (1), (2)) AS v(a) GROUP BY DISTINCT GROUPING SETS ((a), (v.a))) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES (1), (2)) AS v(a) GROUP BY DISTINCT GROUPING SETS ((a + NULL), (a + NULL::integer))) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES ('A'::text), ('B'::text)) AS v(a) GROUP BY DISTINCT GROUPING SETS ((LOWER(a)), (lower(a)), (pg_catalog.lower(a)))) AS grouped
SELECT count(*) FROM (SELECT count(*) GROUP BY DISTINCT GROUPING SETS ((lower(NULL)), (lower(NULL::text)))) AS grouped
SELECT count(*) FROM (SELECT count(*) GROUP BY DISTINCT GROUPING SETS ((btrim(NULL)), (btrim(NULL::text)))) AS grouped
SELECT count(*) FROM (SELECT count(*) FROM (VALUES ('a'::text), ('b'::text)) AS v(a) GROUP BY DISTINCT GROUPING SETS ((a || NULL), (a || NULL::text))) AS grouped
SELECT CASE WHEN true THEN generate_series(1, 2) END
SELECT coalesce(generate_series(1, 2), 0)
SELECT 1 WHERE generate_series(1, 2) > 0
SELECT 1 HAVING generate_series(1, 2) > 0
SELECT 1 LIMIT generate_series(1, 2)
SELECT 1 OFFSET generate_series(1, 2)
SELECT 1 FROM (VALUES (1)) AS t(x) JOIN (VALUES (1)) AS u(y) ON generate_series(1, 2) > 0
VALUES (generate_series(1, 2))
SELECT * FROM generate_series(1, generate_series(1, 2))
SELECT * FROM generate_series(5, 1, -2)
SELECT * FROM unnest(ARRAY['x', 'y'])
SELECT * FROM generate_series(4, 6) WITH ORDINALITY
SELECT value, sequence, pg_typeof(sequence) FROM generate_series(4, 5) WITH ORDINALITY AS g(value, sequence)
SELECT * FROM json_each('{"a": 1}') WITH ORDINALITY AS j(k)
SELECT v.n, g.value, g.ordinality FROM (VALUES (2), (0), (1)) AS v(n) CROSS JOIN LATERAL generate_series(1, v.n) WITH ORDINALITY AS g(value, ordinality) ORDER BY v.n DESC, g.ordinality
SELECT * FROM unnest(ARRAY[1, 2], ARRAY['x']) WITH ORDINALITY AS u(a, b, n)
SELECT * FROM generate_series(1, 1) WITH ORDINALITY AS g(a, b, c)
SELECT x FROM (VALUES (1), (2)) AS t(x) ORDER BY x DESC LIMIT 1
-- FETCH WITH TIES
SELECT x FROM (VALUES (1), (2), (2), (3)) AS t(x) ORDER BY x FETCH FIRST 2 ROWS WITH TIES
SELECT x FROM (VALUES (1), (1), (2)) AS t(x) ORDER BY x FETCH FIRST 0 ROWS WITH TIES
SELECT x FROM (VALUES (1), (1), (2), (2), (3)) AS t(x) ORDER BY x OFFSET 1 FETCH FIRST 2 ROWS WITH TIES
SELECT x FROM (VALUES (1, 1, 1), (2, 1, 2), (3, 1, 2), (4, 2, 1)) AS t(x, a, b) ORDER BY a, b FETCH FIRST 2 ROWS WITH TIES
SELECT x FROM (VALUES (1), (2), (NULL), (NULL)) AS t(x) ORDER BY x NULLS LAST OFFSET 2 FETCH FIRST 1 ROW WITH TIES
SELECT x FROM (VALUES (1), (1), (2)) AS t(x) ORDER BY x FETCH FIRST ROW WITH TIES
SELECT x FROM generate_series(1, 5) AS g(x) ORDER BY x FETCH FIRST 2.5 ROWS WITH TIES
SELECT x FROM generate_series(1, 5) AS g(x) ORDER BY x FETCH FIRST (2.5::float8) ROWS WITH TIES
SELECT x FROM generate_series(1, 5) AS g(x) ORDER BY x FETCH FIRST '2' ROWS WITH TIES
VALUES (1), (2), (2), (3) ORDER BY 1 FETCH FIRST 2 ROWS WITH TIES
SELECT x FROM (VALUES (1), (2)) AS l(x) UNION ALL SELECT x FROM (VALUES (2), (3)) AS r(x) ORDER BY x FETCH FIRST 2 ROWS WITH TIES
SELECT 1 FETCH FIRST 1 ROW WITH TIES
SELECT 1 ORDER BY 1 FETCH FIRST NULL ROWS WITH TIES
SELECT 1 ORDER BY 1 OFFSET -1 FETCH FIRST 1 ROW WITH TIES
SELECT 1 ORDER BY 1 FETCH FIRST ('2'::text) ROWS WITH TIES
SELECT 1 ORDER BY 1 FETCH FIRST ('NaN'::numeric) ROWS WITH TIES
SELECT DISTINCT x FROM (VALUES (1), (1), (2)) AS t(x) ORDER BY x
SELECT x FROM (VALUES (1), (NULL), (2)) AS t(x) ORDER BY x
SELECT x FROM (VALUES (1), (NULL), (2)) AS t(x) ORDER BY x DESC
SELECT x FROM (VALUES (1), (NULL), (2)) AS t(x) ORDER BY x NULLS FIRST
SELECT row_number() OVER (ORDER BY x) FROM (VALUES ('b'), ('a')) AS t(x)
SELECT rank() OVER (ORDER BY x) FROM (VALUES (1), (1), (2)) AS t(x)
SELECT dense_rank() OVER (ORDER BY x) FROM (VALUES (1), (1), (2)) AS t(x)
SELECT lag(x) OVER (ORDER BY x) FROM (VALUES (1), (2)) AS t(x)
SELECT lag(x, 1, 0) OVER (ORDER BY x) FROM (VALUES (1), (2)) AS t(x)
SELECT sum(x) OVER (ORDER BY x) FROM (VALUES (1), (2), (3)) AS t(x)
SELECT nullif(1, 1) IS NULL
SELECT current_database()
SELECT current_schema
SELECT 1 WHERE false
SELECT EXISTS (SELECT 1 WHERE false)
SELECT (SELECT 42)
SELECT num_nulls(1, NULL, 2)
SELECT num_nonnulls(1, NULL, 2)
SELECT pg_typeof(1)
SELECT pg_typeof(1.5)
SELECT pg_typeof('x'::text)
SELECT pg_typeof(now())
SELECT width_bucket(5.35, 0, 10, 5)
SELECT setseed(0.5)
-- qualified joins
SELECT * FROM (VALUES (1, 'left-value')) AS l(id, shared) JOIN (VALUES (1, 'right-value')) AS r(id, shared) USING (id)
SELECT * FROM (VALUES (1, 'l1'), (2, 'l2')) AS l(id, lval) FULL JOIN (VALUES (1, 'r1'), (3, 'r3')) AS r(id, rval) USING (id) ORDER BY id
SELECT * FROM (VALUES (2, 'b', 'left')) AS l(id, shared, lval) NATURAL JOIN (VALUES ('b', 2, 'right')) AS r(shared, id, rval)
SELECT merged.id, l.id, r.id FROM (VALUES (1), (2)) AS l(id) FULL JOIN (VALUES (1), (3)) AS r(id) USING (id) AS merged ORDER BY merged.id
SELECT id, l.id, r.id, t.id FROM (VALUES (1), (2)) AS l(id) JOIN (VALUES (1), (3)) AS r(id) USING (id) JOIN (VALUES (1), (4)) AS t(id) USING (id)
WITH left_cte(id, shared, lval) AS (VALUES (1, 'same', 'l1'), (2, 'left', 'l2')) SELECT l.lval FROM left_cte l NATURAL JOIN (VALUES (1, 'same', 'r1'), (3, 'right', 'r3')) AS r(id, shared, rval)
SELECT * FROM (VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING (id, id)
SELECT * FROM (VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING (missing)
-- aliases on parenthesized joins
SELECT j.* FROM ((VALUES (1,'l')) AS l(id,lv) JOIN (VALUES (1,'r')) AS r(id,rv) ON l.id=r.id) AS j
SELECT j.a,j.lv,j.rv FROM ((VALUES (1,'l')) AS l(id,lv) JOIN (VALUES (1,'r')) AS r(id,rv) USING(id)) AS j(a)
SELECT j.id FROM ((VALUES (1)) AS l(id) LEFT JOIN (VALUES (1)) AS r(id) ON l.id=r.id) AS j(id,right_id) WHERE j.right_id IS NOT NULL
SELECT j.*, q.n FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING(id)) AS j CROSS JOIN LATERAL (SELECT j.id+1 AS n) AS q
SELECT l.id FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING(id)) AS j
SELECT j.* FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING(id)) AS j(a,b)
SELECT j.a FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) ON l.id=r.id) AS j(a,a)
SELECT j.* FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) ON j.id=r.id) AS j
SELECT j.* FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING(id)) AS j FOR UPDATE OF j
SELECT merged.id FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) USING(id) AS merged) AS j
-- appended: PG18 semantics round (3VL edges, intervals, operators)
SELECT interval '-1 day 3 hours'
SELECT interval '1 day -3 hours'
SELECT interval '1.5 mons'
SELECT interval '1-2'
SELECT interval '3 4:05:06'
SELECT interval '90'
SELECT interval '2 years -1 mons'
SELECT interval '1 mon' * 2
SELECT interval '1 mon' = interval '30 days'
SELECT interval '1 day' < interval '25 hours'
SELECT justify_hours(interval '27 hours')
SELECT age(date '2024-03-31', date '2024-01-30')
SELECT age(timestamp '2024-03-01 10:00:00', timestamp '2024-03-31 08:00:00')
SELECT timestamp '2024-03-01 00:00:00' - timestamp '2024-01-30 12:30:00'
SELECT time '13:45:00' + interval '30 minutes'
SELECT extract(epoch from interval '1 minute')
SELECT extract(month from interval '14 months')
SELECT extract(days from interval '40 days')
SELECT make_interval(years => 1, days => 10, secs => 30.5)
SELECT 2 BETWEEN SYMMETRIC NULL AND 1
SELECT 2 BETWEEN 3 AND NULL
SELECT (1, NULL) = (1, 2)
SELECT (2, 2) < (1, NULL)
SELECT 1 = ANY(ARRAY[1, NULL])
SELECT 3 = ANY(ARRAY[1, NULL])
SELECT 3 <> ALL(ARRAY[3, NULL])
SELECT ltrim('xyxpad', 'xy')
SELECT split_part('a,b,c', ',', -2)
SELECT split_part('a,b,c', ',', 0)
SELECT substring('hello', 0)
SELECT 'of'::boolean
SELECT 'tr'::boolean
SELECT 'o'::boolean
SELECT 2.5::float8::int
SELECT 3.5::float8::int
SELECT 40000::smallint
SELECT quote_ident('Hello')
SELECT quote_ident('hello')
SELECT to_hex(-1)
SELECT string_to_array('a,b,c', ',', 'b')
SELECT string_to_array('', ',')
SELECT regexp_match('foo123', '[0-9]+')
SELECT NULL ~ 'x'
SELECT 'a.c' SIMILAR TO 'a.c'
SELECT 'axc' SIMILAR TO 'a.c'
SELECT 'abc' NOT SIMILAR TO 'a_c'
SELECT date '2024-01-31' - 1
SELECT factorial(0)
SELECT cbrt(-27)
SELECT num_nonnulls(NULL, NULL)
-- independent review regressions
SELECT to_char(2.5::numeric, '9'), to_char(2.5::float8, '9'), to_char(-2.5::numeric, '9'), to_char(1.25::numeric, '9.9')
SELECT avg(x), pg_typeof(avg(x)) FROM (VALUES (9007199254740992::bigint), (9007199254740993::bigint)) AS t(x)
SELECT '[0:-1]={}'::int[]
SELECT array_cat('[0:0][2:3]={{1,2}}'::int[], '[5:5][9:10]={{3,4}}'::int[])
SELECT array_append('{}'::int[], 1)
SELECT array_prepend(1, '{}'::int[])
SELECT ARRAY[1] < ARRAY[2]
SELECT ARRAY[1,2] < ARRAY[[1,2]]
SELECT ARRAY[2,0] > ARRAY[[1,9]]
SELECT ARRAY[1] < '[2:2]={1}'::int[]
SELECT encode((ARRAY[1::smallint]::bytea[])[1], 'hex')
SELECT '-+1'::numeric
SELECT '+NaN'::numeric
SELECT pg_typeof(ARRAY['x', 'y'::varchar])
SELECT pg_typeof(CASE WHEN true THEN 'x' ELSE 'y'::varchar END)
SELECT pg_typeof(COALESCE('x', 'y'::varchar))
SELECT 1e-9000::numeric * 1e-9000::numeric = 0
SELECT 0e200000::numeric = 0
SELECT ('{\ a}'::text[])[1] = ' a'
SELECT ('{N\ULL}'::text[])[1] = 'NULL'
SELECT * FROM (VALUES (1)) AS l(id) FULL JOIN (VALUES (1)) AS r(id) USING (id) AS l
SELECT true, false, ROW(true, false)
SELECT to_char(12::numeric, 'fm000'), to_char(-1.2::numeric, 'S9'), to_char(1::numeric, 'FM090'), to_char(12345.6::numeric, 'FM9999.99')
SELECT regexp_like('a', 'a', 'qn')
SELECT power(0.000001::numeric, 3::numeric), power(2::numeric, 0.1::numeric), power(4::numeric, 0.25::numeric)
SELECT power((-2)::numeric, 0.1::numeric)
SELECT power(0::numeric, (-0.1)::numeric)
SELECT regexp_like('a^b', 'a^b', 'b'), regexp_like('a$b', 'a$b', 'e')
SELECT pg_typeof(power(2::numeric, '0.5')), pg_typeof(power('2', 0.5::numeric)), pg_typeof(power('2', '0.5'))
SELECT power(2, NULL::text)
SELECT var_pop(x) FROM (VALUES (1e70000::numeric), (1e70000::numeric)) AS t(x)
SELECT var_pop(x) FROM (VALUES (1e70000::numeric), (1e70000::numeric + 1)) AS t(x)
SELECT regexp_like(E'\n', '[^a]', 'n'), regexp_like(E'\n', '[^\n]', 'en')
SELECT to_char('NaN'::numeric, '000MI'), to_char('Infinity'::numeric, '000PL'), to_char('-Infinity'::numeric, '99999999.99PR')
SELECT to_char(1e20::float8, '9.9'), to_char(-1e20::float8, '9.9MI'), to_char(1e20::float8, 'FM9.9PL')
SELECT to_char(12::numeric, 'fM000'), to_char(-1.2::numeric, '9Mi'), to_char(12::numeric, '"USD"000')
SELECT jsonb_pretty('[]'::jsonb), jsonb_pretty('{}'::jsonb), jsonb_pretty('{"zz":1,"b":[],"aa":{"long":3,"x":2}}'::jsonb)
SELECT regexp_like('b', '[^]a]', 'n'), regexp_like('d', '\d', 'b'), regexp_like('1', '\d', 'e')
SELECT to_char(12::numeric, '9999.'), to_char(-12::numeric, 'FM9999.')
SELECT 0e-16383::numeric / 4
SELECT var_pop(x), stddev_pop(x) FROM (VALUES ('Infinity'::numeric), (1e100000::numeric)) AS t(x)
SELECT var_pop(x) FROM (VALUES (0::numeric), (1e-10000::numeric)) AS t(x)
SELECT regexp_like(' ', '[[:digit:] ]', 'x'), regexp_like('#', '[[:digit:]#]', 'x')
SELECT regexp_like('ab', 'a' || chr(160) || 'b', 'x'), regexp_like('a' || chr(160) || 'b', 'a' || chr(160) || 'b', 'x'), regexp_like('ab', 'a' || chr(8195) || 'b', 'x')
SELECT power(1::numeric + 1e-16383::numeric, 0.5::numeric)
SELECT to_char(9.99::numeric, '9.'), to_char(-9.99::numeric, '9.S'), to_char(-2.5::numeric, '9.S'), to_char(0.5::numeric, '.9'), to_char(0.5::numeric, 'FM.9')
SELECT to_char(12::numeric, 'SFM999'), to_char(-12::numeric, 'FMS999'), to_char(12::numeric, '999FMS'), to_char('NaN'::numeric, 'SFM999'), to_char(12::numeric, '"USD"SFM999')
SELECT jsonb_pretty('1e-1000'::jsonb), jsonb_pretty('-0'::jsonb), jsonb_pretty('1.00'::jsonb)
SELECT regexp_like('1', '[^-a]', 'n'), regexp_like('[', '[[]'), regexp_like('1', '[^[]', 'n')
SELECT to_char(-1.25::numeric, 'S.9'), to_char(-1.25::numeric, '9S.9'), to_char(-1.2::numeric, '9SG'), to_char(-1e20::float8, 'FM9.9MI'), to_char(1e20::float8, 'FM9.0PL')
SELECT to_char(12::numeric, 'PL999'), to_char(-12::numeric, 'PL999'), to_char(12::numeric, 'SG999'), to_char(-12::numeric, 'SG999'), to_char(12::numeric, '9SG99'), to_char(-12::numeric, '9MI99')
SELECT to_char(12::numeric, '9S9.9'), to_char(-12::numeric, '99S.9'), to_char(12::numeric, '9MI99SG'), to_char(-12::numeric, '9MI99SG')
SELECT to_char(12::numeric, 'PR999')
SELECT to_char(12::numeric, '9S99MI')
SELECT to_char(1::numeric, 'FM9.9MIPL'), to_char(-1::numeric, 'FM9.9MIPL'), to_char(-12::numeric, '999S,'), to_char(-12::numeric, '999,S'), to_char(-12::numeric, '999PR,'), to_char(12::numeric, '999PR,')
SELECT '[2147483647:2147483647]={1}'::int[]
SELECT '[1.0]'::jsonb @> '1'::jsonb, '[{"a":1}]'::jsonb @> '{"a":1}'::jsonb, '{"a":[1.0]}'::jsonb @> '{"a":1}'::jsonb, '[[1.0]]'::jsonb @> '[1]'::jsonb, '{"a":[1.0]}'::jsonb @> '{"a":[1]}'::jsonb
SELECT power(1e-1000::numeric, -17::numeric) = 1e17000::numeric, length(power(1e-1000::numeric, -17::numeric)::text)
SELECT to_char(1e20::float8, 'FM09.90PL'), to_char(-1e20::float8, 'FM09.90MI'), to_char(-1e20::float8, 'FM09.90SG')
SELECT to_char(1485::numeric, '9,999'), to_char(3148.5::numeric, '9G999D999'), to_char(485::numeric, 'L999'), to_char(12::numeric, 'FM00L')
SELECT to_char(12.45::numeric, '99V9'), to_char(482::numeric, '999th'), to_char(0.5::numeric, 'FM00TH'), to_char(12::numeric, 'SP')
SELECT to_char(485::numeric, 'RN'), to_char(5.2::numeric, 'FMRN'), to_char(0.0004859::numeric, '9.99EEEE'), to_char(9.99::float8, '9.9EEEE')
SELECT '1e131072'::jsonb
SELECT '1e-16384'::jsonb
SELECT '[1e131072]'::jsonb
SELECT '{"n":1e131072}'::jsonb
SELECT '1e131071'::jsonb > '0'::jsonb, '0e200000'::jsonb = '0'::jsonb, json_typeof('1e200000'::json)
