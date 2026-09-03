\set ON_ERROR_STOP on
\pset tuples_only on
\pset format unaligned

DROP SCHEMA IF EXISTS uqa_rule_condition_oracle CASCADE;
DROP SCHEMA IF EXISTS uqa_rule_condition_first CASCADE;
DROP SCHEMA IF EXISTS uqa_rule_condition_second CASCADE;
DROP ROLE IF EXISTS uqa_rule_condition_owner;
DROP ROLE IF EXISTS uqa_rule_condition_caller;

CREATE ROLE uqa_rule_condition_owner;
CREATE ROLE uqa_rule_condition_caller;
CREATE SCHEMA uqa_rule_condition_oracle;

CREATE OR REPLACE FUNCTION pg_temp.rule_condition_probe(label text, role_name text, command text)
RETURNS text
LANGUAGE plpgsql
AS $oracle$
DECLARE
    state text;
BEGIN
    EXECUTE format('SET ROLE %I', role_name);
    EXECUTE command;
    RESET ROLE;
    RETURN label || '|ok';
EXCEPTION WHEN OTHERS THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE;
    RESET ROLE;
    RETURN label || '|' || state;
END
$oracle$;

CREATE TABLE uqa_rule_condition_oracle.items(id integer, payload integer);
CREATE TABLE uqa_rule_condition_oracle.lookup(id integer);
CREATE TABLE uqa_rule_condition_oracle.log(kind text, id integer);
INSERT INTO uqa_rule_condition_oracle.lookup VALUES (2);
CREATE RULE a_exists_constant AS ON INSERT TO uqa_rule_condition_oracle.items WHERE EXISTS (SELECT 1) DO ALSO INSERT INTO uqa_rule_condition_oracle.log VALUES ('constant', NEW.id);
CREATE RULE b_exists_correlated AS ON INSERT TO uqa_rule_condition_oracle.items WHERE EXISTS (SELECT 1 WHERE NEW.id > 0) DO ALSO INSERT INTO uqa_rule_condition_oracle.log VALUES ('correlated', NEW.id);
CREATE RULE c_scalar_correlated AS ON INSERT TO uqa_rule_condition_oracle.items WHERE (SELECT NEW.id > 1) DO ALSO INSERT INTO uqa_rule_condition_oracle.log VALUES ('scalar', NEW.id);
CREATE RULE d_in_subquery AS ON INSERT TO uqa_rule_condition_oracle.items WHERE NEW.id IN (SELECT 2) DO ALSO INSERT INTO uqa_rule_condition_oracle.log VALUES ('in', NEW.id);
CREATE RULE e_external_relation AS ON INSERT TO uqa_rule_condition_oracle.items WHERE EXISTS (SELECT 1 FROM uqa_rule_condition_oracle.lookup AS candidate WHERE candidate.id = NEW.id) DO ALSO INSERT INTO uqa_rule_condition_oracle.log VALUES ('external', NEW.id);
CREATE RULE f_local_unqualified AS ON INSERT TO uqa_rule_condition_oracle.items WHERE EXISTS (SELECT 1 FROM uqa_rule_condition_oracle.lookup WHERE id = 2) DO ALSO INSERT INTO uqa_rule_condition_oracle.log VALUES ('local', NEW.id);
CREATE RULE g_local_new_alias AS ON INSERT TO uqa_rule_condition_oracle.items WHERE EXISTS (SELECT 1 FROM uqa_rule_condition_oracle.lookup AS new WHERE new.id = 2) DO ALSO INSERT INTO uqa_rule_condition_oracle.log VALUES ('shadow', NEW.id);
INSERT INTO uqa_rule_condition_oracle.items VALUES (0, 10), (2, 20);
SELECT 'basic|' || string_agg(kind || ':' || id, ',' ORDER BY id, kind) FROM uqa_rule_condition_oracle.log;

CREATE TABLE uqa_rule_condition_oracle.timing_also(id integer);
CREATE TABLE uqa_rule_condition_oracle.timing_also_log(id integer);
CREATE RULE timing_also_rule AS ON INSERT TO uqa_rule_condition_oracle.timing_also WHERE NOT EXISTS (SELECT 1 FROM uqa_rule_condition_oracle.timing_also AS seen WHERE seen.id = NEW.id) DO ALSO INSERT INTO uqa_rule_condition_oracle.timing_also_log VALUES (NEW.id);
INSERT INTO uqa_rule_condition_oracle.timing_also VALUES (1);
SELECT 'timing-also|' || (SELECT count(*) FROM uqa_rule_condition_oracle.timing_also) || '|' || (SELECT count(*) FROM uqa_rule_condition_oracle.timing_also_log);

CREATE TABLE uqa_rule_condition_oracle.timing_instead(id integer);
CREATE TABLE uqa_rule_condition_oracle.timing_instead_log(id integer);
CREATE RULE timing_instead_rule AS ON INSERT TO uqa_rule_condition_oracle.timing_instead WHERE EXISTS (SELECT 1 FROM uqa_rule_condition_oracle.timing_instead AS seen WHERE seen.id = NEW.id) DO INSTEAD INSERT INTO uqa_rule_condition_oracle.timing_instead_log VALUES (NEW.id);
INSERT INTO uqa_rule_condition_oracle.timing_instead VALUES (1);
SELECT 'timing-instead|' || (SELECT count(*) FROM uqa_rule_condition_oracle.timing_instead) || '|' || (SELECT count(*) FROM uqa_rule_condition_oracle.timing_instead_log);

CREATE TABLE uqa_rule_condition_oracle.mutation_items(id integer PRIMARY KEY, value integer);
CREATE TABLE uqa_rule_condition_oracle.mutation_lookup(id integer);
CREATE TABLE uqa_rule_condition_oracle.mutation_log(entry text);
INSERT INTO uqa_rule_condition_oracle.mutation_items VALUES (1, 10), (2, 20);
INSERT INTO uqa_rule_condition_oracle.mutation_lookup VALUES (2);
CREATE RULE mutation_update AS ON UPDATE TO uqa_rule_condition_oracle.mutation_items WHERE EXISTS (WITH delta AS (SELECT NEW.value - OLD.value AS amount) SELECT 1 FROM delta, uqa_rule_condition_oracle.mutation_lookup AS lookup WHERE amount > 0 AND lookup.id = NEW.id) DO ALSO INSERT INTO uqa_rule_condition_oracle.mutation_log VALUES ('update:' || OLD.value || ':' || NEW.value);
CREATE RULE mutation_delete AS ON DELETE TO uqa_rule_condition_oracle.mutation_items WHERE OLD.id IN (SELECT id FROM uqa_rule_condition_oracle.mutation_lookup) DO INSTEAD INSERT INTO uqa_rule_condition_oracle.mutation_log VALUES ('retain:' || OLD.id);
UPDATE uqa_rule_condition_oracle.mutation_items SET value = value + 1;
DELETE FROM uqa_rule_condition_oracle.mutation_items;
SELECT 'mutation|' || string_agg(entry, ',' ORDER BY entry) FROM uqa_rule_condition_oracle.mutation_log;
SELECT 'mutation-survivor|' || string_agg(id::text, ',' ORDER BY id) FROM uqa_rule_condition_oracle.mutation_items;

CREATE TABLE uqa_rule_condition_oracle.error_items(id integer);
SELECT pg_temp.rule_condition_probe('wrong-type', 'postgres', 'CREATE RULE wrong_type AS ON INSERT TO uqa_rule_condition_oracle.error_items WHERE (SELECT 1) DO NOTHING');
SELECT pg_temp.rule_condition_probe('missing-relation', 'postgres', 'CREATE RULE missing_relation AS ON INSERT TO uqa_rule_condition_oracle.error_items WHERE EXISTS (SELECT 1 FROM uqa_rule_condition_oracle.absent) DO NOTHING');
SELECT pg_temp.rule_condition_probe('invalid-old', 'postgres', 'CREATE RULE invalid_old AS ON INSERT TO uqa_rule_condition_oracle.error_items WHERE EXISTS (SELECT 1 WHERE OLD.id = 1) DO NOTHING');

CREATE TABLE uqa_rule_condition_oracle.cardinality_items(id integer);
CREATE TABLE uqa_rule_condition_oracle.cardinality_log(id integer);
CREATE RULE cardinality_rule AS ON INSERT TO uqa_rule_condition_oracle.cardinality_items WHERE (SELECT accepted FROM (VALUES (true), (false)) AS candidates(accepted)) DO ALSO INSERT INTO uqa_rule_condition_oracle.cardinality_log VALUES (NEW.id);
SELECT pg_temp.rule_condition_probe('scalar-cardinality', 'postgres', 'INSERT INTO uqa_rule_condition_oracle.cardinality_items VALUES (1)');
SELECT 'scalar-atomic|' || (SELECT count(*) FROM uqa_rule_condition_oracle.cardinality_items) || '|' || (SELECT count(*) FROM uqa_rule_condition_oracle.cardinality_log);

CREATE SCHEMA uqa_rule_condition_first;
CREATE SCHEMA uqa_rule_condition_second;
CREATE TABLE uqa_rule_condition_first.lookup(id integer);
CREATE TABLE uqa_rule_condition_second.lookup(id integer);
CREATE TABLE uqa_rule_condition_first.items(id integer);
CREATE TABLE uqa_rule_condition_first.log(id integer);
INSERT INTO uqa_rule_condition_first.lookup VALUES (1);
INSERT INTO uqa_rule_condition_second.lookup VALUES (2);
SET search_path = uqa_rule_condition_first, public;
CREATE RULE bound_rule AS ON INSERT TO items WHERE EXISTS (SELECT 1 FROM lookup WHERE id = NEW.id) DO ALSO INSERT INTO uqa_rule_condition_first.log VALUES (NEW.id);
SET search_path = uqa_rule_condition_second, public;
INSERT INTO uqa_rule_condition_first.items VALUES (1), (2);
SELECT 'bound-name|' || COALESCE(string_agg(id::text, ',' ORDER BY id), '') FROM uqa_rule_condition_first.log;
RESET search_path;

CREATE TABLE uqa_rule_condition_oracle.rename_items(id integer);
CREATE TABLE uqa_rule_condition_oracle.rename_lookup(id integer);
CREATE TABLE uqa_rule_condition_oracle.rename_log(id integer);
INSERT INTO uqa_rule_condition_oracle.rename_lookup VALUES (1);
CREATE RULE rename_rule AS ON INSERT TO uqa_rule_condition_oracle.rename_items WHERE EXISTS (SELECT 1 FROM uqa_rule_condition_oracle.rename_lookup WHERE id = 1 AND NEW.id > 0) DO ALSO INSERT INTO uqa_rule_condition_oracle.rename_log VALUES (NEW.id);
ALTER TABLE uqa_rule_condition_oracle.rename_items RENAME COLUMN id TO item_id;
INSERT INTO uqa_rule_condition_oracle.rename_items(item_id) VALUES (2);
SELECT 'rename-row|' || string_agg(id::text, ',' ORDER BY id) FROM uqa_rule_condition_oracle.rename_log;
SELECT 'rename-definition|' || (pg_get_ruledef(oid, true) LIKE '%new.item_id%') || '|' || (pg_get_ruledef(oid, true) LIKE '%id = 1%') FROM pg_rewrite WHERE rulename = 'rename_rule';

ALTER SCHEMA uqa_rule_condition_oracle OWNER TO uqa_rule_condition_owner;
SET ROLE uqa_rule_condition_owner;
CREATE TABLE uqa_rule_condition_oracle.owner_lookup(id integer);
CREATE TABLE uqa_rule_condition_oracle.owner_event(id integer);
CREATE TABLE uqa_rule_condition_oracle.caller_event(id integer);
CREATE TABLE uqa_rule_condition_oracle.owner_function_event(id integer);
CREATE TABLE uqa_rule_condition_oracle.caller_function_event(id integer);
CREATE TABLE uqa_rule_condition_oracle.security_log(kind text, id integer);
CREATE FUNCTION uqa_rule_condition_oracle.owner_only() RETURNS boolean LANGUAGE SQL AS 'SELECT true';
CREATE FUNCTION uqa_rule_condition_oracle.caller_only() RETURNS boolean LANGUAGE SQL AS 'SELECT true';
REVOKE ALL ON FUNCTION uqa_rule_condition_oracle.owner_only(), uqa_rule_condition_oracle.caller_only() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION uqa_rule_condition_oracle.caller_only() TO uqa_rule_condition_caller;
INSERT INTO uqa_rule_condition_oracle.owner_lookup VALUES (1);
REVOKE ALL ON uqa_rule_condition_oracle.owner_lookup FROM PUBLIC;
GRANT INSERT ON uqa_rule_condition_oracle.owner_event, uqa_rule_condition_oracle.caller_event, uqa_rule_condition_oracle.owner_function_event, uqa_rule_condition_oracle.caller_function_event TO uqa_rule_condition_caller;
RESET ROLE;
GRANT USAGE, CREATE ON SCHEMA uqa_rule_condition_oracle TO uqa_rule_condition_caller;
SET ROLE uqa_rule_condition_caller;
CREATE TABLE uqa_rule_condition_oracle.caller_lookup(id integer);
INSERT INTO uqa_rule_condition_oracle.caller_lookup VALUES (1);
REVOKE ALL ON uqa_rule_condition_oracle.caller_lookup FROM PUBLIC;
RESET ROLE;
SET ROLE uqa_rule_condition_owner;
CREATE RULE owner_condition AS ON INSERT TO uqa_rule_condition_oracle.owner_event WHERE EXISTS (SELECT 1 FROM uqa_rule_condition_oracle.owner_lookup WHERE id = NEW.id) DO ALSO INSERT INTO uqa_rule_condition_oracle.security_log VALUES ('owner', NEW.id);
CREATE RULE caller_condition AS ON INSERT TO uqa_rule_condition_oracle.caller_event WHERE EXISTS (SELECT 1 FROM uqa_rule_condition_oracle.caller_lookup WHERE id = NEW.id) DO ALSO INSERT INTO uqa_rule_condition_oracle.security_log VALUES ('caller', NEW.id);
CREATE RULE owner_function_condition AS ON INSERT TO uqa_rule_condition_oracle.owner_function_event WHERE EXISTS (SELECT 1 WHERE uqa_rule_condition_oracle.owner_only()) DO ALSO INSERT INTO uqa_rule_condition_oracle.security_log VALUES ('owner-function', NEW.id);
CREATE RULE caller_function_condition AS ON INSERT TO uqa_rule_condition_oracle.caller_function_event WHERE EXISTS (SELECT 1 WHERE uqa_rule_condition_oracle.caller_only()) DO ALSO INSERT INTO uqa_rule_condition_oracle.security_log VALUES ('caller-function', NEW.id);
RESET ROLE;
SELECT pg_temp.rule_condition_probe('owner-condition-source', 'uqa_rule_condition_caller', 'INSERT INTO uqa_rule_condition_oracle.owner_event VALUES (1)');
SELECT pg_temp.rule_condition_probe('caller-condition-source', 'uqa_rule_condition_caller', 'INSERT INTO uqa_rule_condition_oracle.caller_event VALUES (1)');
SELECT pg_temp.rule_condition_probe('owner-condition-function', 'uqa_rule_condition_caller', 'INSERT INTO uqa_rule_condition_oracle.owner_function_event VALUES (1)');
SELECT pg_temp.rule_condition_probe('caller-condition-function', 'uqa_rule_condition_caller', 'INSERT INTO uqa_rule_condition_oracle.caller_function_event VALUES (1)');
SELECT 'security-log|' || COALESCE(string_agg(kind || ':' || id, ',' ORDER BY kind), '') FROM uqa_rule_condition_oracle.security_log;
SELECT 'security-atomic|' || count(*) FROM uqa_rule_condition_oracle.caller_event;
SELECT 'function-atomic|' || count(*) FROM uqa_rule_condition_oracle.owner_function_event;

DROP SCHEMA uqa_rule_condition_first CASCADE;
DROP SCHEMA uqa_rule_condition_second CASCADE;
DROP SCHEMA uqa_rule_condition_oracle CASCADE;
DROP OWNED BY uqa_rule_condition_owner;
DROP OWNED BY uqa_rule_condition_caller;
DROP ROLE uqa_rule_condition_owner;
DROP ROLE uqa_rule_condition_caller;
