-- PostgreSQL 18 reference for one rewrite plan, typed assignment and savepoint rollback.
BEGIN;
CREATE SCHEMA generated_rewrite_oracle;
SET search_path = generated_rewrite_oracle, public;
CREATE TABLE typed(id int PRIMARY KEY, source char(4));
INSERT INTO typed VALUES(1,'a'),(2,'bc'),(3,NULL);
ALTER TABLE typed ADD COLUMN as_text text GENERATED ALWAYS AS (source) STORED;
SELECT id,quote_nullable(as_text),length(as_text) FROM typed ORDER BY id;
CREATE SEQUENCE calls;
CREATE FUNCTION folded() RETURNS bigint IMMUTABLE LANGUAGE sql AS 'SELECT nextval(''generated_rewrite_oracle.calls'')';
ALTER TABLE typed ADD COLUMN folded_value bigint GENERATED ALWAYS AS (folded()) STORED;
SELECT count(*), min(folded_value),max(folded_value),last_value FROM typed CROSS JOIN calls GROUP BY last_value;
SAVEPOINT before_rewrite;
ALTER TABLE typed ALTER COLUMN as_text SET EXPRESSION AS (upper(source));
SELECT id,as_text FROM typed ORDER BY id;
ROLLBACK TO before_rewrite;
SELECT id,as_text FROM typed ORDER BY id;
ROLLBACK;
