#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Pinned, lossless fixture transport for standalone Docker reference drivers."""

import argparse
import base64
import difflib
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

from reference_runtime import ROOT, docker_command, prepare_jars


def encoded(text):
    return base64.b64encode(text.encode("utf-8")).decode("ascii")


def inventory(cases):
    identities = [case["id"] for case in cases]
    if not identities or len(set(identities)) != len(identities):
        raise RuntimeError("Empty or duplicate reference case inventory")
    if any(not re.fullmatch(r"[A-Za-z0-9_-]+", identity) for identity in identities):
        raise RuntimeError("Reference case identity is not safe for TSV transport")
    return identities


def provenance(stem, entrypoint, cases, output):
    return {
        "format_version": 1,
        "fixture_count": len(cases),
        "reference_manifest_sha256": hashlib.sha256((ROOT / "manifest.json").read_bytes()).hexdigest(),
        "source_sha256": hashlib.sha256((ROOT / entrypoint).read_bytes()).hexdigest(),
        "cases_sha256": hashlib.sha256((ROOT / f"{stem}_cases.json").read_bytes()).hexdigest(),
        "expected_sha256": hashlib.sha256(output.encode("utf-8")).hexdigest(),
    }


def canonical_output(stdout, runtime, identities):
    # str.splitlines would incorrectly split a JSON string containing NEL, LS, or PS.
    actual = [json.loads(line) for line in stdout.rstrip("\n").split("\n")]
    if actual[0] != {"runtime": runtime} or [row["id"] for row in actual[1:]] != identities:
        raise RuntimeError("Reference runtime or case inventory differs")
    return "".join(json.dumps(row, ensure_ascii=True, separators=(",", ":")) + "\n" for row in actual[1:])


def main(stem, entrypoint, fields, description):
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("--cache-dir", type=Path, default=Path(tempfile.gettempdir()) / "uqa-nori-reference-jars")
    parser.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], default="linux/arm64")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--write", action="store_true", help="Record a reviewed change to inputs or reference output")
    args = parser.parse_args()
    cases = json.loads((ROOT / f"{stem}_cases.json").read_text(encoding="utf-8"))
    identities = inventory(cases)
    expected_path = ROOT / f"{stem}_expected.jsonl"
    provenance_path = ROOT / f"{stem}_manifest.json"
    if not args.write:
        expected = expected_path.read_text(encoding="utf-8")
        if provenance(stem, entrypoint, cases, expected) != json.loads(provenance_path.read_text(encoding="utf-8")):
            raise RuntimeError("Reference input, output, or provenance changed")
    cache = args.cache_dir.resolve()
    manifest = prepare_jars(cache, args.offline)
    with tempfile.TemporaryDirectory(prefix=f"uqa-nori-{stem}-") as temporary:
        directory = Path(temporary)
        rows = ["\t".join([case["id"], *fields(case)]) for case in cases]
        (directory / "cases.tsv").write_text("\n".join(rows) + "\n", encoding="ascii")
        command = docker_command(manifest, cache, args.platform, args.offline, entrypoint, ["/output/cases.tsv"], output=directory, output_readonly=True)
        result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", check=True)
    output = canonical_output(result.stdout, manifest["runtime"], identities)
    if args.write:
        expected_path.write_text(output, encoding="utf-8")
        provenance_path.write_text(json.dumps(provenance(stem, entrypoint, cases, output), indent=2) + "\n", encoding="utf-8")
    elif output != expected:
        raise RuntimeError("".join(difflib.unified_diff(expected.splitlines(True), output.splitlines(True))))
    print(f"Verified {len(cases)} {stem} cases with Docker ({args.platform})")
