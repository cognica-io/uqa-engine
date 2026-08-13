-- PG18 differential probes: one SELECT per line, no DDL required.
-- Lines starting with -- are skipped.
-- arithmetic
SELECT 7 / 2
SELECT -7 / 2
SELECT 7 % 3
SELECT -7 % 3
SELECT 7.0 / 2
SELECT 2 ^ 10
SELECT 2 ^ 0.5
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
SELECT 'abc' SIMILAR TO 'a(b|c)c'
SELECT 'abc' ~ 'a.c'
SELECT 'abc' ~* 'A.C'
SELECT 'abc' !~ 'x'
SELECT regexp_replace('foo123bar', '[0-9]+', 'X')
SELECT regexp_replace('foo123bar456', '[0-9]+', 'X', 'g')
SELECT (regexp_match('foo123', '[0-9]+'))[1]
SELECT regexp_count('a1b2c3', '[0-9]')
SELECT regexp_like('hello', 'ell')
SELECT to_hex(255)
SELECT encode('abc'::bytea, 'base64')
SELECT decode('YWJj', 'base64')
-- null semantics
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
SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY x) FROM (VALUES (1), (2), (3), (4)) AS t(x)
SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY x) FROM (VALUES (1), (2), (3), (4)) AS t(x)
SELECT mode() WITHIN GROUP (ORDER BY x) FROM (VALUES (1), (1), (2)) AS t(x)
SELECT json_agg(x) FROM (VALUES (1), (2)) AS t(x)
SELECT jsonb_object_agg(k, v) FROM (VALUES ('a', 1), ('b', 2)) AS t(k, v)
-- rows / set returning / misc
SELECT * FROM generate_series(1, 3)
SELECT generate_series(1, 3)
SELECT * FROM generate_series(5, 1, -2)
SELECT * FROM unnest(ARRAY['x', 'y'])
SELECT x FROM (VALUES (1), (2)) AS t(x) ORDER BY x DESC LIMIT 1
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
