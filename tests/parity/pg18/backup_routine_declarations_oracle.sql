-- PostgreSQL 18 reference for routine parameter layouts, array domains and deferred source bodies.
CREATE DOMAIN declaration_int AS integer;
CREATE FUNCTION declaration_same_name(IN n integer, OUT n integer) LANGUAGE sql AS 'SELECT $1 + 1';
CREATE FUNCTION declaration_variadic(VARIADIC n integer[], OUT total integer) LANGUAGE sql AS 'SELECT cardinality($1)';
CREATE FUNCTION declaration_domain(VARIADIC n declaration_int[]) RETURNS integer LANGUAGE sql AS 'SELECT cardinality($1)';
CREATE FUNCTION declaration_polymorphic(n anyelement) RETURNS anyelement LANGUAGE sql AS 'SELECT $1';
SET check_function_bodies = off;
CREATE FUNCTION declaration_lazy() RETURNS integer LANGUAGE sql AS 'SELECT declaration_missing()';
CREATE FUNCTION declaration_trigger_argument(n integer) RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NULL; END';
CREATE FUNCTION declaration_lazy_state() RETURNS text LANGUAGE plpgsql AS $$ BEGIN PERFORM declaration_lazy(); RETURN 'unexpected'; EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE; END $$;
SET check_function_bodies = on;
CREATE FUNCTION declaration_estimate(n integer) RETURNS SETOF integer LANGUAGE sql COST 1e40 ROWS 1e40 AS 'SELECT n';
CREATE FUNCTION declaration_supported(n numeric) RETURNS numeric LANGUAGE sql SUPPORT numeric_support AS 'SELECT n';
CREATE ROLE declaration_owner;
ALTER FUNCTION declaration_supported(numeric) OWNER TO declaration_owner;
-- capture boundary
SELECT declaration_same_name(4);
SELECT declaration_variadic(2,3,4);
SELECT declaration_domain(VARIADIC ARRAY[2,3]::declaration_int[]);
SELECT declaration_polymorphic(7);
SELECT declaration_polymorphic('x'::text);
SELECT declaration_lazy_state();
SELECT declaration_estimate(9);
