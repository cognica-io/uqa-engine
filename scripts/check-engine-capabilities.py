#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Enforce ownership allowlists for migrated Engine capability boundaries."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parents[1]
POLICY_PATH = ROOT / "scripts" / "engine-capability-policy.json"
ENGINE_REFERENCE = re.compile(r"\bEngine\b|\b(?:self|[A-Za-z_][A-Za-z0-9_]*)\.engine\b")
RUST_IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
DATA_TYPE_DECLARATION = re.compile(
    r"\b(?P<kind>struct|enum|union)\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)"
)
TRAIT_DECLARATION = re.compile(r"\btrait\s+[A-Za-z_][A-Za-z0-9_]*")
TYPE_ALIAS_DECLARATION = re.compile(r"\btype\s+[A-Za-z_][A-Za-z0-9_]*[^;]*;")
IMPORT_ALIAS_DECLARATION = re.compile(r"\buse\b[^;]*\bas\b[^;]*;", re.DOTALL)
FUNCTION_DECLARATION = re.compile(r"\bfn\s+[A-Za-z_][A-Za-z0-9_]*")
CAPABILITY_ESCAPE_HATCHES = (
    (
        re.compile(r"\b(?:std::ops::)?Deref(?:Mut)?\b"),
        "capability module must not implement or import Deref",
    ),
    (
        re.compile(r"\b(?:EngineServices|EngineContext|EngineCapabilitySet)\b"),
        "capability module must not define a catch-all engine service type",
    ),
    (
        re.compile(r"(?m)\bfn\s+(?:engine|as_engine|into_engine)\s*\("),
        "capability module must not expose an engine recovery method",
    ),
)


class PolicyError(RuntimeError):
    """The capability policy or current source tree is invalid."""


def normalized_path(value: object, field: str) -> str:
    if not isinstance(value, str) or not value:
        raise PolicyError(f"{field} must be a non-empty relative path")
    path = pathlib.PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or path.as_posix() != value:
        raise PolicyError(f"{field} must be a normalized relative path: {value!r}")
    return value


def sorted_unique_paths(value: object, field: str) -> list[str]:
    if not isinstance(value, list):
        raise PolicyError(f"{field} must be a list")
    paths = [normalized_path(path, f"{field} entry") for path in value]
    if paths != sorted(paths):
        raise PolicyError(f"{field} must be sorted")
    if len(paths) != len(set(paths)):
        raise PolicyError(f"{field} contains duplicates")
    return paths


def sorted_unique_identifiers(value: object, field: str) -> list[str]:
    if not isinstance(value, list):
        raise PolicyError(f"{field} must be a list")
    identifiers: list[str] = []
    for entry in value:
        if not isinstance(entry, str) or RUST_IDENTIFIER.fullmatch(entry) is None:
            raise PolicyError(f"{field} entries must be Rust identifiers")
        identifiers.append(entry)
    if identifiers != sorted(identifiers):
        raise PolicyError(f"{field} must be sorted")
    if len(identifiers) != len(set(identifiers)):
        raise PolicyError(f"{field} contains duplicates")
    return identifiers


def mask_rust_non_code(source: str) -> str:
    """Mask comments and string literals while preserving source offsets."""
    masked = list(source)

    def blank(start: int, end: int) -> None:
        for position in range(start, end):
            if masked[position] != "\n":
                masked[position] = " "

    position = 0
    while position < len(source):
        if source.startswith("//", position):
            end = source.find("\n", position + 2)
            if end == -1:
                end = len(source)
            blank(position, end)
            position = end
            continue
        if source.startswith("/*", position):
            depth = 1
            end = position + 2
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            blank(position, end)
            position = end
            continue
        raw = re.match(r"(?:br|cr|r)(?P<hashes>#{0,255})\"", source[position:])
        if raw is not None:
            terminator = '"' + raw.group("hashes")
            content_start = position + raw.end()
            content_end = source.find(terminator, content_start)
            end = len(source) if content_end == -1 else content_end + len(terminator)
            blank(position, end)
            position = end
            continue
        prefix = 2 if source.startswith(('b"', 'c"'), position) else 1
        if source[position] == '"' or prefix == 2:
            end = position + prefix
            escaped = False
            while end < len(source):
                character = source[end]
                end += 1
                if character == '"' and not escaped:
                    break
                if character == "\n" and not escaped:
                    break
                escaped = character == "\\" and not escaped
                if character != "\\":
                    escaped = False
            blank(position, end)
            position = end
            continue
        position += 1
    return "".join(masked)


def matching_brace(source: str, opening: int) -> int:
    depth = 0
    for position in range(opening, len(source)):
        if source[position] == "{":
            depth += 1
        elif source[position] == "}":
            depth -= 1
            if depth == 0:
                return position + 1
    return len(source)


def data_type_declarations(source: str) -> list[tuple[str, str, str]]:
    declarations: list[tuple[str, str, str]] = []
    for match in DATA_TYPE_DECLARATION.finditer(source):
        brace = source.find("{", match.end())
        semicolon = source.find(";", match.end())
        if semicolon != -1 and (brace == -1 or semicolon < brace):
            end = semicolon + 1
        elif brace != -1:
            end = matching_brace(source, brace)
        else:
            end = len(source)
        declarations.append((match.group("kind"), match.group("name"), source[match.start() : end]))
    return declarations


def function_signatures(source: str) -> list[str]:
    signatures: list[str] = []
    for match in FUNCTION_DECLARATION.finditer(source):
        parentheses = 0
        brackets = 0
        position = match.end()
        while position < len(source):
            character = source[position]
            if character == "(":
                parentheses += 1
            elif character == ")":
                parentheses = max(0, parentheses - 1)
            elif character == "[":
                brackets += 1
            elif character == "]":
                brackets = max(0, brackets - 1)
            elif character in "{;" and parentheses == 0 and brackets == 0:
                signatures.append(source[match.start() : position])
                break
            position += 1
    return signatures


def load_policy(path: pathlib.Path) -> dict[str, Any]:
    try:
        policy = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PolicyError(f"cannot read capability policy {path}: {error}") from error
    if not isinstance(policy, dict) or set(policy) != {
        "schema_version",
        "capability_module",
        "declared_types",
        "scopes",
    }:
        raise PolicyError("capability policy has unexpected top-level fields")
    if policy["schema_version"] != 2:
        raise PolicyError("capability policy must use schema_version 2")
    policy["capability_module"] = normalized_path(
        policy["capability_module"], "capability_module"
    )
    policy["declared_types"] = sorted_unique_identifiers(
        policy["declared_types"], "declared_types"
    )
    scopes = policy["scopes"]
    if not isinstance(scopes, list) or not scopes:
        raise PolicyError("scopes must be a non-empty list")
    names: set[str] = set()
    covered: set[str] = set()
    for index, scope in enumerate(scopes):
        field = f"scopes[{index}]"
        if not isinstance(scope, dict) or set(scope) != {
            "name",
            "files",
            "engine_allowlist",
        }:
            raise PolicyError(f"{field} has unexpected fields")
        name = scope["name"]
        if not isinstance(name, str) or not name.strip():
            raise PolicyError(f"{field}.name must be non-empty")
        if name in names:
            raise PolicyError(f"duplicate scope name: {name}")
        names.add(name)
        files = sorted_unique_paths(scope["files"], f"{field}.files")
        if not files:
            raise PolicyError(f"{field}.files must not be empty")
        overlap = covered.intersection(files)
        if overlap:
            raise PolicyError(f"files occur in multiple scopes: {sorted(overlap)}")
        covered.update(files)
        allowlist = sorted_unique_paths(
            scope["engine_allowlist"], f"{field}.engine_allowlist"
        )
        unexpected = set(allowlist).difference(files)
        if unexpected:
            raise PolicyError(
                f"{field}.engine_allowlist contains files outside the scope: {sorted(unexpected)}"
            )
        scope["files"] = files
        scope["engine_allowlist"] = allowlist
    if policy["capability_module"] not in covered:
        raise PolicyError("capability_module must belong to a declared scope")
    return policy


def verify(root: pathlib.Path, policy_path: pathlib.Path) -> dict[str, int]:
    policy = load_policy(policy_path)
    errors: list[str] = []
    checked = 0
    allowed = 0
    for scope in policy["scopes"]:
        allowlist = set(scope["engine_allowlist"])
        for relative in scope["files"]:
            path = root / relative
            if not path.is_file():
                errors.append(f"declared capability-policy file is missing: {relative}")
                continue
            checked += 1
            source = path.read_text(encoding="utf-8")
            code = mask_rust_non_code(source)
            if ENGINE_REFERENCE.search(code):
                if relative not in allowlist:
                    errors.append(
                        f"{relative}: Engine reference is not allowed in {scope['name']}"
                    )
                else:
                    allowed += 1
            elif relative in allowlist:
                errors.append(
                    f"{relative}: stale Engine allowlist entry in {scope['name']}"
                )
    capability_path = root / policy["capability_module"]
    if capability_path.is_file():
        source = capability_path.read_text(encoding="utf-8")
        code = mask_rust_non_code(source)
        declarations = data_type_declarations(code)
        declared_names = [name for _, name, _ in declarations]
        duplicate_names = sorted(
            name for name in set(declared_names) if declared_names.count(name) > 1
        )
        if duplicate_names:
            errors.append(
                f"{policy['capability_module']}: duplicate data type declarations: "
                f"{duplicate_names}"
            )
        if sorted(set(declared_names)) != policy["declared_types"]:
            errors.append(
                f"{policy['capability_module']}: declared data types must match policy; "
                f"expected {policy['declared_types']}, found {sorted(set(declared_names))}"
            )
        if TRAIT_DECLARATION.search(code):
            errors.append(
                f"{policy['capability_module']}: capability module must not define service traits"
            )
        if IMPORT_ALIAS_DECLARATION.search(code):
            errors.append(
                f"{policy['capability_module']}: capability module must not rename imports"
            )
        for _, name, declaration in declarations:
            if ENGINE_REFERENCE.search(declaration):
                errors.append(
                    f"{policy['capability_module']}: data type {name} must not retain "
                    "an Engine value or reference"
                )
        for alias in TYPE_ALIAS_DECLARATION.finditer(code):
            if ENGINE_REFERENCE.search(alias.group(0)):
                errors.append(
                    f"{policy['capability_module']}: capability module must not alias Engine"
                )
        for signature in function_signatures(code):
            if ENGINE_REFERENCE.search(signature):
                errors.append(
                    f"{policy['capability_module']}: capability function signatures "
                    "must not accept or return Engine"
                )
        for pattern, message in CAPABILITY_ESCAPE_HATCHES:
            if pattern.search(code):
                errors.append(f"{policy['capability_module']}: {message}")
    if errors:
        raise PolicyError("\n".join(errors))
    return {
        "checked_files": checked,
        "declared_engine_adapters": allowed,
        "engine_free_leaf_files": checked - allowed,
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, default=ROOT)
    parser.add_argument("--policy", type=pathlib.Path, default=POLICY_PATH)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        result = verify(args.root.resolve(), args.policy.resolve())
    except PolicyError as error:
        print(error, file=sys.stderr)
        return 1
    print(
        "Engine capability policy: "
        f"{result['checked_files']} files, "
        f"{result['declared_engine_adapters']} declared adapters, "
        f"{result['engine_free_leaf_files']} engine-free leaves"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
