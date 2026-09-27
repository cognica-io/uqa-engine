#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
"""Capture index declaration diagnostics from an existing PostgreSQL 18 container."""

import argparse
import json
from pathlib import Path
import subprocess


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    source = Path(__file__).with_name("index_definition_order_oracle.sql")
    result = subprocess.run(
        ["docker", "exec", "-i", args.container, "psql", "-X", "-qAt", "-U", "postgres", "-d", "postgres"],
        input=source.read_text(), text=True, capture_output=True, check=True,
    )
    reference = json.loads(result.stdout)
    if not reference["postgresql_version"].startswith("18."):
        raise ValueError("the reference must use PostgreSQL 18")
    reference["docker_image"] = subprocess.run(
        ["docker", "inspect", "--format", "{{.Image}}", args.container],
        text=True, capture_output=True, check=True,
    ).stdout.strip()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    header = {key: value for key, value in reference.items() if key != "cases"}
    cases = ",\n".join("    " + json.dumps(case, ensure_ascii=False) for case in reference["cases"])
    encoded = json.dumps(header, ensure_ascii=False, indent=2)[:-2] + ',\n  "cases": [\n' + cases + "\n  ]\n}\n"
    args.output.write_text(encoded)
    print(f"Captured {len(reference['cases'])} index declaration cases")


if __name__ == "__main__":
    main()
