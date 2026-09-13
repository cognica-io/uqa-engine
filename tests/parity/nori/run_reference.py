#!/usr/bin/env python3
"""Reproduce the Nori design examples with a digest-pinned Docker JVM."""

import argparse
import difflib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from reference_runtime import ROOT, docker_command, prepare_jars


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-dir", type=Path, default=Path(tempfile.gettempdir()) / "uqa-nori-reference-jars")
    parser.add_argument("--offline", action="store_true", help="Require cached jars and Docker image")
    parser.add_argument("--write", action="store_true", help="Replace expected output after inspecting an intentional reference change")
    parser.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], default="linux/arm64")
    args = parser.parse_args()
    cache = args.cache_dir.resolve()
    manifest = prepare_jars(cache, args.offline)
    command = docker_command(manifest, cache, args.platform, args.offline, "NoriReference.java")
    result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", check=True)
    actual = [json.loads(line) for line in result.stdout.splitlines()]
    if actual[0] != {"runtime": manifest["runtime"]}:
        raise RuntimeError(f"Unexpected JVM runtime: {actual[0]}")
    fixtures = actual[1:]
    ids = [fixture["id"] for fixture in fixtures]
    if len(ids) != manifest["fixture_count"] or len(set(ids)) != len(ids):
        raise RuntimeError("Reference fixture count or unique identities changed")
    output = "".join(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n" for row in actual)
    expected_path = ROOT / "expected.jsonl"
    if args.write:
        expected_path.write_text(output, encoding="utf-8")
        print(f"Wrote {len(fixtures)} Lucene reference cases to {expected_path}")
        return 0
    expected = expected_path.read_text(encoding="utf-8")
    if output != expected:
        sys.stderr.writelines(difflib.unified_diff(
            expected.splitlines(keepends=True), output.splitlines(keepends=True),
            fromfile="expected.jsonl", tofile="Docker Lucene output",
        ))
        return 1
    print(f"Verified {len(fixtures)} Lucene {manifest['lucene_version']} reference cases using Docker ({args.platform})")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as error:
        sys.stderr.write(error.stderr or str(error))
        sys.exit(1)
    except (OSError, RuntimeError, ValueError) as error:
        sys.stderr.write(f"Nori reference failed: {error}\n")
        sys.exit(1)
