#!/usr/bin/env python3
"""Run the checked-in TPC-H query set against UQA and PostgreSQL 18."""

from __future__ import annotations

import argparse
import csv
import io
import json
import os
import pathlib
import re
import statistics
import subprocess
import sys
from decimal import Decimal, InvalidOperation
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "benchmarks" / "tpch"
DEFAULT_REPORT = ROOT / "target" / "benchmark-runs" / "tpch-pg18.json"
DEFAULT_EXPECTED = FIXTURE / "expected" / "pg18.json"
TABLES = (
    "region",
    "nation",
    "supplier",
    "customer",
    "part",
    "partsupp",
    "orders",
    "lineitem",
)
NUMERIC = re.compile(r"^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$")


def command(
    args: list[str],
    *,
    input_bytes: bytes | None = None,
    cwd: pathlib.Path = ROOT,
) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        args,
        cwd=cwd,
        input=input_bytes,
        capture_output=True,
        check=True,
    )


def psql_command(container: str, database: str, *args: str) -> list[str]:
    return [
        "docker",
        "exec",
        "-i",
        container,
        "psql",
        "-X",
        "-U",
        "postgres",
        "-d",
        database,
        "-v",
        "ON_ERROR_STOP=1",
        *args,
    ]


def psql(
    container: str,
    database: str,
    *args: str,
    input_bytes: bytes | None = None,
) -> bytes:
    try:
        return command(
            psql_command(container, database, *args), input_bytes=input_bytes
        ).stdout
    except subprocess.CalledProcessError as error:
        stderr = error.stderr.decode(errors="replace").strip()
        raise RuntimeError(f"psql failed: {stderr}") from error


def postgres_version(container: str, database: str) -> dict[str, str]:
    output = psql(
        container,
        database,
        "-tA",
        "-c",
        "SHOW server_version_num; SHOW server_version; SELECT version();",
    ).decode()
    lines = [line for line in output.splitlines() if line]
    if len(lines) != 3 or not lines[0].startswith("18"):
        raise RuntimeError(
            f"container {container!r} is not PostgreSQL 18: {output.strip()}"
        )
    return {
        "server_version_num": lines[0],
        "server_version": lines[1],
        "version": lines[2],
    }


def reset_postgres(container: str, database: str) -> None:
    psql(
        container,
        database,
        "-c",
        "DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;",
    )
    psql(
        container,
        database,
        "-f",
        "-",
        input_bytes=(FIXTURE / "schema.sql").read_bytes(),
    )
    for table in TABLES:
        source = FIXTURE / "data" / f"{table}.tbl"
        rows = []
        for line_number, line in enumerate(source.read_bytes().splitlines(), 1):
            if not line.endswith(b"|"):
                raise RuntimeError(f"{source}:{line_number} does not end in '|'")
            rows.append(line[:-1])
        payload = b"\n".join(rows) + b"\n"
        psql(
            container,
            database,
            "-c",
            f"COPY {table} FROM STDIN WITH (FORMAT csv, DELIMITER '|', QUOTE E'\\x01')",
            input_bytes=payload,
        )
    psql(container, database, "-c", "ANALYZE")


def queries() -> list[str]:
    return [
        (FIXTURE / "queries" / f"q{number:02}.sql").read_text().strip()
        for number in range(1, 23)
    ]


def without_semicolon(query: str) -> str:
    return query.rstrip().removesuffix(";").rstrip()


def canonical_numeric(cell: str) -> str:
    if not NUMERIC.fullmatch(cell):
        return cell
    try:
        value = Decimal(cell)
    except InvalidOperation:
        return cell
    if not value.is_finite():
        return cell
    if value == 0:
        return "0"
    return format(value.normalize(), "f")


def postgres_result_description(
    container: str, database: str, query: str
) -> list[tuple[str, str]]:
    output = psql(
        container,
        database,
        "--csv",
        "-P",
        "footer=off",
        input_bytes=(without_semicolon(query) + "\n\\gdesc\n").encode(),
    ).decode()
    records = list(csv.reader(io.StringIO(output)))
    if not records or records[0] != ["Column", "Type"]:
        raise RuntimeError(f"unexpected PostgreSQL result description: {records!r}")
    return [(record[0], record[1]) for record in records[1:]]


def canonical_postgres_result(
    result: dict[str, Any], description: list[tuple[str, str]]
) -> dict[str, Any]:
    described_columns = [column for column, _ in description]
    if result["columns"] != described_columns:
        raise RuntimeError(
            "PostgreSQL result header does not match \\gdesc: "
            f"COPY={result['columns']!r}, description={described_columns!r}"
        )
    numeric_types = {
        "smallint",
        "integer",
        "bigint",
        "numeric",
        "real",
        "double precision",
    }
    numeric_columns = {
        index
        for index, (_, type_name) in enumerate(description)
        if type_name.partition("(")[0].strip() in numeric_types
    }
    return {
        "columns": result["columns"],
        "rows": [
            [
                canonical_numeric(cell) if index in numeric_columns else cell
                for index, cell in enumerate(row)
            ]
            for row in result["rows"]
        ],
    }


def postgres_result(container: str, database: str, query: str) -> dict[str, Any]:
    description = postgres_result_description(container, database, query)
    copy = (
        f"COPY ({without_semicolon(query)}) TO STDOUT "
        "WITH (FORMAT csv, HEADER true, NULL '<NULL>')"
    )
    output = psql(container, database, "-c", copy).decode()
    records = list(csv.reader(io.StringIO(output)))
    if not records:
        raise RuntimeError("PostgreSQL COPY returned no header")
    return canonical_postgres_result(
        {"columns": records[0], "rows": records[1:]}, description
    )


def postgres_execution_ms(container: str, database: str, query: str) -> float:
    explain = (
        "EXPLAIN (ANALYZE true, FORMAT JSON, TIMING false, SUMMARY true) "
        + without_semicolon(query)
    )
    output = psql(container, database, "-tA", "-c", explain).decode().strip()
    payload = json.loads(output)
    return float(payload[0]["Execution Time"])


def build_uqa_runner() -> pathlib.Path:
    configured = os.environ.get("UQA_TPCH_RUNNER")
    runner = (
        pathlib.Path(configured)
        if configured
        else ROOT / "target" / "release" / "examples" / "tpch_runner"
    )
    if configured and not runner.is_file():
        raise RuntimeError(f"UQA_TPCH_RUNNER does not exist: {runner}")
    if not configured:
        subprocess.run(
            [
                "cargo",
                "build",
                "--release",
                "-p",
                "uqa-engine",
                "--example",
                "tpch_runner",
            ],
            cwd=ROOT,
            check=True,
        )
    return runner


def run_uqa(iterations: int) -> dict[str, Any]:
    runner = build_uqa_runner()
    output = command([str(runner), "--iterations", str(iterations)]).stdout
    return json.loads(output)


def first_difference(expected: dict[str, Any], actual: dict[str, Any]) -> str:
    if expected["columns"] != actual["columns"]:
        return f"columns: PG={expected['columns']!r}, UQA={actual['columns']!r}"
    if len(expected["rows"]) != len(actual["rows"]):
        return f"row count: PG={len(expected['rows'])}, UQA={len(actual['rows'])}"
    for row_index, (pg_row, uqa_row) in enumerate(
        zip(expected["rows"], actual["rows"])
    ):
        if pg_row != uqa_row:
            return f"row {row_index}: PG={pg_row!r}, UQA={uqa_row!r}"
    return "unknown difference"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--container",
        default=os.environ.get("UQA_TPCH_PG_CONTAINER", "uqa-tpch-pg18"),
    )
    parser.add_argument(
        "--database", default=os.environ.get("UQA_TPCH_PG_DATABASE", "uqa_tpch")
    )
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--output", type=pathlib.Path, default=DEFAULT_REPORT)
    parser.add_argument(
        "--write-expected",
        action="store_true",
        help="replace the checked-in PG18 result fixture after a successful match",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.iterations <= 0:
        raise RuntimeError("--iterations must be positive")
    version = postgres_version(args.container, args.database)
    print(f"PostgreSQL {version['server_version']} in {args.container}", flush=True)
    reset_postgres(args.container, args.database)
    print("Loaded TPC-H dbgen SF 0.001 fixture", flush=True)

    workload = queries()
    pg_results = []
    pg_times = []
    for index, query in enumerate(workload, 1):
        pg_results.append(postgres_result(args.container, args.database, query))
        samples = [
            postgres_execution_ms(args.container, args.database, query)
            for _ in range(args.iterations)
        ]
        pg_times.append(samples)
        print(f"PG18 Q{index:02}: {statistics.median(samples):.3f} ms", flush=True)

    uqa = run_uqa(args.iterations)
    if len(uqa.get("queries", [])) != len(workload):
        raise RuntimeError(
            f"UQA runner returned {len(uqa.get('queries', []))} queries; "
            f"expected {len(workload)}"
        )
    if [query.get("query") for query in uqa["queries"]] != list(range(1, 23)):
        raise RuntimeError("UQA runner returned an invalid query sequence")
    comparisons = []
    mismatches = []
    for index, (pg_result, uqa_query, pg_elapsed) in enumerate(
        zip(pg_results, uqa["queries"], pg_times), 1
    ):
        uqa_result = uqa_query["result"]
        matches = pg_result == uqa_result
        uqa_median = statistics.median(uqa_query["elapsed_ms"])
        pg_median = statistics.median(pg_elapsed)
        item = {
            "query": index,
            "matches": matches,
            "rows": len(pg_result["rows"]),
            "postgres_execution_ms": pg_elapsed,
            "postgres_median_ms": pg_median,
            "uqa_execution_ms": uqa_query["elapsed_ms"],
            "uqa_median_ms": uqa_median,
            "uqa_over_postgres": uqa_median / pg_median if pg_median else None,
        }
        comparisons.append(item)
        if not matches:
            mismatches.append(
                f"Q{index:02} {first_difference(pg_result, uqa_result)}"
            )

    report = {
        "schema_version": 1,
        "workload": "tpch-dbgen-2.14.0-sf0.001-default-queries",
        "disclaimer": (
            "Local compatibility/regression measurement; not a compliant, audited, "
            "or published TPC-H result."
        ),
        "postgres": version,
        "uqa": {
            "load_ms": uqa["load_ms"],
            "iterations": args.iterations,
        },
        "exact_match": not mismatches,
        "queries": comparisons,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")

    if mismatches:
        print("Result mismatches:", file=sys.stderr)
        for mismatch in mismatches:
            print(f"  {mismatch}", file=sys.stderr)
        print(f"Report: {args.output}", file=sys.stderr)
        return 1

    if args.write_expected:
        DEFAULT_EXPECTED.parent.mkdir(parents=True, exist_ok=True)
        DEFAULT_EXPECTED.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "postgres": version,
                    "workload": report["workload"],
                    "queries": [
                        {"query": index, "result": result}
                        for index, result in enumerate(pg_results, 1)
                    ],
                },
                indent=2,
            )
            + "\n"
        )
        print(f"Expected results: {DEFAULT_EXPECTED}")
    print(f"exact_match=22/22 report={args.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2) from error
