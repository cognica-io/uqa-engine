#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Pinned artifact validation and Docker-only JVM execution for Lucene reference tools."""

import hashlib
import json
from urllib.request import urlopen


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def prepare_jars(source_root, cache, offline):
    manifest = json.loads((source_root / "manifest.json").read_text(encoding="utf-8"))
    cache.mkdir(parents=True, exist_ok=True)
    for artifact in manifest["jars"]:
        target = cache / (artifact["artifact"] + "-" + manifest["lucene_version"] + ".jar")
        if not target.exists():
            if offline:
                raise RuntimeError(f"Missing cached artifact: {target}")
            with urlopen(artifact["url"], timeout=60) as source:
                data = source.read()
            if len(data) != artifact["bytes"] or hashlib.sha256(data).hexdigest() != artifact["sha256"]:
                raise RuntimeError(f"Downloaded artifact checksum mismatch: {target.name}")
            target.write_bytes(data)
        if target.stat().st_size != artifact["bytes"] or sha256(target) != artifact["sha256"]:
            raise RuntimeError(f"Cached artifact checksum mismatch: {target}")
    return manifest


def docker_command(source_root, manifest, cache, platform, offline, entrypoint, arguments=(), output=None, output_readonly=False, input_directory=None):
    # An explicit classpath excludes unrelated jars in a reused cache directory.
    classpath = ":".join(
        "/jars/" + item["artifact"] + "-" + manifest["lucene_version"] + ".jar"
        for item in manifest["jars"]
    )
    command = [
        "docker", "run", "--rm", "--platform", platform,
        "--pull", "never" if offline else "missing",
        "--network", "none", "--read-only", "--tmpfs", "/tmp:rw,nosuid,nodev,size=256m",
        "--mount", f"type=bind,source={cache},target=/jars,readonly",
        "--mount", f"type=bind,source={source_root},target=/src,readonly",
    ]
    if output is not None:
        command += ["--mount", f"type=bind,source={output},target=/output" + (",readonly" if output_readonly else "")]
    if input_directory is not None:
        command += ["--mount", f"type=bind,source={input_directory},target=/input,readonly"]
    return command + [manifest["docker_image"], "java", "-Xmx1g", "--class-path", classpath, "/src/" + entrypoint, *arguments]
