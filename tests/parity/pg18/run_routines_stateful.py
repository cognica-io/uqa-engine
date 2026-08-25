#!/usr/bin/env python3
"""Stateful PostgreSQL 18.4 + AGE routine parity runner."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import re
import secrets
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).parent
REPO_ROOT = HERE.parent.parent.parent
FIXTURE = HERE / "routines_stateful.sql"
EXPECTED = HERE / "routines_stateful.expected.json"
USQL = os.environ.get("UQA_USQL", str(REPO_ROOT / "target" / "release" / "usql"))
PG_CONTAINER = os.environ.get("UQA_PG_CONTAINER", "uqa-pg18-age")
PG_DATABASE = os.environ.get("UQA_PG_DATABASE", "postgres")
SCHEMA_PLACEHOLDER = "__UQA_STATEFUL_SCHEMA__"
ORACLE_SERVER_VERSION_NUM = "180004"
CASE_START = re.compile(r"^-- @case ([a-z0-9_]+) (ok|rows|error)$")
SQLSTATE_ERROR = re.compile(r"^ERROR:\s+([0-9A-Z]{5}):")


@dataclass(frozen=True)
class Case:
    name: str
    mode: str
    sql: str


def parse_cases(source: str) -> list[Case]:
    """Parse explicitly delimited statements without guessing at SQL quoting."""
    cases: list[Case] = []
    current: tuple[str, str] | None = None
    body: list[str] = []
    seen: set[str] = set()
    for line_number, line in enumerate(source.splitlines(), 1):
        start = CASE_START.fullmatch(line)
        if start is not None:
            if current is not None:
                raise RuntimeError(f"nested @case at {FIXTURE}:{line_number}")
            current = (start.group(1), start.group(2))
            body = []
            continue
        if line == "-- @end":
            if current is None:
                raise RuntimeError(f"orphan @end at {FIXTURE}:{line_number}")
            name, mode = current
            sql = "\n".join(body).strip()
            if not sql:
                raise RuntimeError(f"empty SQL for case {name}")
            if name in seen:
                raise RuntimeError(f"duplicate case name {name}")
            seen.add(name)
            cases.append(Case(name=name, mode=mode, sql=sql))
            current = None
            body = []
            continue
        if current is not None:
            body.append(line)
        elif line.strip() and not line.lstrip().startswith("--"):
            raise RuntimeError(f"SQL outside @case at {FIXTURE}:{line_number}")
    if current is not None:
        raise RuntimeError(f"unterminated @case {current[0]}")
    if not cases:
        raise RuntimeError("stateful routine fixture has no cases")
    if SCHEMA_PLACEHOLDER not in cases[0].sql:
        raise RuntimeError("first case must create the runner-provided schema")
    return cases


def quote_identifier(identifier: str) -> str:
    return '"' + identifier.replace('"', '""') + '"'


def sql_error(output: str) -> str | None:
    line = next(
        (line for line in output.splitlines() if line.lstrip().startswith("ERROR:")),
        None,
    )
    if line is None:
        return None
    match = SQLSTATE_ERROR.match(line.lstrip())
    if match is None:
        raise RuntimeError(f"SQL error does not expose a SQLSTATE: {line}")
    return match.group(1)


def require_success(program: str, proc: subprocess.CompletedProcess[str]) -> None:
    if proc.returncode == 0:
        return
    detail = proc.stderr.strip() or proc.stdout.strip() or "no diagnostic output"
    raise RuntimeError(f"{program} exited with status {proc.returncode}: {detail}")


def decode_copy_cell(cell: str) -> str | None:
    if cell == r"\N":
        return None
    decoded: list[str] = []
    index = 0
    escapes = {"b": "\b", "f": "\f", "n": "\n", "r": "\r", "t": "\t", "v": "\v"}
    while index < len(cell):
        if cell[index] != "\\":
            decoded.append(cell[index])
            index += 1
            continue
        index += 1
        if index == len(cell):
            raise RuntimeError(f"COPY cell ends with an escape: {cell!r}")
        escaped = cell[index]
        if escaped in escapes:
            decoded.append(escapes[escaped])
            index += 1
        elif escaped in "01234567":
            end = index + 1
            while end < min(index + 3, len(cell)) and cell[end] in "01234567":
                end += 1
            decoded.append(chr(int(cell[index:end], 8)))
            index = end
        elif escaped == "x":
            end = index + 1
            while (
                end < min(index + 3, len(cell))
                and cell[end] in "0123456789abcdefABCDEF"
            ):
                end += 1
            if end == index + 1:
                decoded.append("x")
                index += 1
            else:
                decoded.append(chr(int(cell[index + 1 : end], 16)))
                index = end
        else:
            decoded.append(escaped)
            index += 1
    return "".join(decoded)


def copy_rows(payload: str) -> list[list[str | None]]:
    if not payload:
        return []
    lines = payload.split("\n")
    if lines.pop() != "":
        raise RuntimeError("COPY output does not end with a row terminator")
    return [[decode_copy_cell(cell) for cell in line.split("\t")] for line in lines]


def transcript_entry(
    case: Case, stdout: str, stderr: str, returncode: int, error_output: str
) -> dict:
    if returncode != 0:
        error = sql_error(error_output)
        if error is not None:
            return {"name": case.name, "kind": "error", "sqlstate": error}
        detail = stderr.strip() or stdout.strip() or "no diagnostic output"
        raise RuntimeError(f"case {case.name} failed outside SQL: {detail}")
    if case.mode == "rows":
        return {"name": case.name, "kind": "rows", "rows": copy_rows(stdout)}
    if stdout:
        raise RuntimeError(
            f"case {case.name} unexpectedly wrote row output: {stdout!r}"
        )
    return {"name": case.name, "kind": "ok"}


def psql_base() -> list[str]:
    return [
        "docker",
        "exec",
        "-i",
        "-e",
        "PGOPTIONS=-c client_min_messages=warning",
        PG_CONTAINER,
        "psql",
        "-U",
        "postgres",
        "-d",
        PG_DATABASE,
        "-X",
        "-q",
        "-v",
        "ON_ERROR_STOP=1",
        "-v",
        "VERBOSITY=verbose",
    ]


def pg_query(sql: str, *, timeout: int = 30) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        psql_base() + ["-c", sql], capture_output=True, text=True, timeout=timeout
    )


def pg_oracle_metadata() -> dict[str, str]:
    proc = pg_query(
        "COPY (SELECT current_setting('server_version'), "
        "current_setting('server_version_num'), version(), "
        "COALESCE((SELECT extversion FROM pg_catalog.pg_extension WHERE extname = 'age'), '')) "
        "TO STDOUT"
    )
    require_success("PostgreSQL oracle metadata query", proc)
    rows = copy_rows(proc.stdout)
    if len(rows) != 1 or len(rows[0]) != 4 or any(value is None for value in rows[0]):
        raise RuntimeError(f"unexpected PostgreSQL oracle metadata: {rows!r}")
    server_version, server_version_num, version, age_version = rows[0]
    assert server_version is not None
    assert server_version_num is not None
    assert version is not None
    assert age_version is not None
    if server_version_num != ORACLE_SERVER_VERSION_NUM:
        raise RuntimeError(
            f"stateful oracle requires PostgreSQL 18.4 ({ORACLE_SERVER_VERSION_NUM}), got "
            f"{server_version} ({server_version_num})"
        )
    if not age_version:
        raise RuntimeError("stateful oracle requires Apache AGE in pg_extension")
    return {
        "server_version": server_version,
        "server_version_num": server_version_num,
        "version": version,
        "age_version": age_version,
    }


def execute_pg_case(case: Case, schema: str) -> dict:
    sql = case.sql.replace(SCHEMA_PLACEHOLDER, quote_identifier(schema))
    prefix = (
        ""
        if SCHEMA_PLACEHOLDER in case.sql
        else f"SET search_path = {quote_identifier(schema)}, pg_catalog;\n"
    )
    if case.mode == "rows":
        query = sql.rstrip().removesuffix(";")
        command = f"{prefix}COPY ({query}) TO STDOUT"
    else:
        command = prefix + sql
    proc = pg_query(command)
    return transcript_entry(
        case, proc.stdout, proc.stderr, proc.returncode, proc.stderr
    )


def run_postgres(cases: list[Case]) -> tuple[dict[str, str], list[dict]]:
    metadata = pg_oracle_metadata()
    extension = pg_query("CREATE EXTENSION IF NOT EXISTS btree_gist")
    require_success("PostgreSQL btree_gist setup", extension)
    schema = f"uqa_pg18_stateful_{os.getpid()}_{secrets.token_hex(4)}"
    entries: list[dict] = []
    try:
        for case in cases:
            entry = execute_pg_case(case, schema)
            entries.append(entry)
    finally:
        cleanup = pg_query(
            f"DROP SCHEMA IF EXISTS {quote_identifier(schema)} CASCADE", timeout=60
        )
        require_success("PostgreSQL stateful fixture cleanup", cleanup)
    return metadata, entries


def execute_usql_case(case: Case, schema: str, database: Path) -> dict:
    sql = case.sql.replace(SCHEMA_PLACEHOLDER, quote_identifier(schema))
    prefix = (
        ""
        if SCHEMA_PLACEHOLDER in case.sql
        else f"SET search_path = {quote_identifier(schema)}, pg_catalog;\n"
    )
    proc = subprocess.run(
        [USQL, "--db", str(database), "--copy-text", "-c", prefix + sql],
        capture_output=True,
        text=True,
        timeout=30,
    )
    return transcript_entry(
        case, proc.stdout, proc.stderr, proc.returncode, proc.stdout
    )


def run_uqa(cases: list[Case]) -> list[dict]:
    if not Path(USQL).is_file():
        raise RuntimeError(
            f"usql binary not found at {USQL}; run `cargo build --release -p uqa-cli` "
            "or set UQA_USQL"
        )
    schema = f"uqa_pg18_stateful_{os.getpid()}_{secrets.token_hex(4)}"
    with tempfile.TemporaryDirectory(prefix="uqa-pg18-stateful-") as directory:
        database = Path(directory) / "state.db"
        return [execute_usql_case(case, schema, database) for case in cases]


def fixture_sha256() -> str:
    return hashlib.sha256(FIXTURE.read_bytes()).hexdigest()


def expected_document(metadata: dict[str, str], entries: list[dict]) -> dict:
    return {
        "schema_version": 1,
        "fixture_sha256": fixture_sha256(),
        "oracle": metadata,
        "cases": entries,
    }


def validate_expected_document(document: object, cases: list[Case]) -> dict:
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "fixture_sha256",
        "oracle",
        "cases",
    }:
        raise RuntimeError(
            "stateful expected transcript has an invalid top-level shape"
        )
    if document["schema_version"] != 1:
        raise RuntimeError("stateful expected transcript schema_version must be 1")
    digest = document["fixture_sha256"]
    if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise RuntimeError(
            "stateful expected transcript has an invalid fixture SHA-256"
        )
    if digest != fixture_sha256():
        raise RuntimeError(
            "routines_stateful.sql changed; regenerate the transcript with "
            "--update-expected against PostgreSQL 18.4 + AGE"
        )

    oracle = document["oracle"]
    oracle_keys = {"server_version", "server_version_num", "version", "age_version"}
    if (
        not isinstance(oracle, dict)
        or set(oracle) != oracle_keys
        or any(
            not isinstance(oracle[key], str) or not oracle[key] for key in oracle_keys
        )
        or oracle["server_version_num"] != ORACLE_SERVER_VERSION_NUM
    ):
        raise RuntimeError("stateful expected transcript has invalid oracle provenance")

    entries = document["cases"]
    if not isinstance(entries, list) or len(entries) != len(cases):
        raise RuntimeError("stateful expected transcript has an invalid case count")
    for case, entry in zip(cases, entries):
        if not isinstance(entry, dict):
            raise RuntimeError(f"stateful expected case {case.name} is not an object")
        required = {
            "ok": {"name", "kind"},
            "rows": {"name", "kind", "rows"},
            "error": {"name", "kind", "sqlstate"},
        }[case.mode]
        if (
            set(entry) != required
            or entry.get("name") != case.name
            or entry.get("kind") != case.mode
        ):
            raise RuntimeError(
                f"stateful expected case {case.name} has an invalid shape"
            )
        if case.mode == "rows":
            rows = entry["rows"]
            if not isinstance(rows, list) or any(
                not isinstance(row, list)
                or any(cell is not None and not isinstance(cell, str) for cell in row)
                for row in rows
            ):
                raise RuntimeError(
                    f"stateful expected case {case.name} has invalid rows"
                )
        elif case.mode == "error" and (
            not isinstance(entry["sqlstate"], str)
            or re.fullmatch(r"[0-9A-Z]{5}", entry["sqlstate"]) is None
        ):
            raise RuntimeError(
                f"stateful expected case {case.name} has invalid SQLSTATE"
            )
    return document


def load_expected(cases: list[Case]) -> dict:
    return validate_expected_document(json.loads(EXPECTED.read_text()), cases)


def write_expected(document: dict) -> None:
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w",
            encoding="utf-8",
            dir=HERE,
            prefix=".routines-stateful-",
            delete=False,
        ) as output:
            temporary = Path(output.name)
            json.dump(document, output, indent=2, ensure_ascii=False)
            output.write("\n")
        os.replace(temporary, EXPECTED)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()


def compare(label: str, actual: list[dict], expected: list[dict]) -> list[str]:
    differences: list[str] = []
    if len(actual) != len(expected):
        return [f"{label}: expected {len(expected)} cases, got {len(actual)}"]
    for actual_entry, expected_entry in zip(actual, expected):
        if actual_entry != expected_entry:
            differences.append(
                f"{label} {expected_entry['name']}: expected "
                f"{json.dumps(expected_entry, ensure_ascii=False, sort_keys=True)}, got "
                f"{json.dumps(actual_entry, ensure_ascii=False, sort_keys=True)}"
            )
    return differences


def validate_declared_modes(label: str, cases: list[Case], entries: list[dict]) -> None:
    if len(cases) != len(entries):
        raise RuntimeError(
            f"{label} returned {len(entries)} entries for {len(cases)} cases"
        )
    mismatches = [
        f"{case.name}: declared {case.mode}, got {entry['kind']}"
        for case, entry in zip(cases, entries)
        if case.mode != entry["kind"]
    ]
    if mismatches:
        raise RuntimeError(f"{label} violated fixture modes: {'; '.join(mismatches)}")


def main() -> int:
    global FIXTURE, EXPECTED
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--backend",
        choices=("both", "postgres", "uqa"),
        default="both",
        help="run both backends (default) or one backend while debugging",
    )
    parser.add_argument(
        "--update-expected",
        action="store_true",
        help="replace the checked-in transcript from the PostgreSQL 18.4 + AGE oracle",
    )
    parser.add_argument(
        "--fixture",
        type=Path,
        default=FIXTURE,
        help="stateful SQL fixture (default: routines_stateful.sql)",
    )
    parser.add_argument(
        "--expected",
        type=Path,
        default=EXPECTED,
        help="expected JSON transcript (default: routines_stateful.expected.json)",
    )
    args = parser.parse_args()
    if args.update_expected and args.backend != "postgres":
        parser.error("--update-expected requires --backend postgres")

    FIXTURE = args.fixture.resolve()
    EXPECTED = args.expected.resolve()
    if FIXTURE.parent != HERE or EXPECTED.parent != HERE:
        parser.error("--fixture and --expected must remain inside tests/parity/pg18")
    cases = parse_cases(FIXTURE.read_text())
    document: dict | None = None if args.update_expected else load_expected(cases)
    run_pg = args.backend in {"both", "postgres"}
    run_uq = args.backend in {"both", "uqa"}
    metadata: dict[str, str] | None = None
    pg_entries: list[dict] | None = None
    uqa_entries: list[dict] | None = None

    if run_pg and run_uq:
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            pg_future = executor.submit(run_postgres, cases)
            uqa_future = executor.submit(run_uqa, cases)
            metadata, pg_entries = pg_future.result()
            uqa_entries = uqa_future.result()
    elif run_pg:
        metadata, pg_entries = run_postgres(cases)
    else:
        uqa_entries = run_uqa(cases)

    if pg_entries is not None:
        validate_declared_modes("PostgreSQL", cases, pg_entries)

    if args.update_expected:
        assert metadata is not None and pg_entries is not None
        document = validate_expected_document(
            expected_document(metadata, pg_entries), cases
        )
        write_expected(document)
        print(f"updated {EXPECTED.relative_to(REPO_ROOT)} from PostgreSQL 18.4 + AGE")
    assert document is not None

    expected_metadata = document["oracle"]
    if metadata is not None and (
        metadata["server_version_num"] != expected_metadata["server_version_num"]
        or metadata["age_version"] != expected_metadata["age_version"]
    ):
        raise RuntimeError(
            "live PostgreSQL/AGE oracle version does not match the checked-in transcript"
        )

    expected_entries = document["cases"]
    differences: list[str] = []
    if pg_entries is not None:
        differences.extend(compare("PostgreSQL", pg_entries, expected_entries))
    if uqa_entries is not None:
        differences.extend(compare("UQA", uqa_entries, expected_entries))
    if differences:
        print(f"stateful routine parity: cases={len(cases)} diff={len(differences)}")
        for difference in differences:
            print(difference)
        return 1
    backends = "+".join(
        label for label, enabled in (("PostgreSQL", run_pg), ("UQA", run_uq)) if enabled
    )
    print(
        f"stateful routine parity: cases={len(cases)} match={len(cases)} backends={backends}"
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (
        json.JSONDecodeError,
        OSError,
        RuntimeError,
        subprocess.SubprocessError,
    ) as error:
        print(f"stateful routine parity failed: {error}", file=sys.stderr)
        raise SystemExit(2) from error
