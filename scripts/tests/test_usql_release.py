# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.

from __future__ import annotations

import importlib.util
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("usql_release", ROOT / "scripts/usql-release.py")
CLI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CLI)


class CLIReleaseTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.assets = pathlib.Path(cls.temporary.name)
        # Packaging tests use an inert payload with the real pinned dictionaries.
        # Only the native CI smoke test executes the actual Cargo-built binary.
        cls.payload = b"#!/bin/sh\nexit 0\n" + b"".join(
            (ROOT / f"crates/uqa-{language}-data/data/{language}.uqa{ending}").read_bytes()
            for language, ending in (("nori", "n"), ("kuromoji", "k"))
        )
        binary = cls.assets / "input"
        binary.write_bytes(cls.payload)
        for target in CLI.TARGETS:
            CLI.package(target, binary, cls.assets)

    def test_all_targets_have_verified_archives_checksums_and_pinned_installers(self):
        for target in CLI.TARGETS:
            CLI.verify(target, self.assets)
        for name in CLI.INSTALLERS:
            self.assertNotIn(b"@UQA_VERSION@", (self.assets / name).read_bytes())
        self.assertEqual(CLI.FEATURES, ["arrow", "duckdb", "kuromoji", "nori"])

    def test_windows_checkout_retains_cross_platform_asset_bytes(self):
        paths = ["scripts/" + name for name in CLI.INSTALLERS]
        paths += [str(path.relative_to(ROOT)) for path in (ROOT / "scripts/usql-release/licenses").glob("*.txt")]
        paths += ["scripts/fixtures/usql-release/rows.arrow"]
        with tempfile.TemporaryDirectory() as temporary:
            destination = pathlib.Path(temporary)
            subprocess.run(["git", "-c", "core.autocrlf=true", "checkout-index",
                            "--prefix=" + destination.as_posix() + "/", "--", *paths], cwd=ROOT, check=True)
            for path in paths:
                self.assertEqual((destination / path).read_bytes(), (ROOT / path).read_bytes(), path)

    def test_archive_damage_and_wrong_source_metadata_are_rejected(self):
        for target in (CLI.TARGETS[0], CLI.TARGETS[-1]):
            with tempfile.TemporaryDirectory() as temporary:
                copy = pathlib.Path(temporary) / "assets"
                shutil.copytree(self.assets, copy)
                archive = copy / CLI.asset_name(target)
                with archive.open("ab") as stream:
                    stream.write(b"damaged transport")
                with self.assertRaisesRegex(ValueError, "checksum"):
                    CLI.verify(target, copy)
                shutil.copyfile(self.assets / archive.name, archive)
                (copy / "install-usql.sh").write_text("wrong release")
                with self.assertRaisesRegex(ValueError, "installer"):
                    CLI.verify(target, copy)

    def test_missing_platform_does_not_pass_complete_inventory(self):
        with tempfile.TemporaryDirectory() as temporary:
            copy = pathlib.Path(temporary) / "assets"
            shutil.copytree(self.assets, copy)
            (copy / CLI.asset_name(CLI.TARGETS[-1])).unlink()
            result = subprocess.run([os.sys.executable, str(ROOT / "scripts/usql-release.py"),
                                     "verify", "--directory", str(copy)], capture_output=True)
            self.assertNotEqual(result.returncode, 0)

    @unittest.skipIf(os.name == "nt", "POSIX installer")
    def test_posix_installer_selects_every_target_and_preserves_install_on_bad_checksum(self):
        for system, machine, target in (("Linux", "x86_64", CLI.TARGETS[0]),
                                         ("Linux", "aarch64", CLI.TARGETS[1]),
                                         ("Darwin", "x86_64", CLI.TARGETS[2]),
                                         ("Darwin", "arm64", CLI.TARGETS[3])):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temporary:
                work = pathlib.Path(temporary)
                archive_dir = work / "downloaded assets"
                shutil.copytree(self.assets, archive_dir)
                bin_dir = work / "installed bin"
                uname = work / "uname"
                uname.write_text(f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {machine};; esac\n')
                uname.chmod(0o755)
                env = {**os.environ, "PATH": str(work) + os.pathsep + os.environ["PATH"]}
                command = ["sh", str(archive_dir / "install-usql.sh"), "--archive-dir", str(archive_dir), "--bin-dir", str(bin_dir)]
                subprocess.run(command, env=env, check=True, capture_output=True)
                installed = bin_dir / "usql"
                self.assertEqual(installed.read_bytes(), self.payload)
                self.assertTrue(os.access(installed, os.X_OK))
                installed.write_bytes(b"previous working installation")
                checksum = archive_dir / (CLI.asset_name(target) + ".sha256")
                checksum.write_text("0" * 64 + "  " + CLI.asset_name(target) + "\n")
                failed = subprocess.run(command, env=env, capture_output=True)
                self.assertNotEqual(failed.returncode, 0)
                self.assertIn(b"checksum mismatch", failed.stderr)
                self.assertEqual(installed.read_bytes(), b"previous working installation")
                self.assertEqual(list(bin_dir.iterdir()), [installed])

    @unittest.skipUnless(os.name == "nt", "Windows installer")
    def test_windows_installer_selects_both_targets_and_preserves_install_on_bad_checksum(self):
        for architecture, target in (("AMD64", CLI.TARGETS[4]), ("ARM64", CLI.TARGETS[5])):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temporary:
                work = pathlib.Path(temporary)
                archive_dir = work / "downloaded assets"
                shutil.copytree(self.assets, archive_dir)
                bin_dir = work / "installed bin"
                env = {**os.environ, "PROCESSOR_ARCHITEW6432": architecture}
                command = ["pwsh", "-NoProfile", "-File", str(archive_dir / "install-usql.ps1"),
                           "-ArchiveDirectory", str(archive_dir), "-BinDir", str(bin_dir)]
                subprocess.run(command, env=env, check=True, capture_output=True)
                installed = bin_dir / "usql.exe"
                self.assertEqual(installed.read_bytes(), self.payload)
                installed.write_bytes(b"previous working installation")
                (archive_dir / (CLI.asset_name(target) + ".sha256")).write_text("invalid")
                failed = subprocess.run(command, env=env, capture_output=True)
                self.assertNotEqual(failed.returncode, 0)
                self.assertEqual(installed.read_bytes(), b"previous working installation")
                self.assertEqual(list(bin_dir.iterdir()), [installed])

    @unittest.skipIf(os.name == "nt", "POSIX download transport")
    def test_downloads_are_pinned_and_unsupported_platforms_do_not_install(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = pathlib.Path(temporary)
            uname = work / "uname"
            uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
            uname.chmod(0o755)
            curl = work / "curl"
            curl.write_text('''#!/bin/sh
set -eu
while [ "$#" -gt 0 ]; do
    case "$1" in
        https://*) url=$1 ;;
        -o) shift; destination=$1 ;;
    esac
    shift
done
printf '%s\\n' "$url" >> "$URL_LOG"
cp "$ASSET_SOURCE/${url##*/}" "$destination"
''')
            curl.chmod(0o755)
            env = {**os.environ, "PATH": str(work) + os.pathsep + os.environ["PATH"],
                   "URL_LOG": str(work / "urls"), "ASSET_SOURCE": str(self.assets)}
            command = ["sh", str(self.assets / "install-usql.sh"), "--bin-dir", str(work / "bin")]
            subprocess.run(command, env=env, check=True, capture_output=True)
            base = f"https://github.com/cognica-io/uqa-engine/releases/download/v{CLI.version()}/"
            asset = CLI.asset_name(CLI.TARGETS[0])
            self.assertEqual((work / "urls").read_text().splitlines(), [base + asset, base + asset + ".sha256"])
            uname.write_text('#!/bin/sh\necho unsupported\n')
            failed = subprocess.run(command, env=env, capture_output=True)
            self.assertNotEqual(failed.returncode, 0)
            self.assertEqual((work / "bin/usql").read_bytes(), self.payload)

    def test_release_builds_precede_publication_and_checks_do_not_run_on_each_push(self):
        release = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertIn("include_cli: true", release)
        self.assertIn("needs: [resolve, python, javascript, benchmarks]", release)
        checks = (ROOT / ".github/workflows/usql-release-checks.yml").read_text()
        self.assertIn("types: [ready_for_review]", checks)
        self.assertNotIn("  push:", checks)
        self.assertNotIn("workflow_dispatch:", checks)
        action = (ROOT / ".github/actions/package-usql/action.yml").read_text()
        self.assertIn("--features arrow,duckdb", action)
        self.assertIn("--profile release-stripped", action)
        self.assertIn("usql-release.py smoke", action)
        self.assertIn("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS: -C target-feature=+crt-static", action)
        self.assertIn("CARGO_TARGET_AARCH64_PC_WINDOWS_MSVC_RUSTFLAGS: -C target-feature=+crt-static", action)
        self.assertNotIn("--no-default-features", action)
