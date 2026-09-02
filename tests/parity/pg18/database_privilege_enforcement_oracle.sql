\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP DATABASE IF EXISTS uqa_database_enforcement_oracle;
DROP ROLE IF EXISTS uqa_database_enforcement_member;
DROP ROLE IF EXISTS uqa_database_enforcement_creator;
DROP ROLE IF EXISTS uqa_database_enforcement_outsider;

CREATE ROLE uqa_database_enforcement_outsider;
CREATE ROLE uqa_database_enforcement_creator;
CREATE ROLE uqa_database_enforcement_member INHERIT;
GRANT uqa_database_enforcement_creator TO uqa_database_enforcement_member;
CREATE DATABASE uqa_database_enforcement_oracle;

\connect uqa_database_enforcement_oracle

CREATE SCHEMA existing_schema;
REVOKE CREATE, TEMPORARY ON DATABASE uqa_database_enforcement_oracle FROM PUBLIC;

CREATE OR REPLACE FUNCTION public.database_enforcement_probe(label text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
BEGIN
    EXECUTE command;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE;
    RETURN label || '|' || state;
END
$oracle$;

SET ROLE uqa_database_enforcement_outsider;
SELECT public.database_enforcement_probe('schema-create-denied', 'CREATE SCHEMA denied_schema');
SELECT public.database_enforcement_probe('schema-existing-denied', 'CREATE SCHEMA existing_schema');
SELECT public.database_enforcement_probe('schema-existing-if-not-exists-denied', 'CREATE SCHEMA IF NOT EXISTS existing_schema');
SELECT public.database_enforcement_probe('schema-reserved-denied', 'CREATE SCHEMA pg_denied');
SELECT public.database_enforcement_probe('temp-table-denied', 'CREATE TEMP TABLE denied_temp_table(id integer)');
SELECT public.database_enforcement_probe('temp-sequence-denied', 'CREATE TEMP SEQUENCE denied_temp_sequence');
SELECT public.database_enforcement_probe('temp-sequence-invalid', 'CREATE TEMP SEQUENCE denied_invalid_sequence INCREMENT 0');
SELECT public.database_enforcement_probe('temp-view-denied', 'CREATE TEMP VIEW denied_temp_view AS SELECT 1 AS id');
SELECT public.database_enforcement_probe('temp-view-missing-source', 'CREATE TEMP VIEW denied_missing_view AS SELECT * FROM denied_missing_source');
SELECT public.database_enforcement_probe('temp-view-duplicate-columns', 'CREATE TEMP VIEW denied_duplicate_view(id, id) AS SELECT 1, 2');
SELECT public.database_enforcement_probe('temp-ctas-denied', 'CREATE TEMP TABLE denied_temp_ctas AS SELECT 1 AS id');
SELECT public.database_enforcement_probe('temp-ctas-missing-source', 'CREATE TEMP TABLE denied_missing_ctas AS SELECT * FROM denied_missing_source');
SELECT public.database_enforcement_probe('temp-ctas-duplicate-columns', 'CREATE TEMP TABLE denied_duplicate_ctas AS SELECT 1 AS id, 2 AS id');
SELECT public.database_enforcement_probe('temp-select-into-denied', 'SELECT 1 AS id INTO TEMP denied_temp_select_into');
SELECT public.database_enforcement_probe('temp-qualified-denied', 'CREATE TEMP TABLE public.denied_temp_qualified(id integer)');
SELECT public.database_enforcement_probe('temp-definition-denied', 'CREATE TEMP TABLE denied_temp_definition(id integer, id integer)');
RESET ROLE;

GRANT CREATE, TEMPORARY ON DATABASE uqa_database_enforcement_oracle TO uqa_database_enforcement_outsider;
GRANT CREATE, TEMPORARY ON DATABASE uqa_database_enforcement_oracle TO uqa_database_enforcement_creator;

SET ROLE uqa_database_enforcement_outsider;
SELECT public.database_enforcement_probe('schema-create-granted', 'CREATE SCHEMA granted_schema');
SELECT public.database_enforcement_probe('temp-table-granted', 'CREATE TEMP TABLE granted_temp_table(id integer)');
CREATE INDEX granted_temp_index ON granted_temp_table(id);
RESET ROLE;

REVOKE CREATE, TEMPORARY ON DATABASE uqa_database_enforcement_oracle FROM uqa_database_enforcement_outsider;
SET ROLE uqa_database_enforcement_outsider;
SELECT public.database_enforcement_probe('schema-after-revoke', 'CREATE SCHEMA revoked_schema');
SELECT public.database_enforcement_probe('schema-existing-if-not-exists-after-revoke', 'CREATE SCHEMA IF NOT EXISTS granted_schema');
SELECT public.database_enforcement_probe('temp-after-revoke-allocated', 'CREATE TEMP TABLE allocated_after_revoke(id integer)');
SELECT public.database_enforcement_probe('temp-index-after-revoke', 'CREATE INDEX denied_index_after_revoke ON granted_temp_table(id)');
SELECT public.database_enforcement_probe('temp-index-existing-after-revoke', 'CREATE INDEX granted_temp_index ON granted_temp_table(id)');
SELECT public.database_enforcement_probe('temp-index-existing-if-not-exists-after-revoke', 'CREATE INDEX IF NOT EXISTS granted_temp_index ON granted_temp_table(id)');
SELECT public.database_enforcement_probe('temp-index-missing-column-after-revoke', 'CREATE INDEX denied_missing_column_index ON granted_temp_table(missing)');
SELECT public.database_enforcement_probe('temp-index-missing-table-after-revoke', 'CREATE INDEX denied_missing_table_index ON denied_missing_table(id)');
SELECT public.database_enforcement_probe('temp-index-invalid-method-after-revoke', 'CREATE INDEX denied_invalid_method_index ON granted_temp_table USING denied_method(id)');
SELECT public.database_enforcement_probe('temp-unique-after-revoke', 'ALTER TABLE granted_temp_table ADD UNIQUE(id)');
SELECT public.database_enforcement_probe('temp-unique-duplicate-name-after-revoke', 'ALTER TABLE granted_temp_table ADD CONSTRAINT granted_temp_index UNIQUE(id)');
SELECT public.database_enforcement_probe('temp-unique-missing-column-after-revoke', 'ALTER TABLE granted_temp_table ADD UNIQUE(missing)');
SELECT public.database_enforcement_probe('temp-unique-column-after-revoke', 'ALTER TABLE granted_temp_table ADD COLUMN indexed_value integer UNIQUE');
RESET ROLE;

DISCARD TEMP;
SET ROLE uqa_database_enforcement_outsider;
SELECT public.database_enforcement_probe('temp-after-discard', 'CREATE TEMP TABLE denied_after_discard(id integer)');
RESET ROLE;

SET ROLE uqa_database_enforcement_member;
SELECT public.database_enforcement_probe('schema-inherited', 'CREATE SCHEMA inherited_schema');
SELECT public.database_enforcement_probe('temp-inherited', 'CREATE TEMP TABLE inherited_temp_table(id integer)');
RESET ROLE;

\connect postgres

DROP DATABASE uqa_database_enforcement_oracle;
DROP ROLE uqa_database_enforcement_member;
DROP ROLE uqa_database_enforcement_creator;
DROP ROLE uqa_database_enforcement_outsider;
