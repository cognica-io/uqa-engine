-- Unified Query Algebra
-- Copyright (c) 2023-2026 Cognica, Inc.
-- Captured with PostgreSQL 18.4 (postgres:18.4-alpine).
\set ON_ERROR_STOP on
BEGIN;
CREATE TEMP TABLE event_source(id integer);
CREATE TEMP TABLE event_target(id integer);
CREATE TEMP SEQUENCE event_ticket;
SELECT 'sequence_columns', array_agg(attname ORDER BY attnum) FROM pg_attribute WHERE attrelid = 'event_ticket'::regclass AND attnum > 0;
CREATE RULE saved AS ON INSERT TO event_source DO ALSO INSERT INTO event_target VALUES(new.id);
SELECT 'rule_owner', ev_class = 'event_source'::regclass FROM pg_rewrite WHERE rulename = 'saved' AND ev_class = 'event_source'::regclass;
INSERT INTO event_source VALUES(7);
SELECT 'rule_effect', id FROM event_target;
ALTER TABLE event_target RENAME TO event_target_renamed;
SELECT 'renamed_dependency', EXISTS(SELECT FROM pg_depend d JOIN pg_rewrite r ON d.classid = 'pg_rewrite'::regclass AND d.objid = r.oid WHERE r.rulename = 'saved' AND r.ev_class = 'event_source'::regclass AND d.refobjid = 'event_target_renamed'::regclass);
SAVEPOINT definitions;
\set ON_ERROR_STOP off
CREATE RULE missing_target AS ON INSERT TO event_source DO ALSO INSERT INTO absent VALUES(new.id);
\echo missing_target :SQLSTATE
ROLLBACK TO definitions;
CREATE RULE pseudo_source AS ON INSERT TO event_source DO ALSO SELECT id FROM new;
\echo pseudo_source :SQLSTATE
ROLLBACK TO definitions;
CREATE RULE concrete_cte AS ON INSERT TO event_source DO ALSO WITH x AS (SELECT id FROM event_target_renamed) SELECT * FROM x;
\echo concrete_cte :SQLSTATE
ROLLBACK TO definitions;
CREATE RULE missing_owner AS ON INSERT TO absent DO NOTHING;
\echo missing_owner :SQLSTATE
ROLLBACK TO definitions;
\set ON_ERROR_STOP on
CREATE FUNCTION pg_temp.event_guard() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;
SAVEPOINT trigger_definition;
\set ON_ERROR_STOP off
CREATE TRIGGER invalid_column BEFORE UPDATE OF absent ON event_source FOR EACH ROW EXECUTE FUNCTION pg_temp.event_guard();
\echo missing_trigger_column :SQLSTATE
ROLLBACK TO trigger_definition;
\set ON_ERROR_STOP on
CREATE CONSTRAINT TRIGGER event_guard AFTER INSERT ON event_source FROM event_target_renamed FOR EACH ROW EXECUTE FUNCTION pg_temp.event_guard();
SELECT 'trigger_relations', tgrelid = 'event_source'::regclass, tgconstrrelid = 'event_target_renamed'::regclass FROM pg_trigger WHERE tgname = 'event_guard' AND tgrelid = 'event_source'::regclass;
ROLLBACK;
