#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Check an exact immutable crates.io version without publishing a package."""

from __future__ import annotations

import json
import sys
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


if __name__ == "__main__":
    try:
        if len(sys.argv) != 3:
            raise RuntimeError("usage: check-published-crate.py CRATE VERSION")
        raise SystemExit(0 if published(sys.argv[1], sys.argv[2]) else 1)
    except RuntimeError as error:
        print(error, file=sys.stderr)
        raise SystemExit(2) from error
