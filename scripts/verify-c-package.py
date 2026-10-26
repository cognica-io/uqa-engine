#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify installed headers, exact exports and real CMake/pkg-config C/C++ consumers."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
PROVIDERS = ("memory", "sqlite_native", "sqlite_kv", "redb")
CAPABILITIES = {"nori": 1, "kuromoji": 2, "arrow": 4, "duckdb": 8, "arrow-export": 16}
DECLARATION = re.compile(r"UQA_API\s+uqa_status\s+UQA_CALL\s+(uqa_\w+)\s*\(")


def compiler_flags(text: str) -> list[str]:
    if sys.platform != "win32":
        return shlex.split(text)
    # MSVC syntax uses Windows quoting. POSIX shlex would consume backslashes
    # from native drive paths or incorrectly preserve quoted argument fragments.
    import ctypes
    split = ctypes.windll.shell32.CommandLineToArgvW
    split.argtypes = [ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_int)]
    split.restype = ctypes.POINTER(ctypes.c_wchar_p)
    free = ctypes.windll.kernel32.LocalFree
    free.argtypes = [ctypes.c_void_p]
    free.restype = ctypes.c_void_p
    count = ctypes.c_int()
    arguments = split("pkgconf " + text, ctypes.byref(count))
    if not arguments:
        raise ctypes.WinError()
    try:
        return [arguments[index] for index in range(1, count.value)]
    finally:
        free(arguments)


def library(prefix: Path) -> Path:
    if sys.platform == "win32":
        return prefix / "bin/uqa.dll"
    return prefix / ("lib/libuqa.dylib" if sys.platform == "darwin" else "lib/libuqa.so")


def verify_symbols(prefix: Path, manifest: dict) -> None:
    artifact = library(prefix)
    if sys.platform == "win32":
        output = subprocess.check_output(["dumpbin", "/NOLOGO", "/EXPORTS", str(artifact)], text=True)
        actual = set(re.findall(r"^\s*\d+\s+[0-9A-Fa-f]+\s+[0-9A-Fa-f]+\s+(\S+)", output, re.M))
    else:
        command = ["nm", "-gU"] if sys.platform == "darwin" else ["nm", "-D", "--defined-only"]
        output = subprocess.check_output([*command, str(artifact)], text=True)
        actual = {line.split()[-1] for line in output.splitlines() if line.split()}
        if sys.platform == "darwin":
            actual = {name.removeprefix("_") for name in actual}
    expected = set(manifest["exports"])
    if actual != expected:
        raise ValueError(f"Export mismatch: missing={sorted(expected-actual)}, extra={sorted(actual-expected)}")
    declared, deferred = set(), set()
    for header in (prefix / "include/uqa").glob("*.h"):
        (deferred if header.name == "uqa_log.h" else declared).update(DECLARATION.findall(header.read_text()))
    if declared != expected or deferred != set(manifest["declaration_only"]["uqa_log.h"]):
        raise ValueError("Installed declarations and build inventory disagree")
    for package in manifest["dependencies"]:
        for item in package["files"]:
            data = (prefix / "share/uqa/licenses" / item["path"]).read_bytes()
            if hashlib.sha256(data).hexdigest() != item["sha256"]:
                raise ValueError(f"Changed dependency license: {item['path']}")
    print(f"Verified {len(actual)} exact native exports and dependency license digests", flush=True)


def identity_source(manifest: dict, shared: bool) -> str:
    mode = "ifndef" if shared else "ifdef"
    capabilities = sum(CAPABILITIES[feature] for feature in manifest["features"])
    return f'''#include <uqa/uqa.h>
#include <assert.h>
#include <string.h>
#{mode} UQA_USE_SHARED
#error incorrect imported-target linkage definitions
#endif
#ifdef UQA_BUILD_SHARED
#error producer definitions leaked into a consumer
#endif
int main(void) {{
    uint32_t abi = 0;
    uint64_t capabilities = 0;
    uqa_string_view version = {{NULL, 0}}, revision = {{NULL, 0}};
    assert(uqa_library_get_abi_version(&abi) == UQA_OK && abi == {manifest['abi_major']});
    assert(uqa_library_get_capabilities(&capabilities) == UQA_OK && capabilities == {capabilities});
    assert(uqa_library_get_version(&version) == UQA_OK);
    assert(version.size == strlen("{manifest['version']}"));
    assert(memcmp(version.data, "{manifest['version']}", version.size) == 0);
    assert(uqa_library_get_source_revision(&revision) == UQA_OK);
    assert(revision.size == strlen("{manifest['source_revision']}"));
    assert(memcmp(revision.data, "{manifest['source_revision']}", revision.size) == 0);
    return 0;
}}
'''


def generate_consumers(source: Path, prefix: Path, manifest: dict) -> list[tuple[str, str, str]]:
    fixtures = sorted((ROOT / "crates/uqa-ffi/tests/native").glob("*.c"))
    examples = sorted((ROOT / "examples/c").glob("*.c"))
    interop = [ROOT / "crates/uqa-ffi/tests/interop/xml_host.c"]
    cmake = ['cmake_minimum_required(VERSION 3.24)', 'project(UQAConsumers LANGUAGES C CXX)',
             'find_package(UQA CONFIG REQUIRED)', 'find_package(Threads REQUIRED)']
    # An unmodified host copy must coexist with the packaged parser in both linkage modes.
    xml_source = ROOT / "crates/uqa-sql/vendor/libxml2"
    cmake += ['set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)']
    for feature in re.findall(r"option\(LIBXML2_WITH_(\w+)", (xml_source / "CMakeLists.txt").read_text()):
        enabled = "ON" if feature in {"THREADS", "VALID"} else "OFF"
        cmake.append(f'set(LIBXML2_WITH_{feature} {enabled} CACHE BOOL "" FORCE)')
    cmake += [f'add_subdirectory("{xml_source.as_posix()}" host-xml EXCLUDE_FROM_ALL)',
             'set(CMAKE_C_STANDARD 11)', 'set(CMAKE_C_STANDARD_REQUIRED ON)',
             'set(CMAKE_C_EXTENSIONS OFF)', 'set(CMAKE_CXX_STANDARD 17)',
             'set(CMAKE_CXX_STANDARD_REQUIRED ON)', 'set(CMAKE_CXX_EXTENSIONS OFF)',
             'if(MSVC)', '  add_compile_options(/W4 /WX /UNDEBUG /D_CRT_SECURE_NO_WARNINGS)',
             '  set(CMAKE_MSVC_RUNTIME_LIBRARY MultiThreadedDLL)', 'else()',
             '  add_compile_options(-Wall -Wextra -Werror -pedantic -UNDEBUG)', 'endif()']
    records = []
    for language, suffix in (("c11", "c"), ("cxx17", "cpp")):
        for shared in (True, False):
            linkage = "shared" if shared else "static"
            target = "UQA::uqa" if shared else "UQA::uqa_static"
            identity = f"identity_{language}_{linkage}"
            (source / f"{identity}.{suffix}").write_text(identity_source(manifest, shared))
            cmake += [f'add_executable({identity} {identity}.{suffix})',
                      f'target_link_libraries({identity} PRIVATE {target})']
            records.append((identity, "identity", ""))
            for category, paths in (("fixture", fixtures), ("example", examples), ("interop", interop)):
                for path in paths:
                    name = f"{category}_{path.stem}_{language}_{linkage}"
                    (source / f"{name}.{suffix}").write_text(f'#include "{path.as_posix()}"\n')
                    cmake += [f'add_executable({name} {name}.{suffix})',
                              f'target_link_libraries({name} PRIVATE {target} Threads::Threads)',
                              'if(UNIX)', f'  target_link_libraries({name} PRIVATE m)', 'endif()']
                    if category == "interop":
                        cmake.append(f'target_link_libraries({name} PRIVATE LibXml2::LibXml2)')
                    records.append((name, category, path.stem))
        headers = sorted((prefix / "include/uqa").glob("*.h"))
        for mode, definition in (("producer", "UQA_BUILD_SHARED"), ("consumer", "UQA_USE_SHARED"), ("static", "")):
            for index, selected in enumerate([[header] for header in headers] + [headers]):
                name = f"header_{index}_{language}_{mode}"
                (source / f"{name}.{suffix}").write_text(
                    "".join(f'#include <uqa/{header.name}>\n' for header in selected) + "int main(void) { return 0; }\n")
                cmake += [f'add_library({name} OBJECT {name}.{suffix})',
                          f'target_include_directories({name} PRIVATE "{prefix.as_posix()}/include")']
                if definition:
                    cmake.append(f'target_compile_definitions({name} PRIVATE {definition})')
    cmake += ['include(CheckCSourceCompiles)', 'include(CheckCXXSourceCompiles)',
              f'set(CMAKE_REQUIRED_INCLUDES "{prefix.as_posix()}/include")',
              'set(CMAKE_REQUIRED_DEFINITIONS -DUQA_BUILD_SHARED -DUQA_USE_SHARED)',
              'check_c_source_compiles("#include <uqa/uqa.h>\\nint main(void) {return 0;}" UQA_INVALID_C_MODE)',
              'check_cxx_source_compiles("#include <uqa/uqa.h>\\nint main() {return 0;}" UQA_INVALID_CXX_MODE)',
              'if(UQA_INVALID_C_MODE OR UQA_INVALID_CXX_MODE)',
              '  message(FATAL_ERROR "Headers accept conflicting linkage modes")', 'endif()']
    (source / "CMakeLists.txt").write_text("\n".join(cmake) + "\n")
    return records


def execute(executable: Path, category: str, scenario: str, work: Path, prefix: Path) -> int:
    if category in {"identity", "interop"}:
        env = dict(os.environ, PATH=str(prefix / "bin") + os.pathsep + os.environ.get("PATH", ""))
        subprocess.run([str(executable)], env=env, check=True)
        return 1
    executions = 0
    for provider in PROVIDERS:
        if category == "example" and scenario == "storage_transactions" and provider == "memory":
            continue
        directory = work / f"{executable.stem}-{provider}"
        directory.mkdir()
        databases = directory / "databases"
        scratch = directory / "temporary"
        databases.mkdir()
        scratch.mkdir()
        command = [str(executable)]
        if category == "example" and provider != "memory":
            command += [provider.replace("_", "-"), str(databases / "database.uqa")]
        env = dict(os.environ, UQA_TEST_STORAGE=provider, UQA_TEST_DATABASE_DIR=str(databases),
                   TMPDIR=str(scratch), TMP=str(scratch), TEMP=str(scratch), SQLITE_TMPDIR=str(scratch),
                   PATH=str(prefix / "bin") + os.pathsep + os.environ.get("PATH", ""))
        subprocess.run(command, env=env, check=True)
        executions += 1
        print(f"Installed consumer passed: {executable.stem}, {provider}", flush=True)
    return executions


def pkg_config(prefix: Path, manifest: dict, work: Path) -> int:
    env = dict(os.environ, PKG_CONFIG_PATH=str(prefix / "lib/pkgconfig"))
    tool = os.environ.get("PKG_CONFIG", "pkg-config")
    windows = sys.platform == "win32"
    executions = 0
    for standard, compiler, language in (("c11", "cc", "c"), ("c++17", "c++", "c++")):
        for package, shared in (("uqa", True), ("uqa-static", False)):
            command = [tool, *(["--msvc-syntax"] if windows else [])]
            cflags = compiler_flags(subprocess.check_output([*command, "--cflags", package], env=env, text=True))
            libraries = compiler_flags(subprocess.check_output([*command, "--libs", package], env=env, text=True))
            flags = cflags + libraries
            if bool({"-DUQA_USE_SHARED", "/DUQA_USE_SHARED"} & set(flags)) != shared or {"-DUQA_BUILD_SHARED", "/DUQA_BUILD_SHARED"} & set(flags):
                raise ValueError(f"Incorrect pkg-config linkage definitions: {package}")
            if not shared and not any(flag.endswith((".a", ".lib")) for flag in flags):
                raise ValueError("Static pkg-config metadata must select the exact archive")
            source = work / f"pkg-{package}-{standard}.{'c' if language == 'c' else 'cpp'}"
            source.write_text(identity_source(manifest, shared))
            binary = source.with_suffix(".exe")
            if windows:
                command = ["cl", "/nologo", f"/std:{standard}", "/TC" if language == "c" else "/TP",
                           "/W4", "/WX", "/MD", "/UNDEBUG", *cflags, str(source),
                           f"/Fo{source.with_suffix('.obj')}", f"/Fe{binary}", "/link", *libraries]
            else:
                command = [compiler, f"-std={standard}", "-Wall", "-Wextra", "-Werror", "-pedantic",
                           *cflags, str(source), *libraries, f"-Wl,-rpath,{prefix / 'lib'}", "-o", str(binary)]
            subprocess.run(command, check=True)
            executions += execute(binary, "identity", "", work, prefix)
    return executions


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", required=True, type=Path)
    parser.add_argument("--allow-unknown-revision", action="store_true", help="Local development artifacts only")
    parser.add_argument("--symbols-only", action="store_true")
    args = parser.parse_args()
    prefix = args.prefix.resolve(strict=True)
    manifest = json.loads((prefix / "share/uqa/uqa-build.json").read_bytes())
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if manifest["source_revision"] != revision and not (args.allow_unknown_revision and manifest["source_revision"] == "unknown"):
        raise ValueError("Installed library source revision differs from the reviewed source")
    verify_symbols(prefix, manifest)
    if args.symbols_only:
        return
    with tempfile.TemporaryDirectory(prefix="uqa installed consumers ") as temporary:
        work = Path(temporary)
        source, build = work / "source", work / "build"
        source.mkdir()
        records = generate_consumers(source, prefix, manifest)
        subprocess.run(["cmake", "-S", str(source), "-B", str(build), f"-DCMAKE_PREFIX_PATH={prefix}",
                        "-DCMAKE_BUILD_TYPE=Release"], check=True)
        subprocess.run(["cmake", "--build", str(build), "--config", "Release", "--parallel", "2"], check=True)
        executions = 0
        for name, category, scenario in records:
            binary = build / (name + (".exe" if sys.platform == "win32" else ""))
            if not binary.is_file():
                binary = build / "Release" / binary.name
            executions += execute(binary, category, scenario, work, prefix)
        executions += pkg_config(prefix, manifest, work)
    print(f"Verified installed CMake/pkg-config consumers: {executions} executions", flush=True)


if __name__ == "__main__":
    main()
