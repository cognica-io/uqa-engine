#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify bounded CJK width fixtures and exhaustive text/offset hashes using a Docker JVM."""

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent))
try:
    from lucene_runtime import docker_command, prepare_jars, sha256
finally:
    sys.path.pop(0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-dir", type=Path, default=Path(tempfile.gettempdir()) / "uqa-lucene-reference-jars")
    parser.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], default="linux/arm64")
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--write-expected", action="store_true", help="Record reviewed reference changes")
    args = parser.parse_args()
    cache = args.cache_dir.resolve()
    manifest = prepare_jars(ROOT, cache, args.offline)
    command = docker_command(ROOT, manifest, cache, args.platform, args.offline, "CJKWidthReference.java")
    run = subprocess.run(command, capture_output=True, text=True, check=True, timeout=120)
    result = json.loads(run.stdout)
    if result["runtime"] != manifest["runtime"]:
        raise RuntimeError("CJK width reference JVM runtime differs")
    if result["scalar_count"] != 1_112_064 or result["combination_count"] != 1_672 or len(result["examples"]) != 19:
        raise RuntimeError("CJK width reference corpus is incomplete")
    result["source_sha256"] = sha256(ROOT / "CJKWidthReference.java")
    expected_path = ROOT / "expected.json"
    if args.write_expected:
        expected_path.write_text(json.dumps(result, ensure_ascii=False, separators=(",", ":")) + "\n", encoding="utf-8")
    elif result != json.loads(expected_path.read_text(encoding="utf-8")):
        raise RuntimeError("CJK width reference differs from reviewed text, offsets, or source")
    print("CJK width reference verified: all 1,112,064 scalars, 1,672 voiced-mark combinations, 19 examples")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        sys.stderr.write(error.stderr or str(error))
        sys.exit(1)
    except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
        sys.stderr.write(f"CJK width reference failed: {error}\n")
        sys.exit(1)
