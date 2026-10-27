#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture PostgreSQL 18 catalog identities used by native type I/O."""

import json
import os
import subprocess
from pathlib import Path


TYPES = "bool int2 int4 int8 float4 float8 numeric text varchar bpchar name bytea json jsonb xml date time timetz timestamp timestamptz interval bit varbit uuid oid regclass regtype regproc regprocedure regnamespace refcursor aclitem oidvector int2vector record cstring anyarray anyenum anyrange anymultirange".split()


def main():
    command = json.loads(os.environ["PG_EXPLAIN_COMMAND"])

    def sql(statement):
        return subprocess.check_output(command + ["-XAtq", "-v", "ON_ERROR_STOP=1", "-c", statement], text=True).strip()

    version = sql("SELECT version()")
    if not version.startswith("PostgreSQL 18."):
        raise RuntimeError(version)
    names = ",".join("'" + name + "'" for name in TYPES)
    query = "SELECT json_agg(row_to_json(p) ORDER BY p.oid) FROM (SELECT DISTINCT p.oid::int8, p.proname, p.prokind::text, p.proisstrict, p.provolatile::text, p.proparallel::text, p.proleakproof, p.prorettype::int8, string_to_array(p.proargtypes::text,' ')::int8[] AS args, p.prosrc FROM pg_type t CROSS JOIN LATERAL unnest(ARRAY[t.typinput,t.typoutput,t.typreceive,t.typsend,t.typmodin,t.typmodout]) o(oid) JOIN pg_proc p ON p.oid=o.oid WHERE t.typname IN (" + names + ")) p"
    rows = json.loads(sql(query))
    path = Path(__file__).resolve().parents[3] / "crates/uqa-execution/src/catalog/projection/builtin_routines/tests/type_io.json"
    path.write_text(json.dumps({"version": version, "routines": rows}, indent=2) + "\n")
    print(f"Captured {len(rows)} native type I/O identities from {version}")


if __name__ == "__main__":
    main()
