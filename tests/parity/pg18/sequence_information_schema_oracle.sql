CREATE EXTENSION IF NOT EXISTS dblink;
CREATE TEMP SEQUENCE uqa_sequence_information_current_temp;
DO $oracle$
BEGIN
    PERFORM dblink_connect('sequence_information_peer', 'dbname=postgres');
    PERFORM dblink_exec('sequence_information_peer', 'CREATE TEMP SEQUENCE uqa_sequence_information_peer_temp');
END
$oracle$;
SELECT 'temporary-information', sequence_name FROM information_schema.sequences WHERE sequence_name LIKE 'uqa_sequence_information_%_temp' ORDER BY sequence_name;
SELECT 'temporary-pg', sequencename FROM pg_catalog.pg_sequences WHERE sequencename LIKE 'uqa_sequence_information_%_temp' ORDER BY sequencename;
DO $oracle$
BEGIN
    PERFORM dblink_disconnect('sequence_information_peer');
END
$oracle$;
DROP SEQUENCE uqa_sequence_information_current_temp;

DROP SCHEMA IF EXISTS uqa_sequence_information_oracle CASCADE;
DROP ROLE IF EXISTS uqa_sequence_information_reader;
DROP ROLE IF EXISTS uqa_sequence_information_member;
DROP ROLE IF EXISTS uqa_sequence_information_owner;

CREATE ROLE uqa_sequence_information_owner;
CREATE ROLE uqa_sequence_information_member INHERIT;
CREATE ROLE uqa_sequence_information_reader;
CREATE SCHEMA uqa_sequence_information_oracle AUTHORIZATION uqa_sequence_information_owner;
GRANT USAGE ON SCHEMA uqa_sequence_information_oracle TO uqa_sequence_information_reader;

SET ROLE uqa_sequence_information_owner;
CREATE SEQUENCE uqa_sequence_information_oracle.small_ids AS smallint INCREMENT BY -3 MINVALUE -30 MAXVALUE 12 START WITH 9 CYCLE CACHE 4;
CREATE TABLE uqa_sequence_information_oracle.generated_rows (
    smallserial_id smallserial,
    serial_id serial,
    bigserial_id bigserial,
    small_identity smallint GENERATED ALWAYS AS IDENTITY,
    integer_identity integer GENERATED ALWAYS AS IDENTITY,
    bigint_identity bigint GENERATED ALWAYS AS IDENTITY
);
SELECT 'owner', sequence_name, data_type, numeric_precision, numeric_precision_radix, numeric_scale, start_value, minimum_value, maximum_value, increment, cycle_option FROM information_schema.sequences WHERE sequence_schema = 'uqa_sequence_information_oracle' ORDER BY sequence_name;
RESET ROLE;

SET ROLE uqa_sequence_information_reader;
SELECT 'hidden', count(*) FROM information_schema.sequences WHERE sequence_schema = 'uqa_sequence_information_oracle';
RESET ROLE;

SET ROLE uqa_sequence_information_owner;
GRANT USAGE ON ALL SEQUENCES IN SCHEMA uqa_sequence_information_oracle TO uqa_sequence_information_reader;
RESET ROLE;

SET ROLE uqa_sequence_information_reader;
SELECT 'explicit', sequence_name, data_type, start_value, minimum_value, maximum_value, increment, cycle_option FROM information_schema.sequences WHERE sequence_schema = 'uqa_sequence_information_oracle' ORDER BY sequence_name;
RESET ROLE;

GRANT uqa_sequence_information_owner TO uqa_sequence_information_member;
SET ROLE uqa_sequence_information_member;
SELECT 'inherited-owner', count(*) FROM information_schema.sequences WHERE sequence_schema = 'uqa_sequence_information_oracle';
RESET ROLE;

DROP SCHEMA uqa_sequence_information_oracle CASCADE;
DROP ROLE uqa_sequence_information_reader;
DROP ROLE uqa_sequence_information_member;
DROP ROLE uqa_sequence_information_owner;
