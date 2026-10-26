#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Generate native link controls and installed C binding metadata from public headers."""

from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import re
import shlex
import platform
import shutil

from c_bindings_licenses import collect

ROOT = Path(__file__).resolve().parents[1]
HEADERS = ROOT / "crates/uqa-ffi/include/uqa"
FEATURES = {"arrow", "arrow-export", "duckdb", "kuromoji", "nori"}
DECLARATION = re.compile(r"UQA_API\s+uqa_status\s+UQA_CALL\s+(uqa_\w+)\s*\(")


def symbols() -> tuple[list[str], list[str]]:
    exported, deferred = [], []
    for header in sorted(HEADERS.glob("*.h")):
        names = DECLARATION.findall(header.read_text())
        (deferred if header.name == "uqa_log.h" else exported).extend(names)
    if not exported or len(set(exported + deferred)) != len(exported + deferred):
        raise ValueError("empty or duplicate C function declaration inventory")
    return sorted(exported), sorted(deferred)


def legal_payloads(features: set[str]) -> dict[str, bytes]:
    spec = importlib.util.spec_from_file_location("c_release_licenses", ROOT / "scripts/check-release-licenses.py")
    licenses = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(licenses)
    files = licenses.canonical_payloads()
    # This inventory includes shared Lucene, regex, SQL sort and XML notices as
    # well as dictionary licenses. Unused feature notices are harmless; the build
    # manifest, rather than license-file presence, identifies enabled features.
    files.update(licenses.binding_dictionary_payloads())
    files["LICENSE-NOTICE.md"] = (ROOT / "crates/uqa-node/LICENSE-NOTICE.md").read_bytes()
    # Arrow's standard C declarations ship even when runtime export is disabled.
    for path in (ROOT / "scripts/usql-release/licenses").glob("ARROW-*.txt"):
        files[f"THIRD-PARTY/{path.name}"] = path.read_bytes()
    if "duckdb" in features:
        files["THIRD-PARTY/DUCKDB-LICENSE.txt"] = (ROOT / "scripts/usql-release/licenses/DUCKDB-LICENSE.txt").read_bytes()
    return files


def native_link_inputs(flags: str, target: str) -> tuple[list[str], list[str]]:
    parts = iter(shlex.split(flags))
    libraries, options = [], []
    for flag in parts:
        if flag == "-framework":
            flag += " " + next(parts)
        if any(character in flag for character in (';', '"', '\n', '\r', '$', '\\')):
            raise ValueError(f"unsupported native linker token: {flag!r}")
        # MSVC's /defaultlib:... is a linker directive. CMake interprets a
        # leading slash in target_link_libraries as an absolute file path.
        if "windows-msvc" in target and flag.startswith(("/", "-")):
            options.append("LINKER:" + flag)
        else:
            libraries.append(flag)
    return libraries, options


def msvc_pkg_config_libraries(flags: str) -> str:
    # pkgconf's MSVC renderer translates -l names but omits raw .lib paths
    # and native options. Retain every Rust system-library dependency.
    libraries = []
    for flag in shlex.split(flags):
        if flag.lower().startswith(("/defaultlib:", "-defaultlib:")):
            library = flag.split(":", 1)[1]
        elif flag.lower().endswith(".lib"):
            library = flag
        else:
            raise ValueError(f"unsupported pkg-config MSVC linker token: {flag!r}")
        if library.lower().endswith(".lib"):
            library = library[:-4]
        if not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]*", library):
            raise ValueError(f"unsupported pkg-config MSVC library: {library!r}")
        libraries.append("-l" + library)
    return " ".join(libraries)


def generate(output: Path, target: str, revision: str, features: set[str], flags: str,
             cargo_metadata: Path, minimum_os: str = "") -> None:
    if features - FEATURES:
        raise ValueError(f"unknown C binding features: {sorted(features - FEATURES)}")
    if not re.fullmatch(r"[a-zA-Z0-9_-]+", target):
        raise ValueError("invalid target triple")
    if revision != "unknown" and not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("revision must be a complete Git object ID or unknown for a local build")
    versions = re.findall(r'^version = "([^"\n]+)"$', (ROOT / "Cargo.toml").read_text(), re.M)
    if len(versions) != 1:
        raise ValueError("Expected one workspace package version")
    version = versions[0]
    public, deferred = symbols()
    libraries, options = native_link_inputs(flags, target)
    pkg_libraries = msvc_pkg_config_libraries(flags) if "windows-msvc" in target else ""
    dependency_licenses, inventory = collect(cargo_metadata, features)
    payloads = legal_payloads(features) | dependency_licenses
    output.mkdir(parents=True, exist_ok=True)
    (output / "exports.macho").write_text("".join(f"_{name}\n" for name in public))
    (output / "exports.elf").write_text("{\n  global:\n" + "".join(f"    {name};\n" for name in public) + "  local: *;\n};\n")
    (output / "exports.def").write_text("LIBRARY uqa\nEXPORTS\n" + "".join(f"  {name}\n" for name in public))
    (output / "build.cmake").write_text(
        f'set(UQA_VERSION "{version}")\n'
        f'set(UQA_NATIVE_LIBRARIES "{";".join(libraries)}")\n'
        f'set(UQA_NATIVE_LINK_OPTIONS "{";".join(options)}")\n'
        f'set(UQA_PKG_CONFIG_NATIVE_LIBRARIES "{pkg_libraries}")\n')
    share = output / "share/uqa"
    # This directory is entirely generated. A successful feature/dependency
    # reconfiguration must not retain obsolete notices from the previous graph.
    if share.exists():
        shutil.rmtree(share)
    manifest = {
        "schema_version": 1, "abi_major": 1, "version": version, "target": target,
        "source_revision": revision, "features": sorted(features),
        "exports": public, "declaration_only": {"uqa_log.h": deferred},
        "native_static_link_flags": shlex.split(flags),
        "runtime": {"macos_deployment_target": minimum_os if "apple" in target else None,
                    "libc": list(platform.libc_ver()) if "linux" in target else None,
                    "msvc_crt": "dynamic (/MD)" if "windows" in target else None},
    }
    manifest["dependencies"] = inventory
    for name, data in payloads.items():
        path = share / "licenses" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    (share / "uqa-build.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(f"C ABI metadata: {len(public)} exports, {len(deferred)} declaration-only logging functions")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--features", default="")
    parser.add_argument("--native-link-flags", required=True)
    parser.add_argument("--cargo-metadata", required=True, type=Path)
    parser.add_argument("--minimum-os", default="")
    args = parser.parse_args()
    generate(args.output, args.target, args.revision,
             set(filter(None, args.features.split(","))), args.native_link_flags, args.cargo_metadata, args.minimum_os)


if __name__ == "__main__":
    main()
