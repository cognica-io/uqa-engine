#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture rewrite-rule query input evaluation from an independent PostgreSQL 18."""

import json
import os
import re
import subprocess
from pathlib import Path

command = json.loads(os.environ['PG_EXPLAIN_COMMAND']) + ['-XAtq']
version = subprocess.check_output(command + ['-c', 'SELECT version()'], text=True).strip()
if not version.startswith('PostgreSQL 18.'):
    raise RuntimeError(version)
base = "CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint, unused bigint DEFAULT nextval('rule_ticks')); CREATE TEMP TABLE rule_output(ordinal bigserial, n bigint, m bigint, payload text)"
route = "CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD "
cases = [
    ('action-values', base + '; ' + route + 'INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.n) RETURNING n,m', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3) RETURNING n,unused"),
    ('action-multiple-values', base + '; ' + route + 'INSERT INTO rule_output(n,m) VALUES(NEW.n,1),(NEW.n,2) RETURNING n,m', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3) RETURNING n,unused"),
    ('action-select', base + '; ' + route + 'INSERT INTO rule_output(n) SELECT NEW.n WHERE NEW.n % 2 = 1', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3)"),
    ('independent-actions', base + '; ' + route + '(INSERT INTO rule_output(n,m) VALUES(NEW.n,1); INSERT INTO rule_output(n,m) VALUES(NEW.n,2))', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3)"),
    ('unused-volatile-output', base + '; ' + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)', "INSERT INTO rule_event SELECT n,nextval('rule_ticks') FROM generate_series(1,3) g(n)"),
    ('unused-error-output', base + '; ' + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)', "INSERT INTO rule_event SELECT n,1/(n-2) FROM generate_series(1,3) g(n)"),
    ('condition', base + '; ' + route + 'NOTHING; CREATE RULE selected AS ON INSERT TO rule_event WHERE NEW.n <= 2 DO ALSO INSERT INTO rule_output(n) VALUES(NEW.n)', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3)"),
    ('whole-row', base + '; ' + route + 'INSERT INTO rule_output(payload) VALUES(row_to_json(NEW)::text)', "INSERT INTO rule_event SELECT nextval('rule_ticks'), 9::bigint FROM generate_series(1,3)"),
    ('used-default', base + '; ' + route + 'INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.unused)', "INSERT INTO rule_event(n) SELECT n FROM generate_series(1,3) g(n)"),
    ('unused-default', base + '; ' + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)', "INSERT INTO rule_event(n) SELECT n FROM generate_series(1,3) g(n)"),
    ('query-cte', base + '; ' + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)', "WITH input AS (SELECT nextval('rule_ticks') AS n FROM generate_series(1,3)) INSERT INTO rule_event(n) SELECT n FROM input"),
    ('query-cte-multiple-actions', base + '; ' + route + '(INSERT INTO rule_output(n,m) VALUES(NEW.n,1); INSERT INTO rule_output(n,m) VALUES(NEW.n,2))', "WITH input AS (SELECT nextval('rule_ticks') AS n FROM generate_series(1,3)) INSERT INTO rule_event(n) SELECT n FROM input"),
    ('nested-rules', base + '; CREATE TEMP TABLE rule_hop(n bigint); ' + route + "INSERT INTO rule_hop VALUES(NEW.n); CREATE RULE hop AS ON INSERT TO rule_hop DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.n)", "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3)"),
]
cases.extend([
    ('empty-query', base + '; ' + route + 'INSERT INTO rule_output(n) VALUES(7)', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint WHERE false"),
    ('nothing', base + '; ' + route + 'NOTHING', "INSERT INTO rule_event SELECT nextval('rule_ticks'), 1/0 FROM generate_series(1,3)"),
    ('prepared-input', base + '; ' + route + 'INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.n)', "PREPARE p(bigint) AS INSERT INTO rule_event SELECT nextval('rule_ticks')+$1, NULL::bigint FROM generate_series(1,3); EXECUTE p(10)"),
    ('cte-returning', base + '; ' + route + 'INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.n) RETURNING n,m', "WITH input AS (SELECT nextval('rule_ticks') AS n FROM generate_series(1,3)) INSERT INTO rule_event(n) SELECT n FROM input RETURNING (SELECT sum(n) FROM input)"),
    ('update-action', base + '; INSERT INTO rule_output(n) VALUES(1),(2),(3); ' + route + 'UPDATE rule_output SET m=NEW.n WHERE n=NEW.n', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3)"),
    ('delete-action', base + '; INSERT INTO rule_output(n) VALUES(1),(2),(3); ' + route + 'DELETE FROM rule_output WHERE n=NEW.n', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,2)"),
    ('automatic-view', base + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; ' + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)', "INSERT INTO rule_view SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3)"),
    ('also-rule', base + '; CREATE RULE route AS ON INSERT TO rule_event DO ALSO INSERT INTO rule_output(n) VALUES(NEW.n)', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3)"),
    ('conditional-instead', base + '; CREATE RULE route AS ON INSERT TO rule_event WHERE NEW.n%2=1 DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.n)', "INSERT INTO rule_event SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3)"),
    ('view-rule-layers', base + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; CREATE RULE view_route AS ON INSERT TO rule_view DO ALSO INSERT INTO rule_output(n,m) VALUES(NEW.n,1); ' + route + 'INSERT INTO rule_output(n,m) VALUES(NEW.n,2)', "INSERT INTO rule_view SELECT nextval('rule_ticks'), NULL::bigint FROM generate_series(1,3)"),
    ('partial-assignment', base + '; CREATE TEMP TABLE rule_array(n bigint[]); CREATE RULE route AS ON INSERT TO rule_array DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.n[1])', "INSERT INTO rule_array(n[1]) SELECT nextval('rule_ticks') FROM generate_series(1,3)"),
])
cases.extend([
    ('partial-array-volatile-bounds', base + '; CREATE TEMP TABLE rule_array(n bigint[]); CREATE RULE route AS ON INSERT TO rule_array DO INSTEAD INSERT INTO rule_output(n,m) VALUES(array_lower(NEW.n,1),array_lower(NEW.n,1))', "INSERT INTO rule_array(n[nextval('rule_ticks')::integer]) SELECT 7 FROM generate_series(1,3)"),
    ('partial-composite-fields', base + '; CREATE TYPE rule_record AS (a bigint, b bigint[]); CREATE TEMP TABLE rule_record_input(n rule_record); CREATE RULE route AS ON INSERT TO rule_record_input DO INSTEAD INSERT INTO rule_output(n,m) VALUES((NEW.n).a,(NEW.n).b[1])', "INSERT INTO rule_record_input(n.a,n.b[1]) SELECT nextval('rule_ticks'),nextval('rule_ticks') FROM generate_series(1,3)"),
    ('partial-nested-volatile-bounds', base + '; CREATE TYPE rule_pair AS (a bigint, b bigint); CREATE TEMP TABLE rule_nested(n rule_pair[]); CREATE RULE route AS ON INSERT TO rule_nested DO INSTEAD INSERT INTO rule_output(n,m) VALUES(array_lower(NEW.n,1),array_upper(NEW.n,1))', "INSERT INTO rule_nested(n[(nextval('rule_ticks')%2+1)::integer].a,n[(nextval('rule_ticks')%2+1)::integer].b) SELECT 7,8 FROM generate_series(1,3)"),
])

cases.extend([
    ('implicit-used-default', base + '; ' + route + 'INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.unused)', "INSERT INTO rule_event SELECT n FROM generate_series(1,3) g(n)"),
    ('implicit-unused-default', base + '; ' + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)', "INSERT INTO rule_event SELECT n FROM generate_series(1,3) g(n)"),
    ('implicit-view-default', base + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; ' + route + 'INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.unused)', "INSERT INTO rule_view SELECT n FROM generate_series(1,3) g(n)"),
    ('null-slice-bound-rule', base + '; CREATE TEMP TABLE rule_array(n bigint[]); CREATE RULE route AS ON INSERT TO rule_array DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.n[1])', "INSERT INTO rule_array(n[NULL:nextval('rule_ticks')::integer]) SELECT ARRAY[7::bigint]"),
    ('null-slice-bound-ordinary', base + '; CREATE TEMP TABLE rule_array(n bigint[])', "INSERT INTO rule_array(n[NULL:nextval('rule_ticks')::integer]) VALUES(ARRAY[nextval('rule_ticks')])"),
])
cases.extend([
    ('cte-automatic-view-multiple-actions', base + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; ' + route + '(INSERT INTO rule_output(n,m) VALUES(NEW.n,1); INSERT INTO rule_output(n,m) VALUES(NEW.n,2))', "WITH input AS (SELECT nextval('rule_ticks') AS n FROM generate_series(1,3)) INSERT INTO rule_view(n) SELECT n FROM input"),
    ('cte-view-original-suppressed', base + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; CREATE RULE view_route AS ON INSERT TO rule_view DO ALSO INSERT INTO rule_output(n) VALUES(NEW.n); ' + route + 'NOTHING', "WITH input AS (SELECT nextval('rule_ticks') AS n FROM generate_series(1,3)) INSERT INTO rule_view(n) SELECT n FROM input"),
    ('cte-also-rule', base + '; CREATE RULE route AS ON INSERT TO rule_event DO ALSO INSERT INTO rule_output(n) VALUES(NEW.n)', "WITH input AS (SELECT nextval('rule_ticks') AS n FROM generate_series(1,3)) INSERT INTO rule_event(n) SELECT n FROM input"),
])

recoveries = {'null-slice-bound-rule', 'null-slice-bound-ordinary'}

cases.extend([
    ('table-query-constant-action', base + '; CREATE TEMP TABLE rule_source(n bigint); INSERT INTO rule_source VALUES(1),(2); ' + route + 'INSERT INTO rule_output(n) VALUES(7)', "INSERT INTO rule_event(n) SELECT n FROM rule_source ORDER BY n"),
    ('table-query-default-action', base + '; CREATE TEMP TABLE rule_source(n bigint); INSERT INTO rule_source VALUES(1),(2); ' + route + 'INSERT INTO rule_output DEFAULT VALUES', "INSERT INTO rule_event(n) SELECT n FROM rule_source ORDER BY n"),
    ('empty-table-default-action', base + '; CREATE TEMP TABLE rule_source(n bigint); ' + route + 'INSERT INTO rule_output DEFAULT VALUES', "INSERT INTO rule_event(n) SELECT n FROM rule_source"),
])

cases.extend([
    ('caller-only', 'CREATE ROLE rule_owner; CREATE ROLE rule_reader; CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint); CREATE TEMP TABLE rule_source(n bigint); INSERT INTO rule_source VALUES(1),(2); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); GRANT SELECT ON rule_source TO rule_reader; ALTER TABLE rule_event OWNER TO rule_owner; GRANT INSERT ON rule_event TO rule_reader; GRANT INSERT ON rule_output TO rule_owner; GRANT USAGE ON SEQUENCE rule_output_ordinal_seq TO rule_reader; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.n); SET ROLE rule_reader', 'INSERT INTO rule_event SELECT n FROM rule_source'),
    ('owner-only', 'CREATE ROLE rule_owner; CREATE ROLE rule_reader; CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint); CREATE TEMP TABLE rule_source(n bigint); INSERT INTO rule_source VALUES(1),(2); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); GRANT SELECT ON rule_source TO rule_owner; ALTER TABLE rule_event OWNER TO rule_owner; GRANT INSERT ON rule_event TO rule_reader; GRANT INSERT ON rule_output TO rule_owner; GRANT USAGE ON SEQUENCE rule_output_ordinal_seq TO rule_reader; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.n); SET ROLE rule_reader', 'INSERT INTO rule_event SELECT n FROM rule_source'),
])

cases.extend([
    ('bound-caller-only', 'CREATE ROLE rule_owner; CREATE ROLE rule_reader; CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint[]); CREATE TEMP TABLE rule_source(n bigint); INSERT INTO rule_source VALUES(1); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); GRANT SELECT ON rule_source TO rule_reader; ALTER TABLE rule_event OWNER TO rule_owner; GRANT INSERT ON rule_event TO rule_reader; GRANT INSERT ON rule_output TO rule_owner; GRANT USAGE ON SEQUENCE rule_output_ordinal_seq TO rule_reader; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n) VALUES(array_lower(NEW.n,1)); SET ROLE rule_reader', 'INSERT INTO rule_event(n[(SELECT n::integer FROM rule_source LIMIT 1)]) SELECT 7'),
    ('condition-caller-only', 'CREATE ROLE rule_owner; CREATE ROLE rule_reader; CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint); CREATE TEMP TABLE rule_source(n bigint); INSERT INTO rule_source VALUES(1); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); GRANT SELECT ON rule_source TO rule_reader; ALTER TABLE rule_event OWNER TO rule_owner; GRANT INSERT ON rule_event TO rule_reader; GRANT INSERT ON rule_output TO rule_owner; GRANT USAGE ON SEQUENCE rule_output_ordinal_seq TO rule_reader; CREATE RULE route AS ON INSERT TO rule_event WHERE (SELECT n FROM rule_source LIMIT 1)=1 DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.n); SET ROLE rule_reader', 'INSERT INTO rule_event SELECT 7'),
    ('bound-owner-only', 'CREATE ROLE rule_owner; CREATE ROLE rule_reader; CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint[]); CREATE TEMP TABLE rule_source(n bigint); INSERT INTO rule_source VALUES(1); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); GRANT SELECT ON rule_source TO rule_owner; ALTER TABLE rule_event OWNER TO rule_owner; GRANT INSERT ON rule_event TO rule_reader; GRANT INSERT ON rule_output TO rule_owner; GRANT USAGE ON SEQUENCE rule_output_ordinal_seq TO rule_reader; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n) VALUES(array_lower(NEW.n,1)); SET ROLE rule_reader', 'INSERT INTO rule_event(n[(SELECT n::integer FROM rule_source LIMIT 1)]) SELECT 7'),
    ('condition-owner-only', 'CREATE ROLE rule_owner; CREATE ROLE rule_reader; CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint); CREATE TEMP TABLE rule_source(n bigint); INSERT INTO rule_source VALUES(1); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); GRANT SELECT ON rule_source TO rule_owner; ALTER TABLE rule_event OWNER TO rule_owner; GRANT INSERT ON rule_event TO rule_reader; GRANT INSERT ON rule_output TO rule_owner; GRANT USAGE ON SEQUENCE rule_output_ordinal_seq TO rule_reader; CREATE RULE route AS ON INSERT TO rule_event WHERE (SELECT n FROM rule_source LIMIT 1)=1 DO INSTEAD INSERT INTO rule_output(n) VALUES(NEW.n); SET ROLE rule_reader', 'INSERT INTO rule_event SELECT 7'),
])

query = "RESET ROLE; SELECT json_build_object('rows', (SELECT json_agg(row_to_json(r)) FROM (SELECT n,m,payload FROM rule_output ORDER BY ordinal) r), 'event', (SELECT json_agg(n ORDER BY n) FROM rule_event), 'next', nextval('rule_ticks'))"
output = []
for name, setup, sql in cases:
    script = '\\set ON_ERROR_STOP on\n\\set VERBOSITY verbose\nBEGIN;\n' + setup + ';\n\\o /dev/null\n' + sql + ';\n\\o\n' + query + ';\nROLLBACK;\n'
    if name in recoveries:
        script = '\\set ON_ERROR_STOP on\n\\set VERBOSITY verbose\nBEGIN;\n' + setup + ';\nSAVEPOINT before_rule;\n\\set ON_ERROR_STOP off\n' + sql + ';\nROLLBACK TO before_rule;\n\\set ON_ERROR_STOP on\n' + query + ';\nROLLBACK;\n'
    result = subprocess.run(command, input=script, text=True, capture_output=True)
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
path=Path(__file__).with_name('rules_query_inputs.expected.json')
path.write_text(json.dumps({'version': version, 'cases': output}, ensure_ascii=False, indent=2)+'\n')
for case in output:
    print(case['name'],case.get('result',case.get('message')))
