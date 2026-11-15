-- PostgreSQL 18 reference for captured routine bindings, lazy defaults, renamed columns and typed mutation results.
CREATE TABLE binding_rows(id integer PRIMARY KEY, payload text);
INSERT INTO binding_rows VALUES(7,'seven'),(8,'eight');
CREATE SEQUENCE binding_ticket START 7;
CREATE FUNCTION binding_read(n integer DEFAULT nextval('binding_ticket')::integer) RETURNS TABLE(id integer,label text) LANGUAGE SQL BEGIN ATOMIC SELECT id,payload FROM binding_rows WHERE id=n; END;
CREATE FUNCTION binding_update(n integer) RETURNS integer LANGUAGE SQL BEGIN ATOMIC UPDATE binding_rows SET payload=payload || '!' WHERE id=n RETURNING id; END;
CREATE FUNCTION binding_divide(n integer) RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT 10/n; END;
CREATE FUNCTION binding_division_state() RETURNS text LANGUAGE plpgsql AS $$ BEGIN PERFORM binding_divide(0); RETURN 'unexpected'; EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE; END $$;
ALTER TABLE binding_rows RENAME COLUMN payload TO label;
-- capture boundary
SELECT is_called FROM binding_ticket;
SELECT id::text || ':' || label FROM binding_read();
SELECT id::text || ':' || label FROM binding_read();
SELECT binding_divide(2);
SELECT binding_division_state();
SELECT binding_update(7);
SELECT label FROM binding_rows WHERE id=7;
