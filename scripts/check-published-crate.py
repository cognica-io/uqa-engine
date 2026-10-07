#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Check an exact immutable crates.io version without publishing a package."""

from __future__ import annotations

import json
import argparse
import pathlib
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


def published(crate: str, version: str) -> bool:
    name = urllib.parse.quote(crate, safe="")
    release = urllib.parse.quote(version, safe="")
    request = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{name}/{release}",
        headers={"User-Agent": "UQA release (https://github.com/cognica-io/uqa-engine)"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            body = json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise RuntimeError(f"Cannot verify {crate}@{version}: HTTP {error.code}") from error
    except (OSError, ValueError) as error:
        raise RuntimeError(f"Cannot verify {crate}@{version}: {error}") from error
    actual = body.get("version") if isinstance(body, dict) else None
    if not isinstance(actual, dict) or actual.get("crate") != crate or actual.get("num") != version:
        raise RuntimeError(f"Registry returned a different crate or version for {crate}@{version}")
    if actual.get("yanked") is not False:
        raise RuntimeError(f"Registry version {crate}@{version} is yanked or has no valid yank status")
    return True


def indexed(crate: str, version: str) -> bool:
    if not re.fullmatch(r"[A-Za-z0-9_-]+", crate):
        raise RuntimeError("invalid registry crate name")
    name = crate.lower()
    prefix = str(len(name)) if len(name) < 3 else "3/" + name[0] if len(name) == 3 else name[:2] + "/" + name[2:4]
    request = urllib.request.Request(
        f"https://index.crates.io/{prefix}/{name}",
        headers={"User-Agent": "UQA release (https://github.com/cognica-io/uqa-engine)", "Cache-Control": "no-cache"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            entries = [json.loads(line) for line in response if line.strip()]
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return False
        raise RuntimeError(f"Cannot verify index for {crate}@{version}: HTTP {error.code}") from error
    except (OSError, ValueError) as error:
        raise RuntimeError(f"Cannot verify index for {crate}@{version}: {error}") from error
    for entry in entries:
        if not isinstance(entry, dict) or entry.get("name") != crate:
            raise RuntimeError(f"Registry index returned another crate for {crate}@{version}")
        if entry.get("vers") == version:
            if entry.get("yanked") is not False:
                raise RuntimeError(f"Registry index version {crate}@{version} is yanked or invalid")
            return True
    return False


def wait_indexed(crate: str, version: str, timeout: float = 600) -> None:
    deadline = time.monotonic() + timeout
    while not indexed(crate, version):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError(f"Registry index did not expose {crate}@{version} within {timeout:g}s")
        print(f"Waiting for crates.io index visibility: {crate}@{version}", file=sys.stderr)
        time.sleep(min(10, remaining))


def missing_dependency(log: str, version: str) -> str | None:
    if "location searched: crates.io index" not in log:
        return None
    match = re.search(
        r'failed to select a version for the requirement `(uqa-[a-z0-9-]+) = "\^?'
        + re.escape(version) + r'"`', log,
    )
    return match[1] if match else None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("crate")
    parser.add_argument("version")
    parser.add_argument("--wait-index", action="store_true")
    parser.add_argument("--dependency-error-log", type=pathlib.Path)
    args = parser.parse_args()
    if args.dependency_error_log:
        dependency = missing_dependency(args.dependency_error_log.read_text(), args.version)
        if dependency is None or dependency == args.crate or not published(dependency, args.version):
            return 1
        wait_indexed(dependency, args.version)
        return 0
    if args.wait_index:
        wait_indexed(args.crate, args.version)
        return 0
    return 0 if published(args.crate, args.version) else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        print(error, file=sys.stderr)
        raise SystemExit(2) from error
