CREATE EXTENSION IF NOT EXISTS dblink;
DROP SCHEMA IF EXISTS uqa_sequence_persistence_session_oracle CASCADE;
CREATE SCHEMA uqa_sequence_persistence_session_oracle;
CREATE SEQUENCE uqa_sequence_persistence_session_oracle.changed_ids CACHE 3;
CREATE SEQUENCE uqa_sequence_persistence_session_oracle.unchanged_ids CACHE 3;

DO $oracle$
BEGIN
    PERFORM dblink_connect('sequence_persistence_peer', 'dbname=postgres');
END
$oracle$;

SELECT 'seed', nextval('uqa_sequence_persistence_session_oracle.changed_ids'), nextval('uqa_sequence_persistence_session_oracle.unchanged_ids');
SELECT 'peer', dblink_exec('sequence_persistence_peer', 'ALTER SEQUENCE uqa_sequence_persistence_session_oracle.changed_ids SET UNLOGGED'), dblink_exec('sequence_persistence_peer', 'ALTER SEQUENCE uqa_sequence_persistence_session_oracle.unchanged_ids SET LOGGED');
SELECT 'after', nextval('uqa_sequence_persistence_session_oracle.changed_ids'), currval('uqa_sequence_persistence_session_oracle.changed_ids'), nextval('uqa_sequence_persistence_session_oracle.unchanged_ids'), currval('uqa_sequence_persistence_session_oracle.unchanged_ids');
SELECT relname, relpersistence FROM pg_catalog.pg_class WHERE relnamespace = 'uqa_sequence_persistence_session_oracle'::regnamespace AND relname IN ('changed_ids', 'unchanged_ids') ORDER BY relname;

DO $oracle$
BEGIN
    PERFORM dblink_disconnect('sequence_persistence_peer');
END
$oracle$;

DROP SCHEMA uqa_sequence_persistence_session_oracle CASCADE;
