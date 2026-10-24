\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned
BEGIN;
CREATE ROLE uqa_coverage_reader_92fe478a8650;
CREATE SCHEMA uqa_coverage_92fe478a8650;
SET search_path = uqa_coverage_92fe478a8650, pg_catalog;
GRANT USAGE ON SCHEMA uqa_coverage_92fe478a8650 TO uqa_coverage_reader_92fe478a8650;
CREATE TABLE coverage_input(id integer, secret text);
INSERT INTO coverage_input VALUES (1, 'one'), (2, 'two');
GRANT SELECT(id) ON coverage_input TO uqa_coverage_reader_92fe478a8650;
CREATE VIEW coverage_owner_view AS SELECT id, secret FROM coverage_input;
CREATE VIEW coverage_invoker_view WITH (security_invoker=true) AS SELECT id, secret FROM coverage_input;
GRANT SELECT ON coverage_owner_view, coverage_invoker_view TO uqa_coverage_reader_92fe478a8650;
CREATE FUNCTION coverage_definer() RETURNS text LANGUAGE sql SECURITY DEFINER AS 'SELECT secret FROM coverage_input WHERE id = 1';
CREATE FUNCTION coverage_invoker() RETURNS text LANGUAGE sql SECURITY INVOKER AS 'SELECT secret FROM coverage_input WHERE id = 1';
CREATE FUNCTION pg_temp.coverage_probe(label text, command text) RETURNS jsonb LANGUAGE plpgsql AS $probe$
DECLARE result jsonb; state text; message text;
BEGIN
  EXECUTE 'SELECT COALESCE(jsonb_agg(to_jsonb(result_row)), ''[]''::jsonb) FROM (' || command || ') AS result_row' INTO result;
  RETURN jsonb_build_object('case', label, 'rows', result);
EXCEPTION WHEN OTHERS THEN
  GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE, message = MESSAGE_TEXT;
  RETURN jsonb_build_object('case', label, 'error', jsonb_build_object('sqlstate', state, 'message', message));
END;
$probe$;
SET ROLE uqa_coverage_reader_92fe478a8650;
SELECT pg_temp.coverage_probe('nested allowed', 'SELECT id FROM (SELECT id FROM (SELECT id FROM coverage_input OFFSET 0) AS inner_input OFFSET 0) AS derived ORDER BY id');
SELECT pg_temp.coverage_probe('unused inner column denied', 'SELECT id FROM (SELECT id, secret FROM coverage_input OFFSET 0) AS derived ORDER BY id');
SELECT pg_temp.coverage_probe('qualification column denied', 'SELECT id FROM (SELECT id FROM coverage_input WHERE secret = ''one'' OFFSET 0) AS derived');
SELECT pg_temp.coverage_probe('zero projected columns', 'SELECT count(*) FROM (SELECT FROM coverage_input OFFSET 0) AS derived');
SELECT pg_temp.coverage_probe('count with column grant', 'SELECT count(*) FROM (SELECT id FROM coverage_input OFFSET 0) AS derived');
SELECT pg_temp.coverage_probe('reachable CTE allowed', 'WITH c AS (SELECT id FROM coverage_input) SELECT id FROM (SELECT id FROM c OFFSET 0) AS derived ORDER BY id');
SELECT pg_temp.coverage_probe('reachable CTE denied', 'WITH c AS (SELECT secret FROM coverage_input) SELECT secret FROM (SELECT secret FROM c OFFSET 0) AS derived');
SELECT pg_temp.coverage_probe('lateral allowed', 'SELECT p.id, d.value FROM coverage_input AS p CROSS JOIN LATERAL (SELECT p.id + 1 AS value OFFSET 0) AS d ORDER BY p.id');
SELECT pg_temp.coverage_probe('lateral denied', 'SELECT d.value FROM coverage_input AS p CROSS JOIN LATERAL (SELECT p.secret AS value OFFSET 0) AS d');
SELECT pg_temp.coverage_probe('view owner', 'SELECT secret FROM (SELECT secret FROM coverage_owner_view OFFSET 0) AS derived ORDER BY secret');
SELECT pg_temp.coverage_probe('view invoker denied', 'SELECT secret FROM (SELECT secret FROM coverage_invoker_view OFFSET 0) AS derived');
SELECT pg_temp.coverage_probe('routine definer', 'SELECT value FROM (SELECT coverage_definer() AS value OFFSET 0) AS derived');
SELECT pg_temp.coverage_probe('routine invoker denied', 'SELECT value FROM (SELECT coverage_invoker() AS value OFFSET 0) AS derived');
RESET ROLE;
ROLLBACK;
