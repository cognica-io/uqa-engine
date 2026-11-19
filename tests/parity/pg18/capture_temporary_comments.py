#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Capture type-description lifetime and rollback from an isolated PostgreSQL 18 database."""

import argparse
import json
import subprocess
import uuid
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    args = parser.parse_args()
    directory = Path(__file__).parent
    cases = json.loads((directory / "temporary_comments.input.json").read_text())["cases"]
    database = "uqa_comment_" + uuid.uuid4().hex
    command = ["docker", "exec", "-i", args.container]
    subprocess.run(command + ["createdb", "-U", "postgres", database], check=True)
    try:
        psql = command + ["psql", "-U", "postgres", "-d", database, "-XAtq", "-v", "ON_ERROR_STOP=1"]
        version = subprocess.check_output(psql + ["-c", "SELECT version()"], text=True).strip()
        if not version.startswith("PostgreSQL 18."):
            raise RuntimeError(version)
        for reopened in [False, True]:
            group = [case for case in cases if bool(case.get("reopen")) == reopened]
            statements = []
            for case in group:
                if case.get("observe"):
                    statements.append("SELECT COALESCE(json_agg(description ORDER BY description),'[]'::json) FROM (" + case["sql"] + ") AS observations")
                else:
                    statements.append(case["sql"])
            output = subprocess.check_output(psql, input=";\n".join(statements) + ";\n", text=True)
            observations = iter(json.loads(line) for line in output.splitlines())
            for case in group:
                if case.get("observe"):
                    case["descriptions"] = next(observations)
            if next(observations, None) is not None:
                raise RuntimeError("unexpected oracle output")
        destination = directory / "temporary_comments.expected.json"
        destination.write_text(json.dumps({"version": version, "cases": cases}, ensure_ascii=False, indent=2) + "\n")
        print("Captured", len(cases), "description lifecycle steps from", version)
    finally:
        subprocess.run(command + ["dropdb", "-U", "postgres", database], check=True)


if __name__ == "__main__":
    main()
