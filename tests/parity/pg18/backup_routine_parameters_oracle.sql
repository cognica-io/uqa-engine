-- PostgreSQL 18 reference for retained SQL body parameters, procedure outputs and lazy source bodies.
CREATE TABLE routine_log(value integer);
CREATE FUNCTION record_input(n integer) RETURNS integer LANGUAGE SQL BEGIN ATOMIC INSERT INTO routine_log VALUES(n); SELECT n+1; END;
CREATE PROCEDURE add_inputs(IN first integer, OUT result integer, IN third integer) LANGUAGE SQL BEGIN ATOMIC SELECT $1+$2; END;
SET check_function_bodies=off;
CREATE FUNCTION lazy_missing() RETURNS integer LANGUAGE SQL AS 'SELECT absent_routine()';
SET check_function_bodies=on;
CREATE FUNCTION lazy_state() RETURNS text LANGUAGE plpgsql AS $$ BEGIN PERFORM lazy_missing(); RETURN 'unexpected'; EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE; END $$;
-- capture boundary
SELECT record_input(4) AS value;
CALL add_inputs(2,NULL,3);
SELECT string_agg(value::text,',' ORDER BY value) AS recorded FROM routine_log;
SELECT lazy_state() AS state;
