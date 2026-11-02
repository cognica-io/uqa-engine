#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Recover native C link inputs from the selected Cargo build's public JSON output."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import re
import shlex

from c_bindings_licenses import selected_packages


def cargo_messages(output: bytes):
    for line in output.splitlines():
        if not line.startswith(b"{"):
            continue
        try:
            message = json.loads(line)
        except (ValueError, UnicodeDecodeError):
            continue
        if isinstance(message, dict):
            yield message


def native_static_flags(output: bytes) -> str:
    flags = []
    for message in cargo_messages(output):
        if message.get("reason") == "compiler-message":
            diagnostic = message["message"]["message"]
            if diagnostic.startswith("native-static-libs:"):
                flags.append(diagnostic.split(":", 1)[1].strip())
    if not flags:
        # Rustc also emits this note as plain text in rendered-diagnostic mode.
        flags = [match.group(1).decode("utf-8").strip() for line in output.splitlines()
                 if not line.startswith(b"{")
                 for match in [re.search(rb"native-static-libs:\s*([^\r\n]+)", line)] if match]
    if not flags or not flags[-1]:
        raise ValueError("rustc did not report native static-library dependencies")
    return flags[-1]


def bundled_msvc_libraries(output: bytes, metadata: dict, features: set[str], flags: str) -> dict[str, Path]:
    scripts = [message for message in cargo_messages(output)
               if message.get("reason") == "build-script-executed"]
    if not scripts:
        return {}
    selected = selected_packages(metadata, features)
    directories = set()
    for script in scripts:
        if script["package_id"] not in selected:
            continue
        for entry in script["linked_paths"]:
            kind, separator, path = entry.partition("=")
            if separator and kind not in {"native", "all"}:
                continue
            directories.add(Path(path if separator else entry).resolve())
    result = {}
    for library in shlex.split(flags):
        # Compiler runtime directives still resolve through the MSVC SDK. Cargo
        # package libraries are bare .lib tokens in native-static-libs output.
        if not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]*\.lib", library, re.I):
            continue
        candidates = sorted({(directory / library).resolve() for directory in directories
                             if (directory / library).is_file()})
        if not candidates:
            continue  # System libraries retain their ordinary SDK resolution.
        digests = {hashlib.sha256(path.read_bytes()).digest() for path in candidates}
        if len(digests) != 1:
            raise ValueError(f"conflicting Cargo native libraries for {library}")
        result[library] = candidates[0]
    return result


def cmake_path(path: Path) -> str:
    value = path.as_posix()
    if any(character in value for character in (';', '"', '\n', '\r', '$')):
        raise ValueError(f"unsupported CMake native library path: {value!r}")
    return value
