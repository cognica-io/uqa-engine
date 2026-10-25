#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture UPDATE/DELETE rewrite-rule inputs from an independent PostgreSQL 18."""

import json
import os
import re
import subprocess
from pathlib import Path

command = json.loads(os.environ['PG_EXPLAIN_COMMAND']) + ['-XAtq']
version = subprocess.check_output(command + ['-c', 'SELECT version()'], text=True).strip()
if not version.startswith('PostgreSQL 18.'):
    raise RuntimeError(version)
base = "CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint,m bigint DEFAULT nextval('rule_ticks')); INSERT INTO rule_event VALUES(1,10),(2,20),(3,30); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text)"
cases = []
for event, original, action in [
    ('update-repeated-new', "UPDATE rule_event SET m=nextval('rule_ticks')", 'INSERT INTO rule_output(n,m) VALUES(NEW.m,NEW.m)'),
    ('update-independent-actions', "UPDATE rule_event SET m=nextval('rule_ticks')", "(INSERT INTO rule_output(n,m) VALUES(OLD.n,NEW.m); INSERT INTO rule_output(n,m) VALUES(OLD.n,NEW.m))"),
    ('update-unused-error', 'UPDATE rule_event SET m=1/(n-2)', 'INSERT INTO rule_output(n) VALUES(OLD.n)'),
    ('update-unused-assignment', "UPDATE rule_event SET m=nextval('rule_ticks')", 'INSERT INTO rule_output(n) VALUES(OLD.n)'),
    ('update-new-constant', 'UPDATE rule_event SET m=7', 'INSERT INTO rule_output(n) VALUES(NEW.m)'),
    ('update-constant', "UPDATE rule_event SET m=nextval('rule_ticks')", 'INSERT INTO rule_output(n) VALUES(7)'),
    ('update-false', "UPDATE rule_event SET m=nextval('rule_ticks') WHERE false", 'INSERT INTO rule_output(n) VALUES(7)'),
    ('update-target-predicate', "UPDATE rule_event SET m=nextval('rule_ticks') WHERE n>0", 'INSERT INTO rule_output(n) VALUES(7)'),
    ('update-independent-predicate', "UPDATE rule_event SET m=nextval('rule_ticks') WHERE nextval('rule_ticks')>0", 'INSERT INTO rule_output(n) VALUES(7)'),
    ('update-from-constant', "UPDATE rule_event SET m=nextval('rule_ticks') FROM generate_series(1,2) g(k)", 'INSERT INTO rule_output(n) VALUES(7)'),
    ('update-from-new', "UPDATE rule_event SET m=k+nextval('rule_ticks') FROM generate_series(1,2) g(k)", 'INSERT INTO rule_output(n,m) VALUES(NEW.m,NEW.m)'),
    ('update-from-old', "UPDATE rule_event SET m=k+nextval('rule_ticks') FROM generate_series(1,2) g(k)", 'INSERT INTO rule_output(n,m) VALUES(OLD.n,OLD.m)'),
    ('update-scalar-new', "UPDATE rule_event SET m=(SELECT n+nextval('rule_ticks'))", 'INSERT INTO rule_output(n,m) VALUES(NEW.m,NEW.m)'),
    ('update-assignment-default', 'UPDATE rule_event SET m=DEFAULT', 'INSERT INTO rule_output(n,m) VALUES(NEW.m,NEW.m)'),
    ('update-whole-row', "UPDATE rule_event SET m=nextval('rule_ticks')", 'INSERT INTO rule_output(payload) VALUES(row_to_json(NEW)::text)'),
    ('update-also', "UPDATE rule_event SET m=nextval('rule_ticks')", 'INSERT INTO rule_output(n,m) VALUES(OLD.n,NEW.m)'),
    ('delete-repeated-old', "DELETE FROM rule_event WHERE nextval('rule_ticks')>0", 'INSERT INTO rule_output(n,m) VALUES(OLD.n,OLD.n)'),
    ('delete-constant', "DELETE FROM rule_event WHERE nextval('rule_ticks')>0", 'INSERT INTO rule_output(n) VALUES(7)'),
    ('delete-target-predicate', "DELETE FROM rule_event WHERE n>0 AND nextval('rule_ticks')>0", 'INSERT INTO rule_output(n) VALUES(7)'),
    ('delete-from-constant', 'DELETE FROM rule_event USING generate_series(1,2) g(k)', 'INSERT INTO rule_output(n) VALUES(7)'),
    ('delete-from-old', 'DELETE FROM rule_event USING generate_series(1,2) g(k)', 'INSERT INTO rule_output(n,m) VALUES(OLD.n,OLD.m)'),
    ('delete-also', "DELETE FROM rule_event WHERE nextval('rule_ticks')>0", 'INSERT INTO rule_output(n) VALUES(OLD.n)'),
]:
    event_kind = 'UPDATE' if event.startswith('update') else 'DELETE'
    schedule = 'ALSO' if event.endswith('also') else 'INSTEAD'
    setup = base + f'; CREATE RULE route AS ON {event_kind} TO rule_event DO {schedule} ' + action
    cases.append((event,setup,original))

for name, original in [
    ('update-joined-qualification', "UPDATE rule_event SET m=1 FROM generate_series(1,2) g(k) WHERE nextval('rule_ticks')>0"),
    ('delete-joined-qualification', "DELETE FROM rule_event USING generate_series(1,2) g(k) WHERE nextval('rule_ticks')>0"),
]:
    cases.append((name,base,original))

for name, additions, original, action, schedule in [
    ('update-new-unchanged', '', "UPDATE rule_event SET m=nextval('rule_ticks')", 'INSERT INTO rule_output(n,m) VALUES(NEW.n,OLD.m)', 'INSTEAD'),
    ('update-scalar-local-shadow', '', "UPDATE rule_event SET m=(SELECT n+nextval('rule_ticks') FROM (VALUES(50)) local(n))", 'INSERT INTO rule_output(n,m) VALUES(NEW.m,NEW.m)', 'INSTEAD'),
    ('update-scalar-qualified-shadow', '', "UPDATE rule_event AS event SET m=(SELECT event.n+nextval('rule_ticks') FROM (VALUES(50)) event(n))", 'INSERT INTO rule_output(n,m) VALUES(NEW.m,NEW.m)', 'INSTEAD'),
    ('update-grouped-assignment', '', "UPDATE rule_event SET (n,m)=(SELECT nextval('rule_ticks'),nextval('rule_ticks'))", 'INSERT INTO rule_output(n,m) VALUES(NEW.m,NEW.m)', 'INSTEAD'),
    ('update-array-assignment', 'ALTER TABLE rule_event ADD COLUMN a bigint[] DEFAULT ARRAY[10,20]::bigint[]', "UPDATE rule_event SET a[1]=nextval('rule_ticks')", 'INSERT INTO rule_output(payload) VALUES(NEW.a::text || NEW.a::text)', 'INSTEAD'),
    ('update-array-multiple-assignment', 'ALTER TABLE rule_event ADD COLUMN a bigint[] DEFAULT ARRAY[10,20]::bigint[]', "UPDATE rule_event SET a[1]=nextval('rule_ticks'),a[2]=nextval('rule_ticks')", 'INSERT INTO rule_output(payload) VALUES(NEW.a::text || NEW.a::text)', 'INSTEAD'),
    ('update-conditional-new', '', "UPDATE rule_event SET m=nextval('rule_ticks')", 'INSERT INTO rule_output(n,m) VALUES(NEW.m,NEW.m)', 'INSTEAD'),
    ('delete-independent-actions', '', "DELETE FROM rule_event WHERE nextval('rule_ticks')>0", '(INSERT INTO rule_output(n) VALUES(OLD.n); INSERT INTO rule_output(n) VALUES(OLD.n))', 'INSTEAD'),
    ('delete-constant-empty', 'DELETE FROM rule_event', "DELETE FROM rule_event WHERE nextval('rule_ticks')>0", 'INSERT INTO rule_output(n) VALUES(7)', 'INSTEAD'),
    ('update-constant-empty', 'DELETE FROM rule_event', "UPDATE rule_event SET m=nextval('rule_ticks')", 'INSERT INTO rule_output(n) VALUES(7)', 'INSTEAD'),
]:
    event = 'UPDATE' if name.startswith('update') else 'DELETE'
    condition = ' WHERE NEW.m > 1' if name == 'update-conditional-new' else ''
    setup = base + '; ' + additions + f'; CREATE RULE route AS ON {event} TO rule_event{condition} DO {schedule} ' + action
    cases.append((name, setup, original))

for name, event, statement, expression, source_grant in [
    ('update-authority-event-old', 'UPDATE', 'UPDATE rule_event SET m=7', 'OLD.n', None),
    ('update-authority-original-read', 'UPDATE', 'UPDATE rule_event SET m=n+7', 'NEW.m', None),
    ('delete-authority-event-old', 'DELETE', 'DELETE FROM rule_event', 'OLD.n', None),
    ('delete-authority-original-read', 'DELETE', 'DELETE FROM rule_event WHERE n>0', 'OLD.n', None),
    ('update-authority-from-caller', 'UPDATE', 'UPDATE rule_event SET m=source.n FROM rule_source source', 'NEW.m', 'rule_reader'),
    ('update-authority-from-owner', 'UPDATE', 'UPDATE rule_event SET m=source.n FROM rule_source source', 'NEW.m', 'rule_owner'),
    ('update-authority-subquery-caller', 'UPDATE', 'UPDATE rule_event SET m=(SELECT max(n) FROM rule_source)', 'NEW.m', 'rule_reader'),
    ('update-authority-subquery-owner', 'UPDATE', 'UPDATE rule_event SET m=(SELECT max(n) FROM rule_source)', 'NEW.m', 'rule_owner'),
]:
    setup = base + '; CREATE ROLE rule_owner; CREATE ROLE rule_reader; ALTER TABLE rule_event OWNER TO rule_owner; GRANT UPDATE(m),DELETE ON rule_event TO rule_reader; GRANT INSERT ON rule_output TO rule_owner; GRANT USAGE ON SEQUENCE rule_output_ordinal_seq TO rule_reader'
    if source_grant:
        setup += '; CREATE TEMP TABLE rule_source(n bigint); INSERT INTO rule_source VALUES(5),(6); GRANT SELECT ON rule_source TO ' + source_grant
    setup += f'; CREATE RULE route AS ON {event} TO rule_event DO INSTEAD INSERT INTO rule_output(n) VALUES({expression}); SET ROLE rule_reader'
    cases.append((name, setup, statement))

for name, event, original, expression, condition in [
    ('update-collation-new', 'UPDATE', "UPDATE rule_event SET c='x' COLLATE \"POSIX\"", 'pg_collation_for(NEW.c)', ''),
    ('update-collation-old', 'UPDATE', "UPDATE rule_event SET c='x' COLLATE \"POSIX\"", 'pg_collation_for(OLD.c)', ''),
    ('update-collation-subquery', 'UPDATE', "UPDATE rule_event SET c=(SELECT 'x' COLLATE \"POSIX\")", '(SELECT pg_collation_for(NEW.c))', ''),
    ('update-collation-condition', 'UPDATE', "UPDATE rule_event SET c='x' COLLATE \"POSIX\"", 'NEW.c', "WHERE pg_collation_for(NEW.c)='\"C\"'"),
    ('update-collation-null', 'UPDATE', 'UPDATE rule_event SET c=NULL', 'pg_collation_for(NEW.c)', ''),
    ('delete-collation-old', 'DELETE', 'DELETE FROM rule_event', 'pg_collation_for(OLD.c)', ''),
]:
    setup = base + '; ALTER TABLE rule_event ADD COLUMN c text COLLATE "C"; UPDATE rule_event SET c=\'kept\'; '
    setup += f'CREATE RULE route AS ON {event} TO rule_event {condition} DO INSTEAD INSERT INTO rule_output(payload) VALUES({expression})'
    cases.append((name,setup,original))

for name, statement in [
    ('update-target-row-field', 'UPDATE rule_event SET m=(rule_event).n'),
    ('update-target-row-alias', 'UPDATE rule_event e SET m=(e).n'),
    ('update-target-row-subquery', 'UPDATE rule_event SET m=(SELECT (rule_event).n)'),
    ('update-target-whole-row', "UPDATE rule_event SET m=(row_to_json(rule_event)->>'n')::bigint"),
    ('update-source-whole-row', "UPDATE rule_event SET m=(row_to_json(s)->>'k')::bigint FROM (VALUES(5,7)) s(k,v)"),
]:
    cases.append((name,base+'; CREATE RULE route AS ON UPDATE TO rule_event DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.m)',statement))
cases.append(('delete-target-whole-row-predicate',base+'; CREATE RULE route AS ON DELETE TO rule_event DO INSTEAD INSERT INTO rule_output(n) VALUES(7)',"DELETE FROM rule_event WHERE (row_to_json(rule_event)->>'n')::bigint>0"))

for name, statement in [
    ('update-system-unqualified', "UPDATE rule_event SET m=7 FROM (VALUES(1),(2)) s(k) WHERE tableoid='rule_event'::regclass"),
    ('update-system-qualified', "UPDATE rule_event e SET m=CASE WHEN e.tableoid='rule_event'::regclass THEN 7 ELSE 8 END"),
    ('update-system-subquery', "UPDATE rule_event e SET m=(SELECT CASE WHEN e.tableoid='rule_event'::regclass THEN 7 ELSE 8 END)"),
    ('update-source-system', "UPDATE rule_event e SET m=CASE WHEN s.tableoid='rule_source'::regclass THEN 7 ELSE 8 END FROM rule_source s"),
    ('delete-system-unqualified', "DELETE FROM rule_event USING (VALUES(1),(2)) s(k) WHERE tableoid='rule_event'::regclass"),
    ('delete-system-qualified', "DELETE FROM rule_event e WHERE e.tableoid='rule_event'::regclass"),
    ('delete-source-system', "DELETE FROM rule_event e USING rule_source s WHERE s.tableoid='rule_source'::regclass"),
]:
    event = 'UPDATE' if name.startswith('update') else 'DELETE'
    expression = 'NEW.m' if event == 'UPDATE' else '7'
    setup = base + '; CREATE TEMP TABLE rule_source(k int); INSERT INTO rule_source VALUES(1),(2); '
    setup += f'CREATE RULE route AS ON {event} TO rule_event DO INSTEAD INSERT INTO rule_output(n) VALUES({expression})'
    cases.append((name, setup, statement))

for name, inner_options, outer_filter, outer_source in [
    ('update-view-lazy-nested', '', '', 'rule_inner'),
    ('update-view-lazy-filtered', '', ' WHERE m>0', 'rule_inner'),
    ('update-view-lazy-aliased', '', '', 'rule_inner AS renamed(a,b,c)'),
    ('update-view-lazy-barrier', ' WITH(security_barrier=true)', '', 'rule_inner'),
]:
    aliased = name.endswith('aliased')
    outputs = 'a AS n,b AS m,c AS boom' if aliased else 'n,m,boom'
    setup = base + f'; CREATE TEMP VIEW rule_inner{inner_options} AS SELECT n,m,10/(n-2) AS boom FROM rule_event; CREATE TEMP VIEW rule_view AS SELECT {outputs} FROM {outer_source}{outer_filter}; '
    setup += 'CREATE RULE route AS ON UPDATE TO rule_view WHERE CASE WHEN NEW.n>0 THEN true ELSE NEW.boom>0 END DO ALSO INSERT INTO rule_output(n) VALUES(OLD.n)'
    cases.append((name,setup,'UPDATE rule_view SET m=7'))

for suffix, grant in [('denied',''), ('column', 'GRANT SELECT(n) ON rule_inner TO rule_outer_owner; '), ('allowed', 'GRANT SELECT ON rule_inner TO rule_outer_owner; ')]:
    setup = base + '; CREATE ROLE rule_inner_owner; CREATE ROLE rule_outer_owner; CREATE ROLE rule_reader; '
    setup += 'GRANT SELECT ON rule_event TO rule_inner_owner; CREATE TEMP VIEW rule_inner AS SELECT n,m FROM rule_event; ALTER VIEW rule_inner OWNER TO rule_inner_owner; '
    setup += 'CREATE TEMP VIEW rule_view AS SELECT n,m FROM rule_inner; ALTER VIEW rule_view OWNER TO rule_outer_owner; '
    setup += grant + 'GRANT UPDATE(m) ON rule_view TO rule_reader; GRANT INSERT ON rule_output TO rule_outer_owner; GRANT USAGE ON SEQUENCE rule_output_ordinal_seq TO rule_reader; '
    setup += 'CREATE RULE route AS ON UPDATE TO rule_view DO INSTEAD INSERT INTO rule_output(n) VALUES(OLD.n); SET ROLE rule_reader'
    cases.append(('update-view-authority-'+suffix,setup,'UPDATE rule_view SET m=7'))

for name, expression in [
    ('update-view-tableoid', "CASE WHEN physical_oid='rule_event'::regclass THEN 7 ELSE 8 END"),
    ('update-view-xmin', "CASE WHEN transaction_id<>'0'::xid THEN 7 ELSE 8 END"),
]:
    setup = base + '; CREATE TEMP VIEW rule_view AS SELECT n,m,tableoid AS physical_oid,xmin AS transaction_id FROM rule_event; '
    setup += f'CREATE RULE route AS ON UPDATE TO rule_view DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.m)'
    cases.append((name,setup,'UPDATE rule_view SET m='+expression))

# One target fixes candidate order independently of the database's join strategy.
candidate_base = base + '; DELETE FROM rule_event WHERE n>1'
cases.append(('update-joined-assignment-every-candidate', candidate_base, "UPDATE rule_event SET m=nextval('rule_ticks') FROM generate_series(1,2) g(k)"))
cases.append(('update-joined-later-assignment-error', candidate_base, "UPDATE rule_event SET m=1/(k-2) FROM generate_series(1,2) g(k)"))
for event, returned, mutation in [
    ('update', 'NEW', 'UPDATE rule_event SET m=k FROM generate_series(1,2) g(k)'),
    ('delete', 'OLD', 'DELETE FROM rule_event USING generate_series(1,2) g(k)'),
]:
    for skip, condition in [('first', "nextval('rule_ticks')=1"), ('all', "nextval('rule_ticks')>0")]:
        setup = candidate_base + f"; CREATE FUNCTION pg_temp.skip_candidate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF {condition} THEN RETURN NULL; END IF; RETURN {returned}; END $$; "
        setup += f'CREATE TRIGGER skip_candidate BEFORE {event.upper()} ON rule_event FOR EACH ROW EXECUTE FUNCTION pg_temp.skip_candidate()'
        cases.append((f'{event}-joined-trigger-skips-{skip}', setup, mutation))
setup = candidate_base + "; CREATE FUNCTION pg_temp.skip_candidate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.m=1 THEN RETURN NULL; END IF; RETURN NEW; END $$; "
setup += 'CREATE TRIGGER skip_candidate BEFORE UPDATE ON rule_event FOR EACH ROW EXECUTE FUNCTION pg_temp.skip_candidate()'
cases.append(('update-joined-assignment-before-skipped-trigger', setup, "UPDATE rule_event SET m=nextval('rule_ticks') FROM generate_series(1,2) g(k)"))

recoveries = set()
query = "RESET ROLE; SELECT json_build_object('rows', (SELECT json_agg(row_to_json(r)) FROM (SELECT n,m,payload FROM rule_output ORDER BY ordinal) r), 'event', (SELECT json_agg(row_to_json(r)) FROM (SELECT n,m FROM rule_event ORDER BY n,m) r), 'next', nextval('rule_ticks'))"
output = []
for name, setup, sql in cases:
    script = '\\set ON_ERROR_STOP on\n\\set VERBOSITY verbose\nBEGIN;\n' + setup + ';\n\\echo UQA_RULE_SETUP_READY\n\\o /dev/null\n' + sql + ';\n\\o\n' + query + ';\nROLLBACK;\n'
    if name in recoveries:
        script = '\\set ON_ERROR_STOP on\n\\set VERBOSITY verbose\nBEGIN;\n' + setup + ';\n\\echo UQA_RULE_SETUP_READY\nSAVEPOINT before_rule;\n\\set ON_ERROR_STOP off\n' + sql + ';\nROLLBACK TO before_rule;\n\\set ON_ERROR_STOP on\n' + query + ';\nROLLBACK;\n'
    result = subprocess.run(command, input=script, text=True, capture_output=True)
    if 'UQA_RULE_SETUP_READY\n' not in result.stdout:
        raise RuntimeError(f'{name}: setup failed: {result.stderr}')
    result.stdout = result.stdout.replace('UQA_RULE_SETUP_READY\n', '', 1)
    case = {'name':name, 'setup':setup, 'sql':sql, 'query':query}
    if result.returncode or name in recoveries:
        error = re.search(r'ERROR:\s+([A-Z0-9]{5}): (.+)',result.stderr)
        if not error:
            raise RuntimeError(result.stderr)
        case.update(sqlstate=error[1], message=error[2])
        if name in recoveries:
            case.update(recover=True, result=json.loads(result.stdout))
    else:
        case['result'] = json.loads(result.stdout)
    output.append(case)
path=Path(__file__).with_name('rules_update_delete_inputs.expected.json')
path.write_text(json.dumps({'version': version, 'cases': output}, ensure_ascii=False, indent=2)+'\n')
for case in output:
    print(case['name'],case.get('result',case.get('message')))
