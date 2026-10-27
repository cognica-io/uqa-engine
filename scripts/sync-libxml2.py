#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Import or verify the pinned XML parser sources used by uqa-sql."""

from __future__ import annotations

import argparse
import hashlib
import io
from pathlib import Path
import tarfile
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
DESTINATION = ROOT / "crates/uqa-sql/vendor/libxml2"
VERSION = "2.15.4"
URL = f"https://download.gnome.org/sources/libxml2/2.15/libxml2-{VERSION}.tar.xz"
ARCHIVE_SHA256 = "98087fd181d9070724f3fbc65c7377db03038eb92bd882374daff44940138821"
SOURCES = {
    "buf.c", "chvalid.c", "dict.c", "encoding.c", "entities.c", "error.c",
    "globals.c", "hash.c", "list.c", "parser.c", "parserInternals.c", "SAX2.c",
    "threads.c", "tree.c", "uri.c", "valid.c", "xmlIO.c", "xmlmemory.c", "xmlstring.c",
    "CMakeLists.txt", "Copyright", "VERSION", "config.h.cmake.in", "libxml.h",
    "libxml2-config.cmake.cmake.in", "libxml-2.0.pc.in", "xml2-config.in",
    "win32/libxml2.rc", "codegen/charset.inc", "codegen/escape.inc", "codegen/ranges.inc",
}


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def check() -> None:
    expected = {}
    for line in (DESTINATION / "SHA256SUMS").read_text().splitlines():
        sha, relative = line.split("  ", 1)
        expected[relative] = sha
    actual = {
        path.relative_to(DESTINATION).as_posix(): digest(path.read_bytes())
        for path in DESTINATION.rglob("*")
        if path.is_file() and path.name not in {"SHA256SUMS", "UPSTREAM.md"}
    }
    if actual != expected:
        raise SystemExit("Pinned libxml2 source inventory or hashes differ")
    if (DESTINATION / "VERSION").read_text().strip() != VERSION:
        raise SystemExit("Pinned libxml2 version differs")
    print(f"Pinned libxml2 {VERSION}: {len(actual)} unmodified source files verified")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--archive", type=Path)
    arguments = parser.parse_args()
    if arguments.check:
        check()
        return
    data = arguments.archive.read_bytes() if arguments.archive else urllib.request.urlopen(URL).read()
    if digest(data) != ARCHIVE_SHA256:
        raise SystemExit("Pinned libxml2 archive hash differs")
    selected = {}
    prefix = f"libxml2-{VERSION}/"
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:xz") as archive:
        for member in archive.getmembers():
            if not member.isfile() or not member.name.startswith(prefix):
                continue
            relative = member.name[len(prefix):]
            if relative in SOURCES or (
                relative.startswith(("include/libxml/", "include/private/"))
                and (relative.endswith(".h") or relative == "include/libxml/xmlversion.h.in")
            ):
                selected[relative] = archive.extractfile(member).read()
    if not SOURCES <= selected.keys():
        raise SystemExit(f"Missing pinned source files: {SOURCES - selected.keys()}")
    for relative, contents in selected.items():
        path = DESTINATION / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(contents)
    (DESTINATION / "SHA256SUMS").write_text("".join(
        f"{digest(contents)}  {relative}\n" for relative, contents in sorted(selected.items())
    ))
    check()


if __name__ == "__main__":
    main()
