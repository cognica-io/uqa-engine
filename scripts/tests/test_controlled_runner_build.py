#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import controlled_runner_host as runner


class ControlledBuildTest(unittest.TestCase):
    def test_operator_toolchain_is_resolved_before_source_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home = root / "operator-home"
            tools = home / ".rustup/toolchains/operator-selected/bin"
            calls = []

            def command(*args, cwd=None):
                calls.append((args, cwd))
                if args[-2:] == ("which", "cargo"):
                    self.assertEqual(cwd, home)
                    return str(tools / "cargo")
                executable = Path(args[-2])
                if executable.name in {"cargo", "rustc"}:
                    self.assertEqual(executable.parent, tools)
                return executable.name + " operator-version"

            with patch.object(runner, "CONTROL", root / "control"), \
                    patch.object(runner.pwd, "getpwnam", return_value=SimpleNamespace(pw_dir=str(home))), \
                    patch.object(runner, "command", side_effect=command), \
                    patch.object(Path, "read_text", return_value="boot-id"), \
                    patch.object(runner.ControlledHost, "make_writable"):
                host = runner.ControlledHost("toolchain-test", {})
            self.assertEqual(calls[0][0][-2:], ("which", "cargo"))
            for role in ("reference", "candidate"):
                source = root / role
                source.mkdir()
                (source / "rust-toolchain.toml").write_text(f'[toolchain]\nchannel = "{role}-only"\n')
                with patch.object(runner.subprocess, "run", return_value=SimpleNamespace(returncode=0)) as execute:
                    host.unit(role, [str(host.rust_bin / "cargo"), "--version"], source)
                args = execute.call_args.args[0]
                self.assertIn(f"--setenv=PATH={tools}:/usr/local/bin:/usr/bin:/bin", args)
                self.assertEqual(args[-2:], [str(tools / "cargo"), "--version"])

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repository = self.root / "repository"
        self.shared = self.root / "claims-reference-source"
        self.binaries = self.root / "binaries"
        self.git_environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        subprocess.run(["git", "init", "--quiet", str(self.repository)], env=self.git_environment, check=True)
        self.git("config", "user.name", "Build cache test")
        self.git("config", "user.email", "build-cache@example.invalid")
        source = self.repository / "lib.rs"
        source.write_text("fn original() {}\n")
        self.git("add", "lib.rs")
        self.git("commit", "--quiet", "-m", "Original source")
        self.reference = self.git("rev-parse", "HEAD")
        source.write_text("fn current() {}\n")
        self.git("commit", "--quiet", "-am", "Changed source")
        self.head = self.git("rev-parse", "HEAD")
        self.git("worktree", "add", "--quiet", "--detach", str(self.shared), self.head)
        self.host = runner.ControlledHost.__new__(runner.ControlledHost)
        self.host.output = self.root / "output"
        self.host.output.mkdir()
        self.host.rust_bin = self.root / "rust-bin"
        self.host.build_environment = "reviewed toolchain and build flags"
        self.host.git = self.git
        self.host.progress = Mock()
        self.host.unit = Mock(side_effect=AssertionError("unexpected build"))
        for name, value in (("WORK", self.root), ("REPOSITORY", self.repository), ("BINARY_ROOT", self.binaries)):
            patched = patch.object(runner, name, value)
            patched.start()
            self.addCleanup(patched.stop)

    def git(self, *args, cwd=None):
        return subprocess.check_output(["git", *args], cwd=cwd or self.repository,
                                       env=self.git_environment, stderr=subprocess.PIPE, text=True).strip()

    def cached_reference(self):
        directory = self.binaries / "claims-reference"
        directory.mkdir(parents=True, exist_ok=True)
        records = {}
        for name in ("analytical_comparison", "row_claim_contention"):
            executable = directory / name
            executable.write_bytes(name.encode())
            records[name] = {"revision": self.reference, "build_environment": self.host.build_environment,
                             "path": str(executable), "sha256": runner.file_hash(executable)}
        (directory / "artifacts.json").write_text(json.dumps(records))
        return records

    def test_cached_claim_reference_preserves_candidate_checkout_and_source_freshness(self):
        cached = self.cached_reference()
        source = self.shared / "lib.rs"
        os.utime(source, ns=(1_000_000_000, 1_000_000_000))
        before = source.stat().st_mtime_ns
        artifacts = self.host.build(self.reference, "claims-reference", True)
        self.assertEqual(self.git("rev-parse", "HEAD", cwd=self.shared), self.head)
        self.assertEqual(source.read_text(), "fn current() {}\n")
        self.assertEqual(source.stat().st_mtime_ns, before)
        self.assertEqual(artifacts, cached)
        self.assertEqual(json.loads((self.host.output / "claims-reference-build.json").read_text()), cached)
        self.host.unit.assert_not_called()

    def test_cache_rejects_different_revision_environment_digest_or_target_inventory(self):
        cached = self.cached_reference()
        variants = []
        for field, value in (("revision", self.head), ("build_environment", "other toolchain"), ("sha256", "0" * 64)):
            changed = copy.deepcopy(cached)
            changed["row_claim_contention"][field] = value
            variants.append(changed)
        variants.append({"row_claim_contention": cached["row_claim_contention"]})
        for variant in variants:
            with self.subTest(variant=variant):
                (self.binaries / "claims-reference/artifacts.json").write_text(json.dumps(variant))
                with patch.object(self.host, "source", side_effect=RuntimeError("build required")) as source:
                    with self.assertRaisesRegex(RuntimeError, "build required"):
                        self.host.build(self.reference, "claims-reference", True)
                    source.assert_called_once_with(self.reference, "claims-reference")

    def test_cache_miss_builds_exact_requested_source_and_records_verified_executables(self):
        for role, revision in (("claims-reference", self.reference), ("head", self.head)):
            with self.subTest(role=role):
                compiler_output = self.root / "compiler.stdout"
                rows = []
                for name in ("analytical_comparison", "row_claim_contention"):
                    executable = self.repository / "target/reference/release" / name
                    executable.parent.mkdir(parents=True, exist_ok=True)
                    executable.write_bytes((revision + name).encode())
                    rows.append({"reason": "compiler-artifact", "target": {"name": name},
                                 "executable": str(executable), "features": [],
                                 "profile": {"opt_level": "3", "debug_assertions": False}})
                compiler_output.write_text("\n".join(json.dumps(row) for row in rows))

                def compile_source(label, args, source, **kwargs):
                    self.assertEqual(self.git("rev-parse", "HEAD", cwd=source), revision)
                    self.assertIn("--locked", args)
                    if label.startswith("metadata-"):
                        metadata = self.root / "metadata.json"
                        target = {"name": "analytical_comparison", "kind": ["bench"]}
                        if role == "head":
                            target["required-features"] = ["duckdb"]
                        metadata.write_text(json.dumps({"packages": [{"name": "uqa-engine", "targets": [target]}]}))
                        return metadata
                    if role == "head":
                        self.assertEqual(args[args.index("--features") + 1], "uqa-engine/duckdb")
                    else:
                        self.assertNotIn("--features", args)
                    self.assertEqual(kwargs["environment"]["CARGO_INCREMENTAL"], "0")
                    return compiler_output

                self.host.unit = Mock(side_effect=compile_source)
                artifacts = self.host.build(revision, role, True)
                self.assertEqual(set(artifacts), {"analytical_comparison", "row_claim_contention"})
                for artifact in artifacts.values():
                    self.assertEqual(artifact["revision"], revision)
                    self.assertEqual(artifact["build_environment"], self.host.build_environment)
                    self.assertEqual(artifact["sha256"], runner.file_hash(Path(artifact["path"])))
                self.assertEqual(self.host.unit.call_count, 2)

    def test_fixture_ignores_the_callers_git_repository_environment(self):
        before = {name: (self.repository / ".git" / name).read_bytes() for name in ("HEAD", "index", "config")}
        environment = {"GIT_DIR": str(self.repository / ".git"), "GIT_WORK_TREE": str(self.repository),
                       "GIT_INDEX_FILE": str(self.root / "caller.index"),
                       "GIT_COMMON_DIR": str(self.repository / ".git")}
        result = unittest.TestResult()
        with patch.dict(os.environ, environment):
            ControlledBuildTest("test_cached_claim_reference_preserves_candidate_checkout_and_source_freshness").run(result)
        self.assertTrue(result.wasSuccessful(), result.errors + result.failures)
        self.assertEqual({name: (self.repository / ".git" / name).read_bytes() for name in before}, before)
        self.assertFalse((self.root / "caller.index").exists())


if __name__ == "__main__":
    unittest.main()
