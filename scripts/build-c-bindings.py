#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Build and install a target-specific C library with exact native link metadata."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def dynamic_dependencies(prefix: Path, target: str) -> list[str]:
    if "windows" in target:
        output = subprocess.check_output(["dumpbin", "/NOLOGO", "/DEPENDENTS", str(prefix / "bin/uqa.dll")], text=True)
        dependencies = re.findall(r"^\s+([A-Za-z0-9_.-]+\.dll)\s*$", output, re.M | re.I)
    elif "apple" in target:
        output = subprocess.check_output(["otool", "-L", str(prefix / "lib/libuqa.dylib")], text=True)
        dependencies = [line.strip().split(" (", 1)[0] for line in output.splitlines()[2:] if line.strip()]
    else:
        output = subprocess.check_output(["readelf", "-d", str(prefix / "lib/libuqa.so")], text=True)
        dependencies = re.findall(r"\(NEEDED\).*\[([^\]]+)\]", output)
    if not dependencies:
        raise ValueError("The installed library has no readable native dependency inventory")
    return sorted(set(dependencies))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--prefix", required=True, type=Path)
    parser.add_argument("--build-directory", required=True, type=Path)
    parser.add_argument("--features", default="nori,kuromoji")
    parser.add_argument("--profile", choices=("dev", "release"), default="dev")
    args = parser.parse_args()
    features = sorted(set(filter(None, args.features.split(","))))
    if set(features) - {"nori", "kuromoji", "arrow", "duckdb", "arrow-export"}:
        parser.error("Unknown C binding feature")
    minimum = os.environ.get("MACOSX_DEPLOYMENT_TARGET")
    if "apple" in args.target and not minimum:
        parser.error("Set MACOSX_DEPLOYMENT_TARGET before building Rust and native dependencies")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    env = dict(os.environ, UQA_SOURCE_REVISION=revision, CARGO_TERM_COLOR="never")
    build = args.build_directory.resolve()
    build.mkdir(parents=True, exist_ok=True)
    feature_args = ["--no-default-features"]
    if features:
        feature_args += ["--features", ",".join(features)]
    # A staticlib-only compilation leaves the distribution's export decisions to
    # its linker; PE objects must not inherit a cdylib's implicit export directives.
    command = ["cargo", "rustc", "--locked", "-p", "uqa-ffi", "--lib", "--crate-type", "staticlib",
               "--target", args.target, *feature_args]
    if args.profile == "release":
        command += ["--release"]
    command += ["--", "--print=native-static-libs"]
    result = subprocess.run(command, cwd=ROOT, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (build / "native-build.log").write_text(result.stdout)
    print(result.stdout, end="", flush=True)
    result.check_returncode()
    matches = re.findall(r"native-static-libs:\s*([^\r\n]+)", result.stdout)
    if not matches:
        raise ValueError("rustc did not report native static-library dependencies")
    flags = matches[-1].strip()
    metadata = subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", args.target,
         "--manifest-path", str(ROOT / "crates/uqa-ffi/Cargo.toml"), *feature_args], cwd=ROOT, env=env, text=True)
    metadata_path = build / "cargo-metadata.json"
    metadata_path.write_text(metadata)
    target_directory = Path(json.loads(metadata)["target_directory"])
    archive = target_directory / args.target / ("release" if args.profile == "release" else "debug")
    archive /= "uqa_ffi.lib" if "windows" in args.target else "libuqa_ffi.a"
    configure = ["cmake", "-S", str(ROOT / "crates/uqa-ffi"), "-B", str(build / "cmake"),
                 f"-DUQA_RUST_STATIC_LIBRARY={archive}", f"-DUQA_NATIVE_LINK_FLAGS={flags}",
                 f"-DUQA_SOURCE_REVISION={revision}", f"-DUQA_FEATURES={','.join(features)}",
                 f"-DUQA_TARGET={args.target}", f"-DUQA_CARGO_METADATA={metadata_path}",
                 f"-DCMAKE_INSTALL_PREFIX={args.prefix.resolve()}", "-DCMAKE_INSTALL_LIBDIR=lib",
                 "-DCMAKE_BUILD_TYPE=Release"]
    if "apple" in args.target:
        configure += [f"-DCMAKE_OSX_DEPLOYMENT_TARGET={minimum}"]
    subprocess.run(configure, check=True, env=env)
    subprocess.run(["cmake", "--build", str(build / "cmake"), "--config", "Release"], check=True, env=env)
    subprocess.run(["cmake", "--install", str(build / "cmake"), "--config", "Release"], check=True, env=env)
    manifest_path = args.prefix / "share/uqa/uqa-build.json"
    manifest = json.loads(manifest_path.read_text())
    manifest["native_dynamic_libraries"] = dynamic_dependencies(args.prefix, args.target)
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
