#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exercise the C library alongside an independently linked host libxml2 allocator."""

import argparse
import hashlib
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]


def host_library(directory: Path) -> tuple[list[str], list[str]]:
    source = ROOT / "crates/uqa-sql/vendor/libxml2"
    build = directory / "host-build"
    prefix = directory / "host"
    # Build unmodified upstream sources without UQA's symbol map or allocator bridge.
    features = re.findall(r"option\(LIBXML2_WITH_(\w+)",
                          (source / "CMakeLists.txt").read_text())
    configure = ["cmake", "-S", str(source), "-B", str(build),
                 "-DCMAKE_BUILD_TYPE=Debug", "-DBUILD_SHARED_LIBS=OFF",
                 "-DCMAKE_POSITION_INDEPENDENT_CODE=ON", "-DCMAKE_INSTALL_LIBDIR=lib",
                 f"-DCMAKE_INSTALL_PREFIX={prefix}"]
    configure += [f"-DLIBXML2_WITH_{feature}={'ON' if feature in {'THREADS', 'VALID'} else 'OFF'}"
                  for feature in features]
    subprocess.run(configure, check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["cmake", "--build", str(build), "--target", "install", "--parallel", "2"],
                   check=True, stdout=subprocess.DEVNULL)
    return ["-DLIBXML_STATIC", f"-I{prefix / 'include/libxml2'}"], [str(prefix / "lib/libxml2.a"), "-lm"]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", required=True, type=Path)
    parser.add_argument("--include", type=Path, default=ROOT / "crates/uqa-ffi/include")
    parser.add_argument("--static", action="store_true")
    parser.add_argument("--link-args", default="")
    parser.add_argument("--sanitize", action="store_true")
    parser.add_argument("--xml-config", help="Use an installed host libxml2 instead of an independent upstream build")
    args = parser.parse_args()
    library = args.library.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="uqa-xml-host-") as temporary:
        if args.xml_config:
            cflags = shlex.split(subprocess.check_output([args.xml_config, "--cflags"], text=True))
            libraries = shlex.split(subprocess.check_output([args.xml_config, "--libs"], text=True))
        else:
            cflags, libraries = host_library(Path(temporary))
        for language, standard, compiler in (
            ("c", "c11", os.environ.get("CC", "cc")),
            ("c++", "c++17", os.environ.get("CXX", "c++")),
        ):
            executable = Path(temporary) / standard
            command = [*shlex.split(compiler), "-x", language, f"-std={standard}",
                       "-Wall", "-Wextra", "-Werror", "-pedantic", "-pthread",
                       "-I", str(args.include), *cflags]
            # This fixture deliberately uses the host's legacy allocator API to detect interference.
            command += ["-Wno-deprecated-declarations"]
            if args.sanitize:
                command += ["-fsanitize=address,undefined", "-fno-omit-frame-pointer"]
            if not args.static:
                command += ["-DUQA_USE_SHARED"]
            command += [str(ROOT / "crates/uqa-ffi/tests/interop/xml_host.c"),
                        "-x", "none", str(library), *libraries, *shlex.split(args.link_args)]
            if not args.static:
                command += [f"-Wl,-rpath,{library.parent}"]
            subprocess.run([*command, "-o", str(executable)], check=True)
            subprocess.run([str(executable)], check=True)
    with library.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    print(f"Passed C11/C++17 host XML allocator isolation; {library.name} sha256={digest}")


if __name__ == "__main__":
    main()
