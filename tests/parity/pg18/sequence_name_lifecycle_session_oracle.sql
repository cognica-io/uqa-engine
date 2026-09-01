CREATE EXTENSION IF NOT EXISTS dblink;
DROP SCHEMA IF EXISTS uqa_sequence_name_lifecycle_session_oracle CASCADE;
DROP SCHEMA IF EXISTS uqa_sequence_name_lifecycle_archive CASCADE;
CREATE SCHEMA uqa_sequence_name_lifecycle_session_oracle;
CREATE SCHEMA uqa_sequence_name_lifecycle_archive;
CREATE SEQUENCE uqa_sequence_name_lifecycle_session_oracle.rename_ids CACHE 3;
CREATE SEQUENCE uqa_sequence_name_lifecycle_session_oracle.schema_ids CACHE 3;
CREATE TABLE uqa_sequence_name_lifecycle_session_oracle.saved_oids(rename_oid oid, schema_oid oid);
INSERT INTO uqa_sequence_name_lifecycle_session_oracle.saved_oids SELECT 'uqa_sequence_name_lifecycle_session_oracle.rename_ids'::regclass::oid, 'uqa_sequence_name_lifecycle_session_oracle.schema_ids'::regclass::oid;

DO $oracle$
BEGIN
    PERFORM dblink_connect('sequence_name_lifecycle_peer', 'dbname=postgres');
END
$oracle$;

SELECT 'seed', nextval('uqa_sequence_name_lifecycle_session_oracle.rename_ids'), nextval('uqa_sequence_name_lifecycle_session_oracle.schema_ids');
SELECT 'peer', dblink_exec('sequence_name_lifecycle_peer', 'ALTER SEQUENCE uqa_sequence_name_lifecycle_session_oracle.rename_ids RENAME TO renamed_ids'), dblink_exec('sequence_name_lifecycle_peer', 'ALTER SEQUENCE uqa_sequence_name_lifecycle_session_oracle.schema_ids SET SCHEMA uqa_sequence_name_lifecycle_archive');
SELECT 'current', currval('uqa_sequence_name_lifecycle_session_oracle.renamed_ids'), currval('uqa_sequence_name_lifecycle_archive.schema_ids'), lastval(), rename_oid = 'uqa_sequence_name_lifecycle_session_oracle.renamed_ids'::regclass::oid, schema_oid = 'uqa_sequence_name_lifecycle_archive.schema_ids'::regclass::oid FROM uqa_sequence_name_lifecycle_session_oracle.saved_oids;
SELECT 'after', nextval('uqa_sequence_name_lifecycle_session_oracle.renamed_ids'), currval('uqa_sequence_name_lifecycle_session_oracle.renamed_ids'), nextval('uqa_sequence_name_lifecycle_archive.schema_ids'), currval('uqa_sequence_name_lifecycle_archive.schema_ids'), lastval(), rename_oid = 'uqa_sequence_name_lifecycle_session_oracle.renamed_ids'::regclass::oid, schema_oid = 'uqa_sequence_name_lifecycle_archive.schema_ids'::regclass::oid FROM uqa_sequence_name_lifecycle_session_oracle.saved_oids;
SELECT 'oid_call', nextval(rename_oid::regclass), currval(rename_oid::regclass), lastval() FROM uqa_sequence_name_lifecycle_session_oracle.saved_oids;

DO $oracle$
BEGIN
    PERFORM dblink_disconnect('sequence_name_lifecycle_peer');
END
$oracle$;

DROP SCHEMA uqa_sequence_name_lifecycle_session_oracle CASCADE;
DROP SCHEMA uqa_sequence_name_lifecycle_archive CASCADE;
