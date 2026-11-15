-- PostgreSQL 18 independently rejects incompatible defaults, absent columns and incompatible SQL result types.
BEGIN;
CREATE SCHEMA backup_binding_errors;
SET LOCAL search_path=backup_binding_errors,public;
CREATE FUNCTION observed(sql text) RETURNS text LANGUAGE plpgsql AS $$
BEGIN EXECUTE sql; RETURN 'accepted'; EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE; END
$$;
SELECT observed('CREATE FUNCTION wrong_default(n integer DEFAULT true) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT n; END');
SELECT observed('CREATE FUNCTION missing_column(n integer) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT missing_column; END');
SELECT observed('CREATE FUNCTION wrong_result(n integer) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT true; END');
ROLLBACK;
