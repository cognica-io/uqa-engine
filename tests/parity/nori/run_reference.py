#!/usr/bin/env python3
"""Reproduce the Nori design examples with a digest-pinned Docker JVM."""

import argparse
import difflib
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from urllib.request import urlopen


ROOT = Path(__file__).resolve().parent


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache-dir", type=Path, default=Path(tempfile.gettempdir()) / "uqa-nori-reference-jars")
    parser.add_argument("--offline", action="store_true", help="Require cached jars and Docker image")
    parser.add_argument("--write", action="store_true", help="Replace expected output after inspecting an intentional reference change")
    parser.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], default="linux/arm64")
    args = parser.parse_args()
    manifest = json.loads((ROOT / "manifest.json").read_text())
    cache = args.cache_dir.resolve()
    cache.mkdir(parents=True, exist_ok=True)
    for artifact in manifest["jars"]:
        target = cache / (artifact["artifact"] + "-" + manifest["lucene_version"] + ".jar")
        if not target.exists():
            if args.offline:
                raise RuntimeError(f"Missing cached artifact: {target}")
            with urlopen(artifact["url"], timeout=60) as source:
                data = source.read()
            if hashlib.sha256(data).hexdigest() != artifact["sha256"]:
                raise RuntimeError(f"Downloaded artifact checksum mismatch: {target.name}")
            target.write_bytes(data)
        if sha256(target) != artifact["sha256"]:
            raise RuntimeError(f"Cached artifact checksum mismatch: {target}")

    # An explicit classpath excludes unrelated jars in a reused cache directory.
    classpath = ":".join(
        "/jars/" + artifact["artifact"] + "-" + manifest["lucene_version"] + ".jar"
        for artifact in manifest["jars"]
    )
    command = [
        "docker", "run", "--rm", "--platform", args.platform,
        "--pull", "never" if args.offline else "missing",
        "--network", "none", "--read-only", "--tmpfs", "/tmp:rw,nosuid,nodev,size=256m",
        "--mount", f"type=bind,source={cache},target=/jars,readonly",
        "--mount", f"type=bind,source={ROOT},target=/src,readonly",
        manifest["docker_image"], "java", "-Xmx512m", "--class-path", classpath,
        "/src/NoriReference.java",
    ]
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
