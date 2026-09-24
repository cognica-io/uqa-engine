#!/usr/bin/env python3
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

"""Capture GROUP BY name precedence and errors from a disposable PostgreSQL 18 container."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[3]
FIXTURE = ROOT / "crates/uqa-sql/src/semantics/grouping_sets/pg18_names.json"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--output", type=pathlib.Path, default=FIXTURE)
    args = parser.parse_args()
    command = [
        "docker", "exec", args.container, "psql", "-X", "-q", "-U", "postgres",
        "-A", "-t", "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose",
    ]

    def query(sql: str) -> subprocess.CompletedProcess:
        return subprocess.run(command + ["-c", sql], capture_output=True, text=True, timeout=30)

    version = query("SELECT version();")
    version.check_returncode()
    if not version.stdout.startswith("PostgreSQL 18."):
        raise RuntimeError("the oracle must be PostgreSQL 18")
    image = subprocess.check_output(
        ["docker", "inspect", "--format", "{{.Image}}", args.container], text=True
    ).strip()
    source = json.loads(FIXTURE.read_text())
    setup = ";".join(source["setup"]) + ";"
    cases = []
    for original in source["cases"]:
        sql = original["sql"]
        result = query(f"BEGIN; {setup} SELECT coalesce(json_agg(row_to_json(t)), '[]'::json) FROM ({sql}) t; ROLLBACK;")
        case = {"sql": sql}
        if result.returncode:
            error = re.search(r"ERROR:\s+([0-9A-Z]{5}):", result.stderr)
            if not error:
                raise RuntimeError(result.stderr or result.stdout)
            case["sqlstate"] = error.group(1)
        else:
            case["rows"] = [list(row.values()) for row in json.loads(result.stdout)]
        cases.append(case)
    lines = ["{", f'  "postgresql": {json.dumps(version.stdout.strip())},', f'  "image": {json.dumps(image)},']
    for key, values in [("setup", source["setup"]), ("cases", cases)]:
        lines.append(f'  "{key}": [')
        lines.extend("    " + json.dumps(value) + ("," if i + 1 < len(values) else "") for i, value in enumerate(values))
        lines.append("  ]" + ("," if key == "setup" else ""))
    lines.append("}")
    args.output.write_text("\n".join(lines) + "\n")
    print(f"Captured {len(cases)} grouping-name expectations from {version.stdout.split(' (')[0]}.")


if __name__ == "__main__":
    main()
