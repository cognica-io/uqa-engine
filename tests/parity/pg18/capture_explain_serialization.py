#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture EXPLAIN serialization visibility and actual output volume from PostgreSQL 18."""

import json
import os
import re
import subprocess
from pathlib import Path


QUERIES = [
    "SELECT FROM generate_series(1, 1025)",
    "SELECT n, n::bigint, n::smallint, true, '한글'::text FROM generate_series(1, 257) AS g(n)",
    "SELECT n::numeric / 10, 1.25::real, -0.5::double precision, '2000-01-01'::date, '02:03:04+02'::timetz FROM generate_series(1, 257) AS g(n)",
    "SELECT ARRAY[n,NULL,n+1], ROW(n,'한글'::text), '[1.25,9.5)'::numrange FROM generate_series(1, 257) AS g(n)",
    "SELECT NULL::int, NULL::text, ''::text, 'x'::char(4), '10101010101'::bit(11) FROM generate_series(1, 257)",
    "SELECT repeat('x', n) AS same, repeat('y', n+1) AS same FROM generate_series(1, 100) AS g(n) ORDER BY n DESC",
    "SELECT '한글'::text AS value WHERE false",
    "VALUES (1,'a'), (2,'b')",
    "SELECT 3::int AS value UNION ALL SELECT 4::int",
    "SELECT n::bigint FROM generate_series(1,257) AS g(n)",
    "SELECT ROW(n,'한글')::serialization_record, 'ready'::serialization_state, ARRAY[1,NULL,3]::serialization_array_domain FROM generate_series(1,257) AS g(n)",
    "SELECT s.* FROM serialization_values s, generate_series(1,257)",
    "SELECT '42'::regclass, '42'::regtype, '42'::regproc FROM generate_series(1,257)",
    "INSERT INTO serialization_items SELECT generate_series(1, 257) RETURNING n, n::bigint",
    "UPDATE serialization_items SET n = 1",
    "CREATE TEMP TABLE serialization_destination AS SELECT generate_series(1, 257) AS n",
]
SETUP = "CREATE TEMP TABLE serialization_items (n integer); CREATE TYPE serialization_record AS (id integer, label text); CREATE TYPE serialization_state AS ENUM ('ready','done'); CREATE DOMAIN serialization_array_domain AS integer[]; CREATE TEMP TABLE serialization_values (r serialization_record, e serialization_state, a serialization_array_domain, bounds int4range); INSERT INTO serialization_values VALUES (ROW(7,'한글'), 'ready', ARRAY[1,NULL,3], '[1,4)')"


def main():
    command = json.loads(os.environ["PG_EXPLAIN_COMMAND"])

    def sql(statement):
        return subprocess.check_output(command + ["-XAtq", "-v", "ON_ERROR_STOP=1", "-c", statement], text=True).strip()

    version = sql("SELECT version()")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(version)
    cases = []
    for format in ["TEXT", "BINARY"]:
        for query in QUERIES:
            statement = f"EXPLAIN (ANALYZE, SERIALIZE {format}, FORMAT JSON, TIMING OFF, BUFFERS OFF, SUMMARY OFF) {query}"
            observed = json.loads(sql(f"BEGIN; {SETUP}; {statement}; ROLLBACK"))[0]["Serialization"]
            cases.append({"setup": SETUP, "sql": statement, "serialization": observed})
    for options in ["SERIALIZE", "SERIALIZE NONE", "SERIALIZE TEXT, TIMING FALSE", "SERIALIZE BINARY, TIMING FALSE"]:
        statement = f"EXPLAIN (ANALYZE, {options}, FORMAT JSON, BUFFERS OFF, SUMMARY OFF) SELECT 1"
        observed = json.loads(sql(statement))[0].get("Serialization")
        if observed is not None:
            observed["timing"] = "Time" in observed
            observed.pop("Time", None)
        cases.append({"setup": "", "sql": statement, "serialization": observed})
    for query in ["SELECT NULL::serialization_no_send", "SELECT ARRAY[]::aclitem[]", "SELECT ROW(NULL::serialization_no_send)", "SELECT NULL::aclitem[]", "SELECT NULL::serialization_no_send WHERE false"]:
        setup = "CREATE DOMAIN serialization_no_send AS aclitem"
        statement = f"EXPLAIN (ANALYZE, SERIALIZE BINARY, FORMAT JSON, TIMING OFF, BUFFERS OFF) {query}"
        result = subprocess.run(command + ["-XAtq", "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose", "-c", f"BEGIN; {setup}; {statement}; ROLLBACK"], capture_output=True, text=True)
        if result.returncode:
            error = re.search(r"ERROR:\s+([A-Z0-9]{5}): (.+)", result.stderr)
            if error is None:
                raise RuntimeError(result.stderr)
            cases.append({"setup": setup, "sql": statement, "sqlstate": error[1], "message": error[2]})
        else:
            cases.append({"setup": setup, "sql": statement, "serialization": json.loads(result.stdout)[0]["Serialization"]})
    mutations = [
        "INSERT INTO serialization_target SELECT nextval('serialization_rows') FROM generate_series(1,8) RETURNING n, NULL::aclitem",
        "INSERT INTO serialization_target VALUES (nextval('serialization_rows')), (nextval('serialization_rows')) RETURNING n, NULL::aclitem",
        "UPDATE serialization_target SET n=nextval('serialization_rows') RETURNING n, NULL::aclitem",
        "DELETE FROM serialization_target RETURNING nextval('serialization_rows'), NULL::aclitem",
        "UPDATE serialization_target SET n=n WHERE nextval('serialization_rows')>0 RETURNING NULL::aclitem",
        "DELETE FROM serialization_target WHERE nextval('serialization_rows')>0 RETURNING NULL::aclitem",
        "MERGE INTO serialization_target t USING generate_series(1,8) g(n) ON t.n=g.n WHEN MATCHED AND nextval('serialization_rows')>0 THEN UPDATE SET n=t.n RETURNING NULL::aclitem",
        "MERGE INTO serialization_target t USING generate_series(1,8) g(n) ON t.n=g.n WHEN MATCHED THEN UPDATE SET n=t.n RETURNING nextval('serialization_rows'), NULL::aclitem",
        "INSERT INTO serialization_event SELECT nextval('serialization_rows') FROM generate_series(1,8) RETURNING n, NULL::aclitem",
        "INSERT INTO serialization_event SELECT n FROM generate_series(1,8) g(n) RETURNING n, nextval('serialization_rows'), NULL::aclitem",
        "INSERT INTO serialization_event VALUES(nextval('serialization_rows')),(nextval('serialization_rows')) RETURNING n, NULL::aclitem",
        "INSERT INTO serialization_partial(n[nextval('serialization_rows')::integer]) SELECT 7 FROM generate_series(1,8) RETURNING n, NULL::aclitem",
        "INSERT INTO serialization_nested(n[nextval('serialization_rows')::integer].a) SELECT 7 FROM generate_series(1,8) RETURNING n, NULL::aclitem",
    ]
    mutation_setup = "CREATE TEMP SEQUENCE serialization_rows; CREATE TEMP SEQUENCE serialization_after; CREATE TEMP TABLE serialization_target(n bigint); INSERT INTO serialization_target SELECT n FROM generate_series(1,8) g(n); CREATE FUNCTION pg_temp.serialization_after() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('serialization_after'); RETURN NULL; END $$; CREATE TRIGGER serialization_after AFTER INSERT OR UPDATE OR DELETE ON serialization_target FOR EACH STATEMENT EXECUTE FUNCTION pg_temp.serialization_after()"
    failures = []
    for mutation in mutations:
        setup = mutation_setup
        if "serialization_event" in mutation:
            setup += "; CREATE TEMP VIEW serialization_event AS SELECT n FROM serialization_target; CREATE RULE serialization_route AS ON INSERT TO serialization_event DO INSTEAD INSERT INTO serialization_target VALUES (NEW.n) RETURNING n"
        if "serialization_partial" in mutation:
            setup += "; CREATE TEMP TABLE serialization_partial(n bigint[]); CREATE RULE serialization_route AS ON INSERT TO serialization_partial DO INSTEAD INSERT INTO serialization_target VALUES(NEW.n[1]) RETURNING ARRAY[n]"
        if "serialization_nested" in mutation:
            setup += "; CREATE TYPE serialization_pair AS (a bigint,b bigint); CREATE TEMP TABLE serialization_nested(n serialization_pair[]); CREATE RULE serialization_route AS ON INSERT TO serialization_nested DO INSTEAD INSERT INTO serialization_target VALUES((NEW.n[1]).a) RETURNING ARRAY[ROW(n,NULL)::serialization_pair]"
        statement = "EXPLAIN (ANALYZE, SERIALIZE BINARY) " + mutation
        script = "BEGIN; CREATE SCHEMA serialization_capture; SET LOCAL search_path=serialization_capture,pg_temp,public; " + setup + "; SAVEPOINT before_output;\n\\set ON_ERROR_STOP off\n" + statement + ";\n\\set ON_ERROR_STOP on\nROLLBACK TO before_output; SELECT json_build_object('rows', currval('serialization_rows'), 'after', nextval('serialization_after')); ROLLBACK;"
        result = subprocess.run(command + ["-XAtq", "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose"], input=script, capture_output=True, text=True, check=True)
        errors = re.findall(r"ERROR:\s+([A-Z0-9]{5}): (.+)", result.stderr)
        if len(errors) != 1:
            raise RuntimeError(result.stderr)
        state, message = errors[0]
        failures.append({"setup": setup, "sql": statement, "sqlstate": state, "message": message, "effects": json.loads(result.stdout)})
    order_setup = "CREATE TEMP SEQUENCE delivery_qualification; CREATE TEMP SEQUENCE delivery_before; CREATE TEMP SEQUENCE delivery_output; CREATE TEMP TABLE delivery_rows(n bigint); INSERT INTO delivery_rows SELECT n FROM generate_series(1,8) g(n); CREATE FUNCTION pg_temp.delivery_before() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('delivery_before'); RETURN OLD; END $$; CREATE TRIGGER delivery_before BEFORE DELETE ON delivery_rows FOR EACH ROW EXECUTE FUNCTION pg_temp.delivery_before()"
    order_statement = "DELETE FROM delivery_rows WHERE nextval('delivery_qualification')>0 RETURNING n, currval('delivery_qualification') AS qualified, currval('delivery_before') AS before_row, nextval('delivery_output') AS output_row"
    order_rows = json.loads(sql(f"BEGIN; {order_setup}; WITH deleted AS ({order_statement}) SELECT json_agg(d) FROM deleted d; ROLLBACK"))
    order = {"setup": order_setup, "sql": order_statement, "rows": order_rows}
    path = Path(__file__).with_name("explain_serialization.expected.json")
    path.write_text(json.dumps({"version": version, "cases": cases, "mutation_failures": failures, "mutation_order": order}, ensure_ascii=False, indent=2) + "\n")
    print(f"Captured {len(cases)} serialization cases from {version}")


if __name__ == "__main__":
    main()
