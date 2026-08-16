#!/usr/bin/env python3
"""Differential PG18-vs-uqa probe runner.

Runs each probe from probes.sql against real PostgreSQL 18 (docker
container uqa-pg18 via psql) and against usql (uqa-rs release
binary), normalizes both outputs, and reports mismatches by category.
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent
REPO_ROOT = HERE.parent.parent.parent
MANIFEST = HERE / "manifest.json"
USQL = os.environ.get("UQA_USQL", str(REPO_ROOT / "target" / "release" / "usql"))
PG_CONTAINER = os.environ.get("UQA_PG_CONTAINER", "uqa-pg18")
PG_DATABASE = os.environ.get("UQA_PG_DATABASE", "uqa")
PSQL = [
    "docker", "exec", "-i", PG_CONTAINER,
    "psql", "-U", "postgres", "-d", PG_DATABASE,
    "-X", "-q", "-v", "ON_ERROR_STOP=0", "-v", "VERBOSITY=verbose",
]

SQLSTATE_ERROR = re.compile(r"^ERROR:\s+([0-9A-Z]{5}):")


def validate_manifest() -> dict:
    """Validate compatibility accounting before using its evidence."""
    manifest = json.loads(MANIFEST.read_text())
    if manifest.get("schema_version") != 1:
        raise RuntimeError("PG18 manifest schema_version must be 1")
    if manifest.get("oracle", {}).get("major") != 18:
        raise RuntimeError("PG18 manifest oracle major must be 18")

    parser_chain = manifest.get("parser_chain", {})
    for field in ("wrapper_revision", "library_revision"):
        revision = parser_chain.get(field)
        if not isinstance(revision, str) or re.fullmatch(r"[0-9a-f]{40}", revision) is None:
            raise RuntimeError(f"PG18 manifest {field} must be a full Git revision")
    cargo_manifest = (REPO_ROOT / "Cargo.toml").read_text()
    dependency = re.search(r"^pg_query\s*=\s*\{([^}]*)\}$", cargo_manifest, re.MULTILINE)
    if dependency is None:
        raise RuntimeError("workspace pg_query dependency is missing")
    revision = re.search(r'rev\s*=\s*"([0-9a-f]{40})"', dependency.group(1))
    if revision is None or revision.group(1) != parser_chain["wrapper_revision"]:
        raise RuntimeError("PG18 manifest wrapper revision does not match Cargo.toml")

    milestones = manifest.get("milestones")
    expected_milestones = {f"M{index}" for index in range(7)}
    if not isinstance(milestones, dict) or set(milestones) != expected_milestones:
        raise RuntimeError("PG18 manifest must account for milestones M0 through M6")
    milestone_states = {"not_started", "in_progress", "complete"}
    invalid_milestones = {
        name: state for name, state in milestones.items() if state not in milestone_states
    }
    if invalid_milestones:
        raise RuntimeError(f"PG18 manifest has invalid milestone states: {invalid_milestones}")

    items = manifest.get("items")
    if not isinstance(items, list) or not items:
        raise RuntimeError("PG18 manifest must contain compatibility items")
    required = {
        "id",
        "postgresql_reference",
        "uqa_test",
        "supported_version",
        "status",
        "open_issue",
    }
    statuses = {"verified", "partial", "explicitly_rejected", "not_audited"}
    seen = set()
    for index, item in enumerate(items):
        if not isinstance(item, dict) or set(item) != required:
            raise RuntimeError(f"PG18 manifest item {index} has an invalid shape")
        item_id = item["id"]
        if not isinstance(item_id, str) or not item_id or item_id in seen:
            raise RuntimeError(f"PG18 manifest item {index} has a duplicate or empty id")
        seen.add(item_id)
        for field in ("postgresql_reference", "uqa_test", "supported_version"):
            if not isinstance(item[field], str) or not item[field].strip():
                raise RuntimeError(f"PG18 manifest item {item_id} has an empty {field}")
        if item["status"] not in statuses:
            raise RuntimeError(f"PG18 manifest item {item_id} has an invalid status")
        if item["status"] == "verified":
            if item["open_issue"] is not None:
                raise RuntimeError(f"verified PG18 manifest item {item_id} has an open issue")
        elif not isinstance(item["open_issue"], str) or not item["open_issue"].strip():
            raise RuntimeError(f"incomplete PG18 manifest item {item_id} lacks an open issue")

    if manifest.get("complete_compatibility_claim") is True:
        if any(item["status"] != "verified" for item in items):
            raise RuntimeError("complete PG18 compatibility cannot be claimed with incomplete items")
        if any(state != "complete" for state in milestones.values()):
            raise RuntimeError("complete PG18 compatibility cannot be claimed before M0-M6 complete")
    elif manifest.get("complete_compatibility_claim") is not False:
        raise RuntimeError("complete_compatibility_claim must be a boolean")
    return manifest


def sql_error(output: str) -> str | None:
    """Return a SQLSTATE, excluding unrelated process failures."""
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
    """Abort the comparison when the oracle or subject process did not run."""
    if proc.returncode == 0:
        return
    detail = proc.stderr.strip() or proc.stdout.strip() or "no diagnostic output"
    raise RuntimeError(f"{program} exited with status {proc.returncode}: {detail}")


def run_pg(query: str) -> tuple[str, str]:
    """Returns (kind, payload): kind in {ok, error}."""
    query = query.rstrip().removesuffix(";")
    proc = subprocess.run(
        PSQL + ["-c", f"COPY ({query}) TO STDOUT"],
        capture_output=True,
        text=True,
        timeout=30,
    )
    error = sql_error(proc.stderr)
    if error is not None:
        return ("error", error)
    require_success("psql", proc)
    return ("ok", proc.stdout)


def run_usql(query: str) -> tuple[str, str]:
    proc = subprocess.run(
        [USQL, "--copy-text", "-c", query],
        capture_output=True,
        text=True,
        timeout=30,
    )
    out = proc.stdout
    err = proc.stderr.strip()
    error = sql_error(out + "\n" + err)
    if error is not None:
        return ("error", error)
    require_success("usql", proc)
    return ("ok", out)


def decode_copy_cell(cell: str) -> str | None:
    """Decode one PostgreSQL COPY text cell without conflating NULL and text."""
    if cell == r"\N":
        return None
    decoded = []
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
            while end < min(index + 3, len(cell)) and cell[end] in "0123456789abcdefABCDEF":
                end += 1
            if end == index + 1:
                decoded.append("x")
                index += 1
            else:
                decoded.append(chr(int(cell[index + 1:end], 16)))
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
    return [
        [decode_copy_cell(cell) for cell in line.split("\t")]
        for line in lines
    ]


def normalize(kind: str, payload: str) -> str:
    if kind == "error":
        return f"<ERROR:{payload}>"
    rows = copy_rows(payload)
    return json.dumps(rows, ensure_ascii=False, separators=(",", ":"))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--validate-manifest",
        action="store_true",
        help="validate compatibility accounting without running live probes",
    )
    args = parser.parse_args()
    manifest = validate_manifest()
    if args.validate_manifest:
        incomplete = sum(item["status"] != "verified" for item in manifest["items"])
        print(f"PG18 manifest valid: items={len(manifest['items'])} incomplete={incomplete}")
        return 0
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
        category = (
            "engine-error" if uq_kind == "error" and pg_kind == "ok"
            else "engine-accepts" if uq_kind == "ok" and pg_kind == "error"
            else "sqlstate-mismatch" if uq_kind == "error" and pg_kind == "error"
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
            pg_disp = pg.replace("\t", "|").replace("\n", "|")[:90]
            uq_disp = uq.replace("\t", "|").replace("\n", "|")[:90]
            print(f"  {q}\n     PG: {pg_disp}\n     UQ: {uq_disp}")
    return 1 if diffs else 0


if __name__ == "__main__":
    raise SystemExit(main())
