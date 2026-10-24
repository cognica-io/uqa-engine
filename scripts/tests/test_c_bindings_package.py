#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import copy
import hashlib
import importlib.util
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
METADATA = module("c_metadata", "c-bindings-metadata.py")
LICENSES = module("c_licenses", "check-release-licenses.py")
DEPENDENCIES = module("c_dependency_licenses", "c_bindings_licenses.py")
COMMIT = "a" * 40
TARGET = "x86_64-unknown-linux-gnu"


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
    def test_bundled_native_build_source_keeps_its_own_license(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            toolchain = work / "toolchain"
            documentation = toolchain / "share/doc/rust"
            documentation.mkdir(parents=True)
            (documentation / "COPYRIGHT-library.html").write_text("Rust runtime inventory")
            packages, nodes = [], []
            for name in ("uqa-ffi", "openssl-sys", "openssl-src", "cc"):
                directory = work / name
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
            (work / "openssl-src/openssl").mkdir()
            (work / "openssl-src/openssl/LICENSE.txt").write_text("OpenSSL native license")
            metadata = work / "metadata.json"
            metadata.write_text(json.dumps({"packages": packages, "resolve": {"nodes": nodes}}))
            with mock.patch.object(DEPENDENCIES.subprocess, "check_output", side_effect=[str(toolchain), "rustc fixture"]):
                files, inventory = DEPENDENCIES.collect(metadata, set())
            self.assertEqual(files["THIRD-PARTY/crates/openssl-src-1.0.0/openssl/LICENSE.txt"], b"OpenSSL native license")
            self.assertEqual({package["name"] for package in inventory},
                             {"openssl-sys", "openssl-src", "rust-standard-library"})

    @unittest.skipUnless(shutil.which("pkg-config"), "pkg-config is required for this metadata check")
    def test_static_pkg_config_escapes_a_relocated_archive_path_once(self):
        with tempfile.TemporaryDirectory(prefix="uqa pkg-config space ") as temporary:
            prefix = Path(temporary)
            directory = prefix / "lib/pkgconfig"
            directory.mkdir(parents=True)
            source = (ROOT / "crates/uqa-ffi/cmake/uqa-static.pc.in").read_text()
            for name, value in {"CMAKE_INSTALL_LIBDIR": "lib", "CMAKE_INSTALL_INCLUDEDIR": "include",
                                "UQA_STATIC_FILENAME": "libuqa.a", "UQA_NATIVE_LINK_FLAGS": "-lm",
                                "UQA_VERSION": "0.6.1"}.items():
                source = source.replace("@" + name + "@", value)
            (directory / "uqa-static.pc").write_text(source)
            flags = shlex.split(subprocess.check_output(["pkg-config", "--cflags", "--libs", "uqa-static"],
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
