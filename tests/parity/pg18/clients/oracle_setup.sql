\set ON_ERROR_STOP on

DO $setup$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'uqa_matrix') THEN
        CREATE ROLE uqa_matrix LOGIN PASSWORD 'uqa-matrix-password';
    ELSE
        ALTER ROLE uqa_matrix LOGIN PASSWORD 'uqa-matrix-password';
    END IF;
END
$setup$;

GRANT CONNECT, TEMPORARY ON DATABASE postgres TO uqa_matrix;
