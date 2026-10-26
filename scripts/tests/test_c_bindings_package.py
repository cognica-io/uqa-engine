#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))


def module(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


RELEASE = module("c_release", "c-bindings-release.py")
BUILD = module("c_build", "build-c-bindings.py")
METADATA = module("c_metadata", "c-bindings-metadata.py")
LICENSES = module("c_licenses", "check-release-licenses.py")
DEPENDENCIES = module("c_dependency_licenses", "c_bindings_licenses.py")
VERIFY = module("c_verify", "verify-c-package.py")
COMMIT = "a" * 40
TARGET = "x86_64-unknown-linux-gnu"
PKG_CONFIG = shutil.which(os.environ.get("PKG_CONFIG", "pkg-config"))


def fixture(prefix: Path, target: str = TARGET):
    exported, deferred = METADATA.symbols()
    legal = b"independent dependency license fixture"
    manifest = {"schema_version": 1, "abi_major": 1, "version": RELEASE.version(),
                "target": target, "source_revision": COMMIT, "features": ["kuromoji", "nori"],
                "exports": exported, "declaration_only": {"uqa_log.h": deferred},
                "native_dynamic_libraries": ["libc.so.6"],
                "dependencies": [{"files": [{"path": "fixture.txt", "sha256": hashlib.sha256(legal).hexdigest()}]}]}
    files = {"share/uqa/uqa-build.json": json.dumps(manifest).encode(),
             "share/uqa/licenses/fixture.txt": legal,
             "lib/pkgconfig/uqa.pc": b"shared metadata", "lib/pkgconfig/uqa-static.pc": b"static metadata",
             "lib/cmake/UQA/UQAConfig.cmake": b"configuration", "lib/cmake/UQA/UQAConfigVersion.cmake": b"version",
             "lib/cmake/UQA/UQATargets.cmake": b"targets"}
    files.update({f"include/uqa/{p.name}": p.read_bytes() for p in (ROOT / "crates/uqa-ffi/include/uqa").glob("*.h")})
    if "windows" in target:
        files.update({"bin/uqa.dll": b"shared fixture", "lib/uqa.lib": b"import fixture", "lib/uqa_static.lib": b"static fixture"})
    else:
        files.update({"lib/libuqa.so": b"shared fixture", "lib/libuqa.a": b"static fixture"})
    for name, data in files.items():
        path = prefix / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    return manifest


class CPackageVerification(unittest.TestCase):
    @unittest.skipUnless(all(shutil.which(tool) for tool in ("cmake", "ninja", "clang-cl", "llvm-rc", "llvm-mt", "llvm-lib", "llvm-readobj", "lld-link")),
                         "CMake, Ninja and LLVM's MSVC tools are required for COFF link verification")
    def test_msvc_runtime_directive_reaches_shared_and_installed_static_links(self):
        with tempfile.TemporaryDirectory(prefix="uqa MSVC package ") as temporary:
            work = Path(temporary)
            prefix, build = work / "prefix", work / "build"
            archive, metadata = work / "uqa.lib", work / "cargo-metadata.json"
            metadata.write_text(json.dumps({
                "packages": [{"id": "ffi", "name": "uqa-ffi", "version": RELEASE.version(),
                              "manifest_path": str(ROOT / "crates/uqa-ffi/Cargo.toml")}],
                "resolve": {"nodes": [{"id": "ffi", "features": [], "deps": []}]}}))
            # A freestanding COFF archive needs no Windows SDK. Duplicate private
            # members model Rust import records; a separate runtime archive models
            # CRT startup and its unused fallback for the native cleanup entry.
            toolchain = ["-G", "Ninja", "-DCMAKE_SYSTEM_NAME=Windows",
                         "-DCMAKE_C_COMPILER=" + Path(shutil.which("clang-cl")).as_posix(),
                         "-DCMAKE_RC_COMPILER=" + Path(shutil.which("llvm-rc")).as_posix(),
                         "-DCMAKE_MT=" + Path(shutil.which("llvm-mt")).as_posix(),
                         "-DCMAKE_C_COMPILER_TARGET=x86_64-pc-windows-msvc",
                         "-DCMAKE_C_COMPILER_WORKS=TRUE", "-DCMAKE_C_ABI_COMPILED=TRUE",
                         "-DCMAKE_SIZEOF_VOID_P=8", "-DCMAKE_BUILD_TYPE=Release",
                         "-DCMAKE_LINKER=" + Path(shutil.which("lld-link")).as_posix(),
                         "-DCMAKE_C_FLAGS=/Zl /GS-",
                         "-DCMAKE_C_STANDARD_LIBRARIES=",
                         "-DCMAKE_SHARED_LINKER_FLAGS=/manifest:no /entry:CRTStartup "
                         + '/libpath:"' + str(work) + '" /map:"' + str(work / "linked.map") + '"']

            def run(*arguments):
                result = subprocess.run(arguments, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, encoding="utf-8")
                self.assertEqual(result.returncode, 0, result.stdout)
                return result.stdout

            public, _ = METADATA.symbols()
            objects = []
            for name, definitions in (
                    ("api_first", public[::2]), ("api_second", public[1::2]),
                    ("native_lifecycle", ["DllMain"]),
                    ("private_first", ["private_import_record"]),
                    ("private_second", ["private_import_record"])):
                source, obj = work / (name + ".c"), work / (name + ".obj")
                source.write_text(
                    "int DllMain(void *module, unsigned long reason, void *reserved) { return 1; }"
                    if name == "native_lifecycle" else
                    "int system_dependency(void);\n" +
                    "\n".join(f"int {symbol}(void) {{ return system_dependency(); }}" for symbol in definitions))
                run("clang-cl", "--target=x86_64-pc-windows-msvc", "/c", "/GS-", "/Zl", str(source), "/Fo" + str(obj))
                objects.append(str(obj))
            run("llvm-lib", "/out:" + str(archive), *objects)
            native_source, native_obj = work / "system.c", work / "system.obj"
            native_source.write_text("int system_dependency(void) { return 0; }\n")
            run("clang-cl", "--target=x86_64-pc-windows-msvc", "/c", "/GS-", "/Zl",
                str(native_source), "/Fo" + str(native_obj))
            run("llvm-lib", "/out:" + str(work / "bcrypt.lib"), str(native_obj))
            runtime_objects = []
            for name, body in (
                    ("crt_entry", "int DllMain(void *, unsigned long, void *); "
                     "int CRTStartup(void *module, unsigned long reason, void *reserved) { "
                     "return DllMain(module, reason, reserved); }"),
                    ("crt_fallback", "int DllMain(void *module, unsigned long reason, void *reserved) { return 1; }")):
                source, obj = work / (name + ".c"), work / (name + ".obj")
                source.write_text(body)
                run("clang-cl", "--target=x86_64-pc-windows-msvc", "/c", "/GS-", "/Zl", str(source), "/Fo" + str(obj))
                runtime_objects.append(str(obj))
            run("llvm-lib", "/out:" + str(work / "msvcrt.lib"), *runtime_objects)
            run("cmake", "-S", str(ROOT / "crates/uqa-ffi"), "-B", str(build), *toolchain,
                "-DUQA_RUST_STATIC_LIBRARY=" + str(archive),
                "-DUQA_NATIVE_LINK_FLAGS=bcrypt.lib /defaultlib:msvcrt", "-DUQA_SOURCE_REVISION=" + COMMIT,
                "-DUQA_TARGET=x86_64-pc-windows-msvc", "-DUQA_CARGO_METADATA=" + str(metadata),
                "-DCMAKE_INSTALL_LIBDIR=lib", "-DCMAKE_INSTALL_PREFIX=" + str(prefix))
            shared = run("cmake", "--build", str(build), "--", "-n", "-v")
            self.assertIn("/defaultlib:msvcrt", shared)
            self.assertIn("/INCLUDE:DllMain", shared)
            self.assertNotIn("msvcrt.obj", shared)
            run("cmake", "--build", str(build))
            linked = (work / "linked.map").read_text()
            self.assertIn("uqa:native_lifecycle.obj", linked)
            self.assertIn("msvcrt:crt_entry.obj", linked)
            self.assertNotIn("crt_fallback", linked)
            exports = run("llvm-readobj", "--coff-exports", str(build / "uqa.dll"))
            names = [line.strip().removeprefix("Name: ") for line in exports.splitlines()
                     if line.strip().startswith("Name: ")]
            self.assertEqual(sorted(names), public)
            installed = prefix / "lib/cmake/UQA"
            installed.mkdir(parents=True)
            shutil.copyfile(build / "UQAConfig.cmake", installed / "UQAConfig.cmake")
            (installed / "UQATargets.cmake").write_text("add_library(UQA::uqa INTERFACE IMPORTED)\n")
            shutil.copyfile(archive, prefix / "lib/uqa_static.lib")
            (prefix / "include").mkdir()
            consumer = work / "consumer"
            consumer.mkdir()
            (consumer / "main.c").write_text("int main(void) { return 0; }\n")
            (consumer / "CMakeLists.txt").write_text(
                "cmake_minimum_required(VERSION 3.24)\nproject(Consumer C)\n"
                "find_package(UQA REQUIRED CONFIG)\nadd_executable(client main.c)\n"
                "target_link_libraries(client PRIVATE UQA::uqa_static)\n")
            run("cmake", "-S", str(consumer), "-B", str(consumer / "build"), *toolchain,
                "-DCMAKE_PREFIX_PATH=" + str(prefix))
            static = run("cmake", "--build", str(consumer / "build"), "--", "-n", "-v")
            self.assertIn("/defaultlib:msvcrt", static)
            self.assertNotIn("/INCLUDE:DllMain", static)
            self.assertNotIn("msvcrt.obj", static)

            if PKG_CONFIG:
                directory = prefix / "lib/pkgconfig"
                directory.mkdir()
                for name in ("uqa", "uqa-static"):
                    shutil.copyfile(build / (name + ".pc"), directory / (name + ".pc"))
                shutil.copyfile(build / "uqa.lib", prefix / "lib/uqa.lib")
                shutil.copytree(ROOT / "crates/uqa-ffi/include/uqa", prefix / "include/uqa")
                env = dict(os.environ, PKG_CONFIG_PATH=str(directory))
                for package in ("uqa", "uqa-static"):
                    cflags = VERIFY.compiler_flags(subprocess.check_output(
                        [PKG_CONFIG, "--msvc-syntax", "--cflags", package], env=env, text=True))
                    libraries = VERIFY.compiler_flags(subprocess.check_output(
                        [PKG_CONFIG, "--msvc-syntax", "--libs", package], env=env, text=True))
                    source, obj = work / (package + ".c"), work / (package + ".obj")
                    executable = work / (package + ".exe")
                    source.write_text('#include <uqa/uqa.h>\nint main(void) { uint32_t abi = 0; '
                                      'return (int)uqa_library_get_abi_version(&abi); }\n')
                    run("clang-cl", "--target=x86_64-pc-windows-msvc", "/c", "/GS-", "/Zl",
                        "/clang:-ffreestanding", *cflags, str(source), "/Fo" + str(obj))
                    run("lld-link", "/entry:main", "/subsystem:console",
                        "/out:" + str(executable), str(obj), *libraries, "/libpath:" + str(work))
                    imports = run("llvm-readobj", "--coff-imports", str(executable))
                    self.assertEqual("Name: uqa.dll" in imports, package == "uqa")
                    if package == "uqa-static":
                        self.assertIn("uqa_static.lib", libraries)
                        self.assertIn("bcrypt.lib", libraries)
                        self.assertIn("msvcrt.lib", libraries)

    @unittest.skipUnless(sys.platform == "win32", "requires the native Windows argument parser")
    def test_windows_pkgconf_output_terminators_do_not_become_arguments(self):
        for flags in ([r'/IC:\Library path\include', "/DUQA_USE_SHARED"],
                      [r'/libpath:C:\Library path\lib', "uqa.lib"],
                      [r'/IC:\Library path\trailing\\'], []):
            for ending in ("\n", "\r\n", " \n", "\t\r\n"):
                with self.subTest(flags=flags, ending=ending):
                    line = subprocess.list2cmdline(flags) + ending
                    self.assertEqual(VERIFY.compiler_flags(line), flags)

    def test_msvc_pkg_config_rejects_link_inputs_it_cannot_preserve(self):
        for flags in ("bcrypt.lib /WHOLEARCHIVE:other.lib", "bcrypt.lib /DEBUG",
                      '"C:/external library/other.lib"', "/defaultlib:"):
            with self.subTest(flags=flags), self.assertRaisesRegex(ValueError, "pkg-config MSVC"):
                METADATA.msvc_pkg_config_libraries(flags)

    def test_cargo_metadata_and_logs_preserve_utf8_on_a_legacy_locale(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            prefix, build = work / "prefix", work / "build"
            fixture(prefix, "x86_64-pc-windows-msvc")
            metadata = json.dumps({"target_directory": str(work / "Łibrary target"),
                                   "authors": ["Łucja"]}, ensure_ascii=False).encode("utf-8")
            build_log = "Compiling Łibrary\nnative-static-libs: bcrypt.lib\n".encode("utf-8")
            real_output = subprocess.check_output

            def output(command, **options):
                if command[:2] == ["git", "rev-parse"]:
                    return COMMIT
                self.assertEqual(command[:2], ["cargo", "metadata"])
                # Exercise real pipe bytes with Windows' legacy text-decoding default.
                if options.get("text") and "encoding" not in options:
                    options["encoding"] = "cp1252"
                return real_output([sys.executable, "-c",
                                    "import sys;sys.stdout.buffer.write(" + repr(metadata) + ")"], **options)

            def run(command, **options):
                if command[:2] == ["cargo", "rustc"]:
                    data = build_log.decode(options.get("encoding", "cp1252")) if options.get("text") else build_log
                    return subprocess.CompletedProcess(command, 0, data)
                if command[0] == sys.executable:
                    return original_run(command, **options)
                self.assertEqual(command[0], "cmake")
                return subprocess.CompletedProcess(command, 0)

            original_run = subprocess.run
            console = mock.Mock(buffer=io.BytesIO())
            arguments = ["build-c-bindings.py", "--target", "x86_64-pc-windows-msvc",
                         "--prefix", str(prefix), "--build-directory", str(build)]
            with mock.patch.object(sys, "argv", arguments), mock.patch.object(sys, "stdout", console), \
                    mock.patch.object(BUILD.subprocess, "check_output", side_effect=output), \
                    mock.patch.object(BUILD.subprocess, "run", side_effect=run), \
                    mock.patch.object(BUILD, "dynamic_dependencies", return_value=["KERNEL32.dll"]):
                BUILD.main()
            self.assertEqual((build / "cargo-metadata.json").read_bytes(), metadata)
            self.assertEqual((build / "native-build.log").read_bytes(), build_log)
            self.assertEqual(console.buffer.getvalue(), build_log)
            self.assertEqual(json.loads((build / "cargo-metadata.json").read_bytes())["authors"], ["Łucja"])

    def test_bundled_native_build_source_keeps_its_own_license(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            toolchain = work / "toolchain"
            documentation = toolchain / "share/doc/rust"
            documentation.mkdir(parents=True)
            (documentation / "COPYRIGHT-library.html").write_text("Rust runtime inventory")
            packages, nodes = [], []
            for name in ("uqa-ffi", "openssl-sys", "openssl-src", "cc"):
                directory = work / ("Łibrary-" + name)
                directory.mkdir()
                (directory / "LICENSE").write_text(name + " license")
                packages.append({"id": name, "name": name, "version": "1.0.0",
                                 "manifest_path": str(directory / "Cargo.toml"),
                                 "license_file": None, "license": "fixture", "repository": None})
                nodes.append({"id": name, "features": [], "deps": []})
            nodes[0]["deps"] = [{"pkg": "openssl-sys", "dep_kinds": [{"kind": None}]}]
            nodes[1]["deps"] = [{"pkg": name, "dep_kinds": [{"kind": "build"}]}
                                for name in ("openssl-src", "cc")]
            # The source wrapper's root license does not replace the bundled C
            # library's own license, which is the actual distributed native code.
            (work / "Łibrary-openssl-src/openssl").mkdir()
            (work / "Łibrary-openssl-src/openssl/LICENSE.txt").write_text("OpenSSL native license")
            metadata = work / "metadata.json"
            metadata.write_bytes(json.dumps({"packages": packages, "resolve": {"nodes": nodes}}, ensure_ascii=False).encode("utf-8"))
            with mock.patch.object(DEPENDENCIES.subprocess, "check_output", side_effect=[str(toolchain), "rustc fixture"]):
                files, inventory = DEPENDENCIES.collect(metadata, set())
            self.assertEqual(files["THIRD-PARTY/crates/openssl-src-1.0.0/openssl/LICENSE.txt"], b"OpenSSL native license")
            self.assertEqual({package["name"] for package in inventory},
                             {"openssl-sys", "openssl-src", "rust-standard-library"})

    @unittest.skipUnless(PKG_CONFIG, "pkg-config is required for this metadata check")
    def test_static_pkg_config_escapes_a_relocated_archive_path_once(self):
        with tempfile.TemporaryDirectory(prefix="uqa pkg-config space ") as temporary:
            prefix = Path(temporary)
            directory = prefix / "lib/pkgconfig"
            directory.mkdir(parents=True)
            source = (ROOT / "crates/uqa-ffi/cmake/uqa-static.pc.in").read_text()
            for name, value in {"CMAKE_INSTALL_LIBDIR": "lib", "CMAKE_INSTALL_INCLUDEDIR": "include",
                                "UQA_PKG_CONFIG_STATIC_LIBRARY": "${libdir}/libuqa.a",
                                "UQA_PKG_CONFIG_NATIVE_LIBRARIES": "-lm",
                                "UQA_VERSION": "0.6.1"}.items():
                source = source.replace("@" + name + "@", value)
            (directory / "uqa-static.pc").write_text(source)
            flags = shlex.split(subprocess.check_output([PKG_CONFIG, "--cflags", "--libs", "uqa-static"],
                                env=dict(os.environ, PKG_CONFIG_PATH=str(directory)), text=True))
            archive = next(flag for flag in flags if flag.endswith(".a"))
            self.assertEqual(Path(archive).resolve(), (prefix / "lib/libuqa.a").resolve())
            self.assertNotIn("-DUQA_USE_SHARED", flags)

    def test_archive_verifies_exact_identity_and_detects_corrupted_bytes(self):
        for target in (TARGET, "x86_64-pc-windows-msvc"):
            with tempfile.TemporaryDirectory() as temporary, mock.patch.object(RELEASE.subprocess, "check_output", return_value=COMMIT):
                work = Path(temporary)
                fixture(work / "prefix", target)
                RELEASE.package(work / "prefix", target, work / "dist")
                RELEASE.verify(target, work / "dist")
                archive = work / "dist" / RELEASE.asset_name(target)
                with archive.open("ab") as stream:
                    stream.write(b"changed after qualification")
                with self.assertRaisesRegex(ValueError, "checksum"):
                    RELEASE.verify(target, work / "dist")

    def test_source_features_symbols_and_license_digest_are_required(self):
        with tempfile.TemporaryDirectory() as temporary:
            manifest = fixture(Path(temporary))
            entries = {path.relative_to(temporary).as_posix(): {"sha256": RELEASE.digest(path)}
                       for path in Path(temporary).rglob("*") if path.is_file()}
            RELEASE.validate_inventory(entries, manifest, TARGET, COMMIT)
            for key, value in (("source_revision", "b" * 40), ("target", "aarch64-apple-darwin"),
                               ("features", []), ("exports", manifest["exports"][:-1]),
                               ("declaration_only", {})):
                with self.subTest(key=key), self.assertRaises(ValueError):
                    RELEASE.validate_inventory(entries, {**manifest, key: value}, TARGET, COMMIT)
            changed = copy.deepcopy(entries)
            changed["share/uqa/licenses/fixture.txt"]["sha256"] = "0" * 64
            with self.assertRaisesRegex(ValueError, "license"):
                RELEASE.validate_inventory(changed, manifest, TARGET, COMMIT)

    def test_archive_paths_and_links_cannot_escape_or_cycle(self):
        for name in ("../file", "/file", "a/../../file", "C:/file", "a\\file", "a//b", "a/./b", "."):
            with self.subTest(path=name), self.assertRaises(ValueError):
                RELEASE.safe_path(name)
        with tempfile.TemporaryDirectory() as temporary:
            manifest = fixture(Path(temporary))
            entries = {path.relative_to(temporary).as_posix(): {"sha256": RELEASE.digest(path)}
                       for path in Path(temporary).rglob("*") if path.is_file()}
            for links in ({"a": {"link": "a"}}, {"a": {"link": "b"}, "b": {"link": "a"}},
                          {"a": {"link": "missing"}}, {"a": {"link": "../outside"}}):
                with self.subTest(links=links), self.assertRaises(ValueError):
                    RELEASE.validate_inventory(entries | links, manifest, TARGET, COMMIT)

    def test_both_c_libraries_must_contain_each_complete_dictionary(self):
        # Independent sentinel bytes exercise the inventory policy without a
        # release build or dependence on a historical machine artifact.
        bundle = b"complete pinned dictionary sentinel"
        for shared, static in (("lib/libuqa.so.0.6.1", "lib/libuqa.a"),
                               ("lib/libuqa.0.6.1.dylib", "lib/libuqa.a"),
                               ("bin/uqa.dll", "lib/uqa_static.lib")):
            with mock.patch.object(Path, "read_bytes", return_value=bundle):
                members = {shared: b"before" + bundle, static: bundle + b"after"}
                LICENSES.check_embedded_dictionary(Path("uqa-c-test.tgz"), members, "nori")
                for missing in (shared, static):
                    with self.assertRaisesRegex(RuntimeError, "both C libraries"):
                        LICENSES.check_embedded_dictionary(Path("uqa-c-test.tgz"), {**members, missing: b"wrong bundle"}, "nori")
                with self.assertRaises(RuntimeError):
                    LICENSES.check_embedded_dictionary(Path("uqa-c-test.tgz"), {**members, "other/libuqa.a": bundle}, "nori")


if __name__ == "__main__":
    unittest.main()
