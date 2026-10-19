#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Compare an installed usql with a local PostgreSQL 18 server on the TPC-H fixture.

UQA runs through the selected ``usql`` executable and its in-memory engine;
PostgreSQL runs through ``psql`` over its default local connection. Every UQA
process loads the fixture with the same 128-row INSERT batches as the Rust
fixture loader and validates all 22 results against ``expected/pg18.json``
before any timed execution. PostgreSQL is validated against the same fixture.

Each round executes every query once untimed and then ``--iterations`` times
for UQA, PostgreSQL client round trips and PostgreSQL EXPLAIN ANALYZE. Rounds
alternate the engine order so that neither engine always runs first.
"""

from __future__ import annotations

import argparse
import csv
import datetime
import io
import json
import pathlib
import platform
import re
import statistics
import subprocess
import sys
import time
from decimal import Decimal, InvalidOperation
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "benchmarks" / "tpch"
DEFAULT_OUTPUT = ROOT / "target" / "benchmark-runs" / "tpch-release-comparison"
WORKLOAD = "tpch-dbgen-2.14.0-sf0.001-default-queries"
QUERY_COUNT = 22
INSERT_ROWS = 128
# Field kinds follow crates/uqa-engine/tests/support/tpch_fixture.rs:
# I = integer, N = numeric, D = date, T = text.
TABLES = (
    ("region", "ITT"),
    ("nation", "ITIT"),
    ("supplier", "ITTITNT"),
    ("customer", "ITTITNTT"),
    ("part", "ITTTTITNT"),
    ("partsupp", "IIINT"),
    ("orders", "IITNDTTIT"),
    ("lineitem", "IIIINNNNTTDDDTTT"),
)
ENGINES = ("uqa", "postgres_round_trip", "postgres_explain")
SERIES = ("uqa", "postgres_round_trip", "postgres_execution", "postgres_planning")
NUMERIC = re.compile(r"^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$")
NUMERIC_TYPES = {"smallint", "integer", "bigint", "numeric", "real", "double precision"}
TIME_LINE = re.compile(r"^Time: ([0-9]+\.[0-9]+) ms(?: \(.*\))?$")
MARKER = re.compile(r"^@@Q([0-9]{2})$")
END_MARKER = "@@END"


def command_output(args: list[str], *, input_bytes: bytes | None = None) -> str:
    completed = subprocess.run(
        args,
        input=input_bytes,
        stdin=None if input_bytes is not None else subprocess.DEVNULL,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        stderr = completed.stderr.decode(errors="replace").strip()
        raise RuntimeError(f"{args[0]} exited with {completed.returncode}: {stderr}")
    return completed.stdout.decode()


def without_semicolon(query: str) -> str:
    return query.rstrip().removesuffix(";").rstrip()


def meta_argument(path: pathlib.PurePath) -> str:
    """Quote a path as one single-quoted psql or usql meta-command argument."""
    return "'" + str(path).replace("\\", "\\\\").replace("'", "''") + "'"


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


def canonical_result(result: dict[str, Any], numeric_columns: set[int]) -> dict[str, Any]:
    """Canonicalize text cells the same way as ``scripts/run-tpch-pg18.py``."""
    return {
        "columns": list(result["columns"]),
        "rows": [
            [
                "<NULL>"
                if cell is None
                else canonical_numeric(cell)
                if index in numeric_columns
                else cell
                for index, cell in enumerate(row)
            ]
            for row in result["rows"]
        ],
    }


def numeric_columns(description: list[tuple[str, str]]) -> set[int]:
    return {
        index
        for index, (_, type_name) in enumerate(description)
        if type_name.partition("(")[0].strip() in NUMERIC_TYPES
    }


def render_row(fields: list[str], kinds: str) -> str:
    """Render one row whose field count the caller has checked against ``kinds``."""
    values = []
    for value, kind in zip(fields, kinds):
        if kind in "IN":
            values.append(value)
        else:
            values.append("'" + value.replace("'", "''") + "'")
    return "(" + ", ".join(values) + ")"


def table_rows(fixture: pathlib.Path, table: str) -> list[list[str]]:
    rows = []
    source = fixture / "data" / f"{table}.tbl"
    for number, line in enumerate(source.read_text(encoding="utf-8").splitlines(), 1):
        fields = line.split("|")
        if fields.pop() != "":
            raise RuntimeError(f"{source}:{number} does not end in '|'")
        rows.append(fields)
    return rows


def insert_statements(fixture: pathlib.Path) -> list[str]:
    """Render the fixture as the Rust loader's ordered 128-row INSERT batches."""
    statements = []
    for table, kinds in TABLES:
        rendered = []
        for fields in table_rows(fixture, table):
            if len(fields) != len(kinds):
                raise RuntimeError(f"{table} row has {len(fields)} fields; expected {len(kinds)}")
            rendered.append(render_row(fields, kinds))
        for start in range(0, len(rendered), INSERT_ROWS):
            batch = ",\n".join(rendered[start : start + INSERT_ROWS])
            statements.append(f"INSERT INTO {table} VALUES\n{batch};\n")
    return statements


def round_order(round_index: int) -> tuple[str, ...]:
    return ENGINES if round_index % 2 == 0 else tuple(reversed(ENGINES))


def marked_blocks(output: str) -> dict[int, list[str]]:
    blocks: dict[int, list[str]] = {}
    current: int | None = None
    for line in output.splitlines():
        if line == END_MARKER:
            current = None
            continue
        marker = MARKER.fullmatch(line)
        if marker:
            current = int(marker.group(1))
            if current in blocks:
                raise RuntimeError(f"duplicate marker for Q{current:02}")
            blocks[current] = []
        elif current is not None:
            blocks[current].append(line)
    return blocks


def parse_timings(
    output: str, executions: int
) -> tuple[dict[int, list[float]], dict[int, list[tuple[str, ...]]]]:
    """Return each query's ``\\timing`` values and the lines printed before each value."""
    samples: dict[int, list[float]] = {}
    outputs: dict[int, list[tuple[str, ...]]] = {}
    for number, lines in marked_blocks(output).items():
        samples[number] = []
        outputs[number] = []
        pending: list[str] = []
        for line in lines:
            timing = TIME_LINE.fullmatch(line)
            if timing:
                samples[number].append(float(timing.group(1)))
                outputs[number].append(tuple(pending))
                pending = []
            else:
                pending.append(line)
    for number in range(1, QUERY_COUNT + 1):
        produced = len(samples.get(number, []))
        if produced != executions:
            raise RuntimeError(f"Q{number:02} produced {produced} timings; expected {executions}")
    return samples, outputs


def parse_explain(output: str, executions: int) -> tuple[dict[int, list[float]], dict[int, list[float]]]:
    """Return each query's EXPLAIN ANALYZE execution and planning times."""
    decoder = json.JSONDecoder()
    execution: dict[int, list[float]] = {}
    planning: dict[int, list[float]] = {}
    for number, lines in marked_blocks(output).items():
        text = "\n".join(lines)
        execution[number] = []
        planning[number] = []
        position = 0
        while True:
            while position < len(text) and text[position].isspace():
                position += 1
            if position == len(text):
                break
            document, position = decoder.raw_decode(text, position)
            execution[number].append(float(document[0]["Execution Time"]))
            planning[number].append(float(document[0]["Planning Time"]))
    for number in range(1, QUERY_COUNT + 1):
        produced = len(execution.get(number, []))
        if produced != executions:
            raise RuntimeError(f"Q{number:02} produced {produced} EXPLAIN documents; expected {executions}")
    return execution, planning


def summary_table(queries: list[dict[str, Any]]) -> str:
    lines = [
        "| Query | UQA usql ms | PostgreSQL psql ms | PostgreSQL execution ms | UQA / psql | UQA / execution |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for item in queries:
        uqa = item["uqa"]["median_ms"]
        round_trip = item["postgres_round_trip"]["median_ms"]
        execution = item["postgres_execution"]["median_ms"]
        lines.append(
            f"| Q{item['query']:02} | {uqa:.3f} | {round_trip:.3f} | {execution:.3f} | "
            f"{uqa / round_trip:.2f}x | {uqa / execution:.2f}x |"
        )
    return "\n".join(lines)


def cpu_description() -> str:
    system = platform.system()
    if system == "Linux":
        return command_output(["lscpu"])
    if system == "Darwin":
        return command_output(["sysctl", "-n", "machdep.cpu.brand_string"]).strip()
    return platform.processor()


def memory_description() -> str:
    system = platform.system()
    if system == "Linux":
        lines = pathlib.Path("/proc/meminfo").read_text(encoding="utf-8").splitlines()
        return next(line for line in lines if line.startswith("MemTotal"))
    if system == "Darwin":
        return command_output(["sysctl", "-n", "hw.memsize"]).strip() + " bytes"
    return "unknown"


class ReleaseComparison:
    """One counterbalanced comparison session over the checked-in fixture."""

    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.output: pathlib.Path = args.output.resolve()
        self.queries = [
            (FIXTURE / "queries" / f"q{number:02}.sql").read_text(encoding="utf-8").strip()
            for number in range(1, QUERY_COUNT + 1)
        ]
        expected = json.loads((FIXTURE / "expected" / "pg18.json").read_text(encoding="utf-8"))
        self.expected = [query["result"] for query in expected["queries"]]
        self.numeric: list[set[int]] = []
        self.load_path = self.output / "uqa-load.sql"

    def psql(self, *args: str, input_bytes: bytes | None = None) -> str:
        return command_output(
            [self.args.psql, "-X", "-q", "-d", self.args.database, "-v", "ON_ERROR_STOP=1", *args],
            input_bytes=input_bytes,
        )

    def postgres_environment(self) -> dict[str, str]:
        version_number = self.psql("-tA", "-c", "SHOW server_version_num").strip()
        if not version_number.startswith("18"):
            raise RuntimeError(f"database {self.args.database!r} is not PostgreSQL 18: {version_number}")
        settings = {"server_version_num": version_number}
        for name in (
            "server_version",
            "shared_buffers",
            "work_mem",
            "jit",
            "max_parallel_workers_per_gather",
        ):
            settings[name] = self.psql("-tA", "-c", f"SHOW {name}").strip()
        settings["version"] = self.psql("-tA", "-c", "SELECT version()").strip()
        settings["database_locale"] = self.psql(
            "-tA",
            "-c",
            "SELECT datlocprovider::text || ' ' || datcollate || ' ' || datctype "
            "FROM pg_database WHERE datname = current_database()",
        ).strip()
        return settings

    def uqa_version(self) -> str:
        return command_output([self.args.usql, "--copy-text", "-c", "SELECT version()"]).strip()

    def load_postgres(self) -> None:
        self.psql("-c", "DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public;")
        self.psql("-f", str(FIXTURE / "schema.sql"))
        for table, _ in TABLES:
            payload = "".join("|".join(fields) + "\n" for fields in table_rows(FIXTURE, table))
            self.psql(
                "-c",
                f"COPY {table} FROM STDIN WITH (FORMAT csv, DELIMITER '|', QUOTE E'\\x01')",
                input_bytes=payload.encode(),
            )
        # A maintained heap keeps autovacuum from changing visibility state between rounds.
        self.psql("-c", "VACUUM (ANALYZE)")

    def validate_postgres(self) -> None:
        for number, query in enumerate(self.queries, 1):
            output = self.psql(
                "--csv", "-P", "footer=off", input_bytes=(without_semicolon(query) + "\n\\gdesc\n").encode()
            )
            records = list(csv.reader(io.StringIO(output)))
            if not records or records[0] != ["Column", "Type"]:
                raise RuntimeError(f"unexpected PostgreSQL result description: {records!r}")
            self.numeric.append(numeric_columns([(record[0], record[1]) for record in records[1:]]))
            copy = (
                f"COPY ({without_semicolon(query)}) TO STDOUT "
                "WITH (FORMAT csv, HEADER true, NULL '<NULL>')"
            )
            records = list(csv.reader(io.StringIO(self.psql("-c", copy))))
            result = canonical_result({"columns": records[0], "rows": records[1:]}, self.numeric[-1])
            if result != self.expected[number - 1]:
                raise RuntimeError(f"PostgreSQL Q{number:02} differs from expected/pg18.json")

    def timed_lines(self, statement: str) -> list[str]:
        return [statement] * (1 + self.args.iterations)

    def run_uqa(self, label: str) -> dict[int, list[float]]:
        lines = [
            "\\set ON_ERROR_STOP on",
            f"\\i {meta_argument(FIXTURE / 'schema.sql')}",
            f"\\i {meta_argument(self.load_path)}",
        ]
        if self.args.uqa_analyze:
            lines.append("ANALYZE;")
        lines.append("\\pset format json")
        for number, query in enumerate(self.queries, 1):
            lines += [f"\\o {meta_argument(self.output / f'{label}-q{number:02}.json')}", query, "\\o"]
        lines += ["\\pset format unaligned", "\\t on", "\\timing on"]
        for number, query in enumerate(self.queries, 1):
            lines.append(f"\\echo @@Q{number:02}")
            lines += self.timed_lines(query)
        lines.append(f"\\echo {END_MARKER}")
        script = self.output / f"{label}.sql"
        script.write_text("\n".join(lines) + "\n", encoding="utf-8")
        output = command_output([self.args.usql, str(script)])
        (self.output / f"{label}.log").write_text(output, encoding="utf-8")
        for number in range(1, QUERY_COUNT + 1):
            produced = json.loads((self.output / f"{label}-q{number:02}.json").read_text(encoding="utf-8"))
            if canonical_result(produced, self.numeric[number - 1]) != self.expected[number - 1]:
                raise RuntimeError(f"UQA {label} Q{number:02} differs from expected/pg18.json")
        samples, outputs = parse_timings(output, 1 + self.args.iterations)
        for number, printed in outputs.items():
            if any(lines != printed[0] for lines in printed):
                raise RuntimeError(f"UQA {label} Q{number:02} output changed across executions")
        return {number: values[1:] for number, values in samples.items()}

    def run_postgres_round_trip(self, label: str) -> dict[int, list[float]]:
        lines = ["\\o /dev/null", "\\timing on"]
        for number, query in enumerate(self.queries, 1):
            lines.append(f"\\echo @@Q{number:02}")
            lines += self.timed_lines(query)
        lines.append(f"\\echo {END_MARKER}")
        script = self.output / f"{label}.sql"
        script.write_text("\n".join(lines) + "\n", encoding="utf-8")
        output = self.psql("-f", str(script))
        (self.output / f"{label}.log").write_text(output, encoding="utf-8")
        samples, _ = parse_timings(output, 1 + self.args.iterations)
        return {number: values[1:] for number, values in samples.items()}

    def run_postgres_explain(self, label: str) -> tuple[dict[int, list[float]], dict[int, list[float]]]:
        lines = ["\\pset format unaligned", "\\t on"]
        for number, query in enumerate(self.queries, 1):
            lines.append(f"\\echo @@Q{number:02}")
            lines += self.timed_lines(
                "EXPLAIN (ANALYZE true, FORMAT JSON, TIMING false, SUMMARY true) " + without_semicolon(query) + ";"
            )
        lines.append(f"\\echo {END_MARKER}")
        script = self.output / f"{label}.sql"
        script.write_text("\n".join(lines) + "\n", encoding="utf-8")
        output = self.psql("-f", str(script))
        (self.output / f"{label}.log").write_text(output, encoding="utf-8")
        execution, planning = parse_explain(output, 1 + self.args.iterations)
        return (
            {number: values[1:] for number, values in execution.items()},
            {number: values[1:] for number, values in planning.items()},
        )

    def run(self) -> dict[str, Any]:
        self.output.mkdir(parents=True, exist_ok=True)
        environment = {
            "uname": " ".join(platform.uname()),
            "cpu": cpu_description(),
            "memory": memory_description(),
            "uqa_version": self.uqa_version(),
            "postgres": self.postgres_environment(),
        }
        self.load_postgres()
        self.validate_postgres()
        print("PostgreSQL matches expected/pg18.json for 22/22 queries", flush=True)
        self.load_path.write_text("".join(insert_statements(FIXTURE)), encoding="utf-8")

        series: dict[str, dict[int, list[list[float]]]] = {
            name: {number: [] for number in range(1, QUERY_COUNT + 1)} for name in SERIES
        }
        orders = []
        uqa_process_seconds = []
        for round_index in range(self.args.rounds):
            order = round_order(round_index)
            orders.append(list(order))
            for engine in order:
                label = f"r{round_index + 1}-{engine}"
                if engine == "uqa":
                    started = time.monotonic()
                    for number, values in self.run_uqa(label).items():
                        series["uqa"][number].append(values)
                    uqa_process_seconds.append(time.monotonic() - started)
                elif engine == "postgres_round_trip":
                    for number, values in self.run_postgres_round_trip(label).items():
                        series["postgres_round_trip"][number].append(values)
                else:
                    execution, planning = self.run_postgres_explain(label)
                    for number in execution:
                        series["postgres_execution"][number].append(execution[number])
                        series["postgres_planning"][number].append(planning[number])
            print(f"round {round_index + 1}/{self.args.rounds}: {' -> '.join(order)}", flush=True)

        queries = []
        for number in range(1, QUERY_COUNT + 1):
            item: dict[str, Any] = {"query": number, "rows": len(self.expected[number - 1]["rows"])}
            for name in SERIES:
                rounds = series[name][number]
                item[name] = {
                    "median_ms": statistics.median(value for values in rounds for value in values),
                    "round_medians_ms": [statistics.median(values) for values in rounds],
                    "samples_ms": rounds,
                }
            queries.append(item)
        return {
            "schema_version": 1,
            "workload": WORKLOAD,
            "disclaimer": "Comparison measurement; not a compliant, audited, or published TPC-H result.",
            "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
            "configuration": {
                "rounds": self.args.rounds,
                "untimed_executions_per_query_per_round": 1,
                "timed_executions_per_query_per_round": self.args.iterations,
                "round_orders": orders,
                "uqa_analyze": self.args.uqa_analyze,
                "uqa_load": "schema.sql and 128-row INSERT batches into the in-memory engine",
                "postgres_load": "COPY, then VACUUM (ANALYZE)",
            },
            "environment": environment,
            "validation": "PostgreSQL and every UQA process matched expected/pg18.json for all 22 queries",
            "uqa_process_seconds": uqa_process_seconds,
            "queries": queries,
        }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--usql", default="usql", help="usql executable to measure")
    parser.add_argument("--psql", default="psql", help="psql executable for the PostgreSQL 18 server")
    parser.add_argument(
        "--database",
        default="uqa_tpch",
        help="dedicated PostgreSQL database whose public schema is recreated",
    )
    parser.add_argument("--rounds", type=int, default=6)
    parser.add_argument("--iterations", type=int, default=34, help="timed executions per query per round")
    parser.add_argument("--uqa-analyze", action="store_true", help="run ANALYZE in UQA after loading")
    parser.add_argument("--output", type=pathlib.Path, default=DEFAULT_OUTPUT)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.rounds <= 0 or args.iterations <= 0:
        raise RuntimeError("--rounds and --iterations must be positive")
    report = ReleaseComparison(args).run()
    path = args.output.resolve() / "report.json"
    path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(summary_table(report["queries"]))
    print(f"report={path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
