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

for name, projection in [
    ('unused-scalar-error', '7,(SELECT 1/0)'),
    ('unused-nested-scalar-error', '7,(SELECT (SELECT 1/0))'),
    ('unused-values-scalar-error', '7,(VALUES ((SELECT 1/0)))'),
    ('used-volatile-unused-scalar-error', "(SELECT nextval('rule_ticks')),(SELECT 1/0)"),
    ('unused-scalar-volatile', "7,(SELECT nextval('rule_ticks'))"),
    ('unused-values-scalar-volatile', "7,(VALUES ((SELECT nextval('rule_ticks'))))"),
]:
    cases.append((name, base + '; ' + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)', 'INSERT INTO rule_event SELECT ' + projection))

for name, projection in [
    ('unused-cte-shadowing-view', '(WITH rule_shadow AS (VALUES(1/0)) SELECT * FROM rule_shadow)'),
    ('unused-nested-cte-shadowing-view', '(WITH rule_shadow AS (VALUES(1/0)) SELECT (SELECT * FROM rule_shadow))'),
]:
    setup = base + "; CREATE TEMP VIEW rule_shadow AS SELECT nextval('rule_ticks'); " + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)'
    cases.append((name, setup, 'INSERT INTO rule_event SELECT 7,' + projection))

for name, sql in [
    ('unused-outer-cte-shadowing-view', 'WITH rule_shadow AS (VALUES(1/0)) INSERT INTO rule_event SELECT 7,(SELECT * FROM rule_shadow)'),
    ('unused-query-cte-shadowing-view', 'INSERT INTO rule_event WITH rule_shadow AS (VALUES(1/0)) SELECT 7,(SELECT * FROM rule_shadow)'),
    ('unused-outer-nested-cte-shadowing-view', 'WITH rule_shadow AS (VALUES(1/0)) INSERT INTO rule_event SELECT 7,(SELECT (SELECT * FROM rule_shadow))'),
    ('unused-derived-cte-shadowing-view', 'WITH rule_shadow AS (VALUES(1/0)) INSERT INTO rule_event SELECT * FROM (SELECT 7,(SELECT * FROM rule_shadow)) input'),
    ('unused-outer-volatile-cte', "WITH rule_shadow AS MATERIALIZED (SELECT nextval('rule_ticks')) INSERT INTO rule_event SELECT 7,(SELECT * FROM rule_shadow)"),
    ('unused-inner-volatile-cte', "INSERT INTO rule_event SELECT 7,(WITH rule_shadow AS MATERIALIZED (SELECT nextval('rule_ticks')) SELECT * FROM rule_shadow)"),
    ('unused-cte-catalog-view-scope', 'WITH rule_shadow AS (VALUES(1/0)) INSERT INTO rule_event SELECT 7,(SELECT * FROM rule_visible)'),
    ('unused-recursive-forward-cte', 'WITH RECURSIVE before_shadow AS (SELECT * FROM rule_shadow),rule_shadow AS (VALUES(1/0)) INSERT INTO rule_event SELECT 7,(SELECT * FROM before_shadow)'),
    ('unused-quoted-cte-shadowing-view', 'WITH "Rule Shadow" AS (VALUES(1/0)) INSERT INTO rule_event SELECT 7,(SELECT * FROM "Rule Shadow")'),
]:
    setup = base + "; CREATE TEMP VIEW rule_shadow AS SELECT nextval('rule_ticks'); CREATE TEMP VIEW rule_visible AS SELECT * FROM rule_shadow; CREATE TEMP VIEW \"Rule Shadow\" AS SELECT nextval('rule_ticks'); " + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)'
    cases.append((name, setup, sql))

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

values_base = "CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint, m bigint DEFAULT nextval('rule_ticks')); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text)"
values_route = 'CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.n)'
default_route = 'CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n,m) VALUES(NEW.m,NEW.m)'
cases.extend([
    ('values-single', values_base + '; ' + values_route, "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
    ('values-multiple', values_base + '; ' + values_route, "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL),(nextval('rule_ticks'),NULL)"),
    ('values-single-unused-effect', values_base + '; ' + values_route, "INSERT INTO rule_event VALUES(1,nextval('rule_ticks'))"),
    ('values-multiple-unused-effect', values_base + '; ' + values_route, "INSERT INTO rule_event VALUES(1,nextval('rule_ticks')),(2,nextval('rule_ticks'))"),
    ('values-single-unused-error', values_base + '; ' + values_route, 'INSERT INTO rule_event VALUES(1,1/0)'),
    ('values-multiple-unused-error', values_base + '; ' + values_route, 'INSERT INTO rule_event VALUES(1,1/0),(2,1/0)'),
    ('values-single-default', values_base + '; ' + default_route, 'INSERT INTO rule_event VALUES(1,DEFAULT)'),
    ('values-multiple-default', values_base + '; ' + default_route, 'INSERT INTO rule_event VALUES(1,DEFAULT),(2,DEFAULT)'),
    ('values-nested', values_base + '; CREATE TEMP TABLE rule_hop(n bigint); CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_hop VALUES(NEW.n); CREATE RULE hop AS ON INSERT TO rule_hop DO INSTEAD INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.n)', "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
    ('values-nested-condition', values_base + '; CREATE TEMP TABLE rule_hop(n bigint); CREATE RULE suppress AS ON INSERT TO rule_event DO INSTEAD NOTHING; CREATE RULE route AS ON INSERT TO rule_event WHERE NEW.n>0 DO ALSO INSERT INTO rule_hop VALUES(NEW.n); CREATE RULE hop AS ON INSERT TO rule_hop DO INSTEAD INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.n)', "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
    ('values-prepared', values_base + '; ' + values_route, "PREPARE p(bigint) AS INSERT INTO rule_event VALUES(nextval('rule_ticks')+$1,NULL); EXECUTE p(10)"),
])
view_values = values_base + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; CREATE RULE outer_log AS ON INSERT TO rule_view DO ALSO INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.m); CREATE RULE base_log AS ON INSERT TO rule_event DO ALSO INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.m)'
cases.extend([
    ('values-view-omitted-default', view_values, 'INSERT INTO rule_view(n) VALUES(1)'),
    ('values-view-explicit-default', view_values, 'INSERT INTO rule_view VALUES(1,DEFAULT)'),
    ('values-view-multiple-defaults', view_values, 'INSERT INTO rule_view VALUES(1,DEFAULT),(2,DEFAULT)'),
])
partial_values = "CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n bigint[], m bigint); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n,m) VALUES(array_lower(NEW.n,1),array_lower(NEW.n,1))"
cases.extend([
    ('values-partial-single', partial_values, "INSERT INTO rule_event(n[nextval('rule_ticks')::integer]) VALUES(7)"),
    ('values-partial-multiple', partial_values, "INSERT INTO rule_event(n[nextval('rule_ticks')::integer]) VALUES(7),(8)"),
])
domain_values = "CREATE TEMP SEQUENCE rule_ticks; CREATE FUNCTION pg_temp.rule_domain_check(bigint) RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('rule_ticks'); RETURN true; END $$; CREATE DOMAIN rule_domain AS bigint CHECK(pg_temp.rule_domain_check(VALUE)); CREATE TEMP TABLE rule_event(n rule_domain,m bigint); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); " + values_route
cases.extend([
    ('values-domain-single', domain_values, 'INSERT INTO rule_event VALUES(1,NULL)'),
    ('values-domain-multiple', domain_values, 'INSERT INTO rule_event VALUES(1,NULL),(2,NULL)'),
])
cases.extend([
    ('values-discarded-input', values_base + '; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD NOTHING', 'INSERT INTO rule_event VALUES(1,1/0),(2,1/0)'),
    ('query-discarded-explicit-columns', values_base + '; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD NOTHING', 'INSERT INTO rule_event(n,m) SELECT 1,1/0'),
    ('values-action-default', values_base + "; ALTER TABLE rule_output ALTER n SET DEFAULT nextval('rule_ticks'); CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n,m) VALUES(DEFAULT,7)", "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
    ('values-action-multiple-defaults', values_base + "; ALTER TABLE rule_output ALTER n SET DEFAULT nextval('rule_ticks'); CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n,m) VALUES(DEFAULT,7),(DEFAULT,8)", "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
    ('values-explicit-default-values', values_base + '; ' + default_route, 'INSERT INTO rule_event(m) DEFAULT VALUES'),
])
collated_values = 'CREATE TEMP SEQUENCE rule_ticks; CREATE TEMP TABLE rule_event(n text COLLATE "C",m bigint); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(payload) VALUES(pg_collation_for(NEW.n))'
cases.extend([
    ('values-column-collation', collated_values, "INSERT INTO rule_event VALUES('x',NULL)"),
    ('values-explicit-collation', collated_values, "INSERT INTO rule_event VALUES('x' COLLATE \"POSIX\",NULL)"),
    ('query-explicit-collation', collated_values, "INSERT INTO rule_event SELECT 'x' COLLATE \"POSIX\",NULL::bigint"),
])

action_view = values_base + "; ALTER TABLE rule_output ALTER n SET DEFAULT nextval('rule_ticks'); CREATE TEMP VIEW rule_hop AS SELECT n,m FROM rule_output; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD "
action_layers = values_base + "; CREATE TEMP TABLE rule_hop(n bigint DEFAULT nextval('rule_ticks')); CREATE TEMP VIEW rule_hop_view AS SELECT n FROM rule_hop; CREATE RULE view_route AS ON INSERT TO rule_hop_view DO ALSO INSERT INTO rule_output(n,m) VALUES(NEW.n,1); CREATE RULE base_route AS ON INSERT TO rule_hop DO INSTEAD INSERT INTO rule_output(n,m) VALUES(NEW.n,2); CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD "
cases.extend([
    ('values-action-default-view', action_view + 'INSERT INTO rule_hop(n,m) VALUES(DEFAULT,7)', "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
    ('values-action-mixed-default-view', action_view + 'INSERT INTO rule_hop(n,m) VALUES(DEFAULT,7),(99,8)', "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
    ('values-action-default-view-rules', action_layers + 'INSERT INTO rule_hop_view VALUES(DEFAULT)', "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
    ('values-action-mixed-default-view-rules', action_layers + 'INSERT INTO rule_hop_view VALUES(DEFAULT),(99)', "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
    ('values-action-default-rule-chain', values_base + "; CREATE TEMP TABLE rule_hop(n bigint DEFAULT nextval('rule_ticks')); CREATE RULE hop_route AS ON INSERT TO rule_hop DO INSTEAD INSERT INTO rule_output(n,m) VALUES(NEW.n,NEW.n); CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_hop VALUES(DEFAULT)", "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL)"),
])

identity_values = "CREATE TEMP TABLE rule_event(n bigint GENERATED BY DEFAULT AS IDENTITY (SEQUENCE NAME rule_ticks),m bigint); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); " + values_route
domain_defaults = "CREATE TEMP SEQUENCE rule_ticks; CREATE DOMAIN rule_default AS bigint DEFAULT nextval('rule_ticks'); CREATE TEMP TABLE rule_event(n rule_default,m bigint); CREATE TEMP TABLE rule_output(ordinal bigserial,n bigint,m bigint,payload text); " + values_route
cases.extend([
    ('identity-default-single', identity_values, 'INSERT INTO rule_event VALUES(DEFAULT,NULL)'),
    ('identity-default-multiple', identity_values, 'INSERT INTO rule_event VALUES(DEFAULT,NULL),(DEFAULT,NULL)'),
    ('identity-omitted', identity_values, 'INSERT INTO rule_event(m) VALUES(NULL)'),
    ('identity-override-single', identity_values, 'INSERT INTO rule_event OVERRIDING USER VALUE VALUES(99,NULL)'),
    ('identity-override-multiple', identity_values, 'INSERT INTO rule_event OVERRIDING USER VALUE VALUES(99,NULL),(99,NULL)'),
    ('domain-default-single', domain_defaults, 'INSERT INTO rule_event VALUES(DEFAULT,NULL)'),
    ('domain-default-multiple', domain_defaults, 'INSERT INTO rule_event VALUES(DEFAULT,NULL),(DEFAULT,NULL)'),
    ('domain-default-omitted-query', domain_defaults, 'INSERT INTO rule_event(m) SELECT NULL::bigint FROM generate_series(1,2)'),
    ('action-identity-always-default', values_base + '; ALTER TABLE rule_output ALTER n SET NOT NULL; ALTER TABLE rule_output ALTER n ADD GENERATED ALWAYS AS IDENTITY; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n,m) VALUES(DEFAULT,7)', 'INSERT INTO rule_event VALUES(9,NULL)'),
    ('action-identity-always-mixed', values_base + '; ALTER TABLE rule_output ALTER n SET NOT NULL; ALTER TABLE rule_output ALTER n ADD GENERATED ALWAYS AS IDENTITY; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n,m) VALUES(DEFAULT,7),(9,8)', 'INSERT INTO rule_event VALUES(9,NULL)'),
])

cases.extend([
    ('identity-mixed-default', identity_values, 'INSERT INTO rule_event VALUES(DEFAULT,NULL),(99,NULL)'),
    ('identity-override-default-single', identity_values, 'INSERT INTO rule_event OVERRIDING USER VALUE VALUES(DEFAULT,NULL)'),
    ('identity-override-default-multiple', identity_values, 'INSERT INTO rule_event OVERRIDING USER VALUE VALUES(DEFAULT,NULL),(DEFAULT,NULL)'),
    ('identity-override-query', identity_values, "INSERT INTO rule_event OVERRIDING USER VALUE SELECT nextval('rule_ticks'),NULL::bigint FROM generate_series(1,2)"),
    ('identity-override-values-effect', identity_values, "INSERT INTO rule_event OVERRIDING USER VALUE VALUES(nextval('rule_ticks'),NULL),(nextval('rule_ticks'),NULL)"),
    ('identity-also-single', identity_values.replace('DO INSTEAD', 'DO ALSO'), 'INSERT INTO rule_event VALUES(DEFAULT,NULL)'),
    ('identity-also-multiple', identity_values.replace('DO INSTEAD', 'DO ALSO'), 'INSERT INTO rule_event VALUES(DEFAULT,NULL),(DEFAULT,NULL)'),
    ('identity-also-override-default', identity_values.replace('DO INSTEAD', 'DO ALSO'), 'INSERT INTO rule_event OVERRIDING USER VALUE VALUES(DEFAULT,NULL),(DEFAULT,NULL)'),
    ('identity-always-view-default', identity_values.replace('BY DEFAULT','ALWAYS').replace('DO INSTEAD','DO ALSO') + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; CREATE RULE view_log AS ON INSERT TO rule_view DO ALSO INSERT INTO rule_output(n,m) VALUES(NEW.n,9)', 'INSERT INTO rule_view VALUES(DEFAULT,NULL)'),
    ('identity-always-view-written', identity_values.replace('BY DEFAULT','ALWAYS').replace('DO INSTEAD','DO ALSO') + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; CREATE RULE view_log AS ON INSERT TO rule_view DO ALSO INSERT INTO rule_output(n,m) VALUES(NEW.n,9)', 'INSERT INTO rule_view VALUES(9,NULL)'),
    ('action-generated-default', values_base + '; ALTER TABLE rule_output DROP n; ALTER TABLE rule_output ADD n bigint GENERATED ALWAYS AS (m+1) STORED; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n,m) VALUES(DEFAULT,7)', 'INSERT INTO rule_event VALUES(9,NULL)'),
    ('action-generated-mixed', values_base + '; ALTER TABLE rule_output DROP n; ALTER TABLE rule_output ADD n bigint GENERATED ALWAYS AS (m+1) STORED; CREATE RULE route AS ON INSERT TO rule_event DO INSTEAD INSERT INTO rule_output(n,m) VALUES(DEFAULT,7),(9,8)', 'INSERT INTO rule_event VALUES(9,NULL)'),
])

cases.extend([
    ('identity-view-override-default', identity_values.replace('DO INSTEAD','DO ALSO') + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; CREATE RULE view_log AS ON INSERT TO rule_view DO ALSO INSERT INTO rule_output(n,m) VALUES(NEW.n,9)', 'INSERT INTO rule_view OVERRIDING USER VALUE VALUES(DEFAULT,NULL),(DEFAULT,NULL)'),
    ('identity-view-override-written', identity_values.replace('DO INSTEAD','DO ALSO') + '; CREATE TEMP VIEW rule_view AS SELECT * FROM rule_event; CREATE RULE view_log AS ON INSERT TO rule_view DO ALSO INSERT INTO rule_output(n,m) VALUES(NEW.n,9)', 'INSERT INTO rule_view OVERRIDING USER VALUE VALUES(99,NULL),(99,NULL)'),
])

set_rule = values_base + '; CREATE RULE route AS ON INSERT TO rule_event DO ALSO INSERT INTO rule_output(n) SELECT 1000::bigint UNION ALL SELECT 1001::bigint'
cases.extend([
    ('set-action-single', set_rule, 'INSERT INTO rule_event VALUES(1,NULL)'),
    ('set-action-multiple', set_rule, 'INSERT INTO rule_event VALUES(1,NULL),(2,NULL)'),
    ('set-action-query', set_rule, 'INSERT INTO rule_event SELECT 1,NULL::bigint'),
    ('set-action-empty-query', set_rule, 'INSERT INTO rule_event SELECT 1,NULL::bigint WHERE false'),
])

cases.extend([
    ('set-action-error-before-input', set_rule, "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL),(2,NULL)"),
    ('set-action-error-before-trigger', set_rule + "; CREATE FUNCTION pg_temp.rule_before() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('rule_ticks'); RETURN NULL; END $$; CREATE TRIGGER rule_before BEFORE INSERT ON rule_event FOR EACH STATEMENT EXECUTE FUNCTION pg_temp.rule_before()", 'INSERT INTO rule_event VALUES(1,NULL),(2,NULL)'),
    ('set-action-nested-error-before-input', values_base + '; CREATE TEMP TABLE rule_hop(n bigint); CREATE RULE route AS ON INSERT TO rule_event DO ALSO INSERT INTO rule_hop VALUES(NEW.n); CREATE RULE nested_route AS ON INSERT TO rule_hop DO ALSO INSERT INTO rule_output(n) SELECT 1000::bigint UNION ALL SELECT 1001::bigint', "INSERT INTO rule_event VALUES(nextval('rule_ticks'),NULL),(2,NULL)"),
])
recoveries.update({'set-action-error-before-input', 'set-action-error-before-trigger', 'set-action-nested-error-before-input'})

cases.append(('recursive-conflict-before-input', values_base + '; CREATE RULE route AS ON INSERT TO rule_event DO ALSO INSERT INTO rule_event(n) VALUES(NEW.n+1)', "INSERT INTO rule_event(n) VALUES(nextval('rule_ticks')) ON CONFLICT DO NOTHING"))
recoveries.add('recursive-conflict-before-input')
cases.append(('set-action-default-values', set_rule, 'INSERT INTO rule_event DEFAULT VALUES'))

# The command planner and executor share CTE reachability for ordinary mutations and rule inputs.
for name, sql in [
    ('ordinary-insert-unused-cte', 'WITH unused AS (VALUES(1/0)) INSERT INTO rule_output(n) VALUES(7)'),
    ('ordinary-update-unused-cte', 'WITH unused AS (VALUES(1/0)) UPDATE rule_output SET n=8'),
    ('ordinary-delete-unused-cte', 'WITH unused AS (VALUES(1/0)) DELETE FROM rule_output'),
    ('ordinary-merge-unused-cte', 'WITH unused AS (VALUES(1/0)) MERGE INTO rule_output target USING (VALUES(8)) source(n) ON true WHEN MATCHED THEN UPDATE SET n=source.n'),
    ('ordinary-unused-volatile-cte', "WITH unused AS MATERIALIZED (SELECT nextval('rule_ticks')) INSERT INTO rule_output(n) VALUES(7)"),
    ('ordinary-cte-write-dependency', 'WITH input AS (VALUES(8)),written AS (INSERT INTO rule_event(n,unused) SELECT column1,0 FROM input) INSERT INTO rule_output(n) VALUES(7)'),
    ('ordinary-cte-unused-chain', 'WITH input AS (VALUES(1/0)),unused AS (SELECT * FROM input) INSERT INTO rule_output(n) VALUES(7)'),
    ('ordinary-cte-returning-reference', "WITH input AS (SELECT nextval('rule_ticks') AS n) INSERT INTO rule_output(n) VALUES(7) RETURNING (SELECT n FROM input)"),
    ('ordinary-cte-referenced-error', 'WITH input AS (VALUES(1/0)) INSERT INTO rule_output(n) SELECT * FROM input'),
]:
    cases.append((name, base + '; INSERT INTO rule_output(n) VALUES(1)', sql))

for name, sql in [
    ('unused-derived-scalar-error', 'INSERT INTO rule_event SELECT * FROM (SELECT 7,(SELECT 1/0)) input'),
    ('unused-derived-scalar-volatile', "INSERT INTO rule_event SELECT * FROM (SELECT 7,(SELECT nextval('rule_ticks'))) input"),
]:
    cases.append((name, base + '; ' + route + 'INSERT INTO rule_output(n) VALUES(NEW.n)', sql))

query = "RESET ROLE; SELECT json_build_object('rows', (SELECT json_agg(row_to_json(r)) FROM (SELECT n,m,payload FROM rule_output ORDER BY ordinal) r), 'event', (SELECT json_agg(n ORDER BY n) FROM rule_event), 'next', nextval('rule_ticks'))"
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
path=Path(__file__).with_name('rules_query_inputs.expected.json')
path.write_text(json.dumps({'version': version, 'cases': output}, ensure_ascii=False, indent=2)+'\n')
for case in output:
    print(case['name'],case.get('result',case.get('message')))
