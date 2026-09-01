DROP SCHEMA IF EXISTS uqa_sequence_security_oracle CASCADE;
DROP ROLE IF EXISTS uqa_sequence_acl_reader;
DROP ROLE IF EXISTS uqa_sequence_acl_delegate;
DROP ROLE IF EXISTS uqa_sequence_acl_owner;
DROP ROLE IF EXISTS uqa_sequence_acl_new_owner;

CREATE ROLE uqa_sequence_acl_owner;
CREATE ROLE uqa_sequence_acl_new_owner;
CREATE ROLE uqa_sequence_acl_delegate;
CREATE ROLE uqa_sequence_acl_reader;
CREATE SCHEMA uqa_sequence_security_oracle AUTHORIZATION uqa_sequence_acl_owner;
GRANT USAGE ON SCHEMA uqa_sequence_security_oracle TO uqa_sequence_acl_new_owner, uqa_sequence_acl_delegate, uqa_sequence_acl_reader;
GRANT CREATE ON SCHEMA uqa_sequence_security_oracle TO uqa_sequence_acl_new_owner;

SET ROLE uqa_sequence_acl_owner;
CREATE SEQUENCE uqa_sequence_security_oracle.ids;
SELECT 'default', relacl IS NULL, pg_get_userbyid(relowner) FROM pg_catalog.pg_class WHERE oid = 'uqa_sequence_security_oracle.ids'::regclass;
GRANT USAGE ON SEQUENCE uqa_sequence_security_oracle.ids TO uqa_sequence_acl_delegate WITH GRANT OPTION;
RESET ROLE;

SET ROLE uqa_sequence_acl_delegate;
GRANT USAGE ON SEQUENCE uqa_sequence_security_oracle.ids TO uqa_sequence_acl_reader;
RESET ROLE;

SELECT 'explicit', relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_sequence_security_oracle.ids'::regclass;
SELECT 'explicit-overloads', has_sequence_privilege('uqa_sequence_acl_reader', 'uqa_sequence_security_oracle.ids', 'USAGE'), has_sequence_privilege('uqa_sequence_acl_reader', 'uqa_sequence_security_oracle.ids'::regclass, 'USAGE'), has_sequence_privilege('uqa_sequence_acl_reader'::regrole::oid, 'uqa_sequence_security_oracle.ids', 'USAGE'), has_sequence_privilege('uqa_sequence_acl_reader'::regrole::oid, 'uqa_sequence_security_oracle.ids'::regclass, 'USAGE');

SET ROLE uqa_sequence_acl_reader;
SELECT 'current-overloads', has_sequence_privilege('uqa_sequence_security_oracle.ids', 'USAGE'), has_sequence_privilege('uqa_sequence_security_oracle.ids'::regclass, 'USAGE');
SELECT 'values', nextval('uqa_sequence_security_oracle.ids'), currval('uqa_sequence_security_oracle.ids'), lastval();
RESET ROLE;

SET ROLE uqa_sequence_acl_owner;
REVOKE GRANT OPTION FOR USAGE ON SEQUENCE uqa_sequence_security_oracle.ids FROM uqa_sequence_acl_delegate CASCADE;
RESET ROLE;
SELECT 'cascade', has_sequence_privilege('uqa_sequence_acl_delegate', 'uqa_sequence_security_oracle.ids', 'USAGE'), has_sequence_privilege('uqa_sequence_acl_delegate', 'uqa_sequence_security_oracle.ids', 'USAGE WITH GRANT OPTION'), has_sequence_privilege('uqa_sequence_acl_reader', 'uqa_sequence_security_oracle.ids', 'USAGE');

SET ROLE uqa_sequence_acl_owner;
REVOKE ALL PRIVILEGES ON SEQUENCE uqa_sequence_security_oracle.ids FROM uqa_sequence_acl_owner;
SELECT 'implicit-owner', has_sequence_privilege('uqa_sequence_security_oracle.ids', 'SELECT WITH GRANT OPTION, UPDATE WITH GRANT OPTION, USAGE WITH GRANT OPTION');
GRANT ALL PRIVILEGES ON SEQUENCE uqa_sequence_security_oracle.ids TO uqa_sequence_acl_new_owner WITH GRANT OPTION;
RESET ROLE;
GRANT uqa_sequence_acl_new_owner TO uqa_sequence_acl_owner WITH INHERIT FALSE, SET TRUE;

SET ROLE uqa_sequence_acl_owner;
ALTER SEQUENCE uqa_sequence_security_oracle.ids OWNER TO uqa_sequence_acl_new_owner;
RESET ROLE;
SELECT 'transfer', pg_get_userbyid(relowner), relacl::text FROM pg_catalog.pg_class WHERE oid = 'uqa_sequence_security_oracle.ids'::regclass;

DROP SCHEMA uqa_sequence_security_oracle CASCADE;
DROP ROLE uqa_sequence_acl_reader;
DROP ROLE uqa_sequence_acl_delegate;
DROP ROLE uqa_sequence_acl_owner;
DROP ROLE uqa_sequence_acl_new_owner;
