#!/usr/bin/env python3
"""Differential PG18-vs-uqa probe runner.

Runs each probe from probes.sql against real PostgreSQL 18 (docker
container uqa-pg18 via psql) and against usql (uqa-rs release
binary), normalizes both outputs, and reports mismatches by category.
"""

import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent
REPO_ROOT = HERE.parent.parent.parent
USQL = os.environ.get("UQA_USQL", str(REPO_ROOT / "target" / "release" / "usql"))
PG_CONTAINER = os.environ.get("UQA_PG_CONTAINER", "uqa-pg18")
PG_DATABASE = os.environ.get("UQA_PG_DATABASE", "uqa")
FIELD_SEP = "\x1f"
PSQL = [
    "docker", "exec", "-i", PG_CONTAINER,
    "psql", "-U", "postgres", "-d", PG_DATABASE,
    "-tA", "-F", FIELD_SEP, "-v", "ON_ERROR_STOP=0", "-P", "null=<NULL>",
]


def sql_error(output: str) -> str | None:
    """Return a SQL error message, excluding unrelated process failures."""
    line = next(
        (line for line in output.splitlines() if line.lstrip().startswith("ERROR:")),
        None,
    )
    return line.split("ERROR:", 1)[1].strip() if line is not None else None


def require_success(program: str, proc: subprocess.CompletedProcess[str]) -> None:
    """Abort the comparison when the oracle or subject process did not run."""
    if proc.returncode == 0:
        return
    detail = proc.stderr.strip() or proc.stdout.strip() or "no diagnostic output"
    raise RuntimeError(f"{program} exited with status {proc.returncode}: {detail}")


def run_pg(query: str) -> tuple[str, str]:
    """Returns (kind, payload): kind in {ok, error}."""
    proc = subprocess.run(
        PSQL + ["-c", query], capture_output=True, text=True, timeout=30
    )
    error = sql_error(proc.stderr)
    if error is not None:
        return ("error", error)
    require_success("psql", proc)
    rows = [l for l in proc.stdout.splitlines()]
    return ("ok", "\n".join(rows).strip())


USQL_ROW_SEP = re.compile(r"^[-+| ]+$")


def aligned_cells(line: str, widths: list[int]) -> list[str]:
    """Extract cells from one fixed-width usql row without altering cell content."""
    cells = []
    offset = 0
    for index, width in enumerate(widths):
        cells.append(line[offset:offset + width].strip())
        offset += width
        if index + 1 < len(widths):
            if line[offset:offset + 3] != " | ":
                raise RuntimeError(f"malformed usql result row: {line!r}")
            offset += 3
    return cells


def separator_widths(separator: str) -> list[int]:
    """Recover the widths used by usql's `-+-` separator join."""
    parts = separator.split("+")
    if len(parts) == 1:
        return [len(parts[0])]
    widths = [
        len(part) - (1 if index in (0, len(parts) - 1) else 2)
        for index, part in enumerate(parts)
    ]
    if any(width <= 0 for width in widths):
        raise RuntimeError(f"malformed usql result separator: {separator!r}")
    return widths


def run_usql(query: str) -> tuple[str, str]:
    proc = subprocess.run(
        [USQL, "-c", query], capture_output=True, text=True, timeout=30
    )
    out = proc.stdout
    err = proc.stderr.strip()
    error = sql_error(out + "\n" + err)
    if error is not None:
        return ("error", error)
    require_success("usql", proc)
    lines = [l.rstrip() for l in out.splitlines() if l.strip()]
    # table shape: header, separator (---), value rows..., (N row(s))
    values = []
    in_body = False
    widths = None
    for index, line in enumerate(lines):
        if USQL_ROW_SEP.match(line) and set(line.strip()) <= set("-+ "):
            if index == 0:
                raise RuntimeError("usql result separator has no header")
            widths = separator_widths(line)
            in_body = True
            continue
        if in_body:
            if re.match(r"^\(\d+ row", line):
                break
            if widths is None:
                raise RuntimeError("usql result row has no column widths")
            values.append(FIELD_SEP.join(aligned_cells(line, widths)))
    return ("ok", "\n".join(values).strip())


FLOAT_RE = re.compile(r"^-?\d+\.\d+([eE][-+]?\d+)?$")
INT_RE = re.compile(r"^-?\d+$")


def normalize_cell(cell: str) -> str:
    c = cell.strip()
    if c in ("t", "true"):
        return "true"
    if c in ("f", "false"):
        return "false"
    if c in ("", "<NULL>", "NULL"):
        return "<NULL>"
    if FLOAT_RE.match(c):
        try:
            f = float(c)
            return f"~{f:.10g}"
        except ValueError:
            pass
    if INT_RE.match(c):
        return c.lstrip("+")
    return c


def normalize(kind: str, payload: str) -> str:
    if kind == "error":
        return "<ERROR>"
    rows = [
        FIELD_SEP.join(normalize_cell(cell) for cell in row.split(FIELD_SEP))
        for row in payload.splitlines()
    ]
    return "\n".join(rows)


def main() -> int:
    probes = [
        line.strip()
        for line in (HERE / "probes.sql").read_text().splitlines()
        if line.strip() and not line.strip().startswith("--")
    ]
    matches = 0
    diffs = []
    for query in probes:
        pg_kind, pg_payload = run_pg(query)
        uq_kind, uq_payload = run_usql(query)
        pg_norm = normalize(pg_kind, pg_payload)
        uq_norm = normalize(uq_kind, uq_payload)
        if pg_norm == uq_norm:
            matches += 1
            continue
        if pg_kind == "error" and uq_kind == "error":
            matches += 1  # both reject; message text not compared
            continue
        category = (
            "engine-error" if uq_kind == "error" and pg_kind == "ok"
            else "engine-accepts" if uq_kind == "ok" and pg_kind == "error"
            else "value-mismatch"
        )
        diffs.append((category, query, pg_payload if pg_kind == "ok" else f"ERROR: {pg_payload}",
                      uq_payload if uq_kind == "ok" else f"ERROR: {uq_payload}"))

    print(f"total={len(probes)} match={matches} diff={len(diffs)}")
    by_cat = {}
    for cat, q, pg, uq in diffs:
        by_cat.setdefault(cat, []).append((q, pg, uq))
    for cat, items in sorted(by_cat.items()):
        print(f"\n=== {cat} ({len(items)}) ===")
        for q, pg, uq in items:
            pg_disp = pg.replace(FIELD_SEP, "|").replace("\n", "|")[:90]
            uq_disp = uq.replace(FIELD_SEP, "|").replace("\n", "|")[:90]
            print(f"  {q}\n     PG: {pg_disp}\n     UQ: {uq_disp}")
    return 1 if diffs else 0


if __name__ == "__main__":
    raise SystemExit(main())
