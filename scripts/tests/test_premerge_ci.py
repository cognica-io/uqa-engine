#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import textwrap
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "run-premerge-ci.sh"
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
HEAD = "a" * 40
BASE = "b" * 40


class PremergeCIWorkflowContractTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.workflow = WORKFLOW.read_text(encoding="utf-8")

    def test_main_push_seeds_only_cache_identity_changes(self) -> None:
        expected_trigger = (
            "  push:\n"
            "    branches: [main]\n"
            "    paths:\n"
            "      - Cargo.toml\n"
            "      - Cargo.lock\n"
            '      - "crates/*/Cargo.toml"\n'
            '      - "examples/rust/*/Cargo.toml"\n'
            "      - rust-toolchain.toml\n"
            '      - ".cargo/**"\n'
            "      - .github/workflows/ci.yml\n"
        )
        push_trigger = self.workflow.split("on:\n", 1)[1].split(
            "  workflow_dispatch:\n", 1
        )[0]

        self.assertEqual(push_trigger, expected_trigger)

    def test_rust_jobs_run_for_main_seed_or_selected_dispatch(self) -> None:
        condition = (
            "if: ${{ github.event_name == 'push' || "
            "(github.event_name == 'workflow_dispatch' && inputs.run_rust) }}"
        )

        self.assertEqual(self.workflow.count(condition), 7)
        self.assertNotIn("if: ${{ inputs.run_rust }}", self.workflow)

    def test_temporary_tag_caches_are_restore_only(self) -> None:
        cache_step = (
            "      - uses: Swatinem/rust-cache@v2\n"
            "        with:\n"
            "          cache-bin: false\n"
            "          cache-targets: true\n"
            "          save-if: ${{ github.ref == 'refs/heads/main' }}\n"
        )

        self.assertEqual(
            self.workflow.count("      - uses: Swatinem/rust-cache@v2\n"), 6
        )
        self.assertEqual(self.workflow.count(cache_step), 6)


class PremergeCITest(unittest.TestCase):
    def run_script(
        self,
        *arguments: str,
        remote_head: str = HEAD,
        existing_run_state: str = "",
        branch_advanced: bool = False,
        dirty_state: str = "",
        base_available: bool = True,
        changed_files: tuple[str, ...] = (
            "crates/uqa-engine/src/lib.rs",
            "crates/uqa-node/src/lib.rs",
            "crates/uqa-python/src/lib.rs",
        ),
    ) -> tuple[subprocess.CompletedProcess[str], list[str], list[str]]:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = pathlib.Path(temporary)
            bin_path = temporary_path / "bin"
            bin_path.mkdir()
            gh_log_path = temporary_path / "gh.log"
            git_log_path = temporary_path / "git.log"
            fetch_marker_path = temporary_path / "base-fetched"
            self.write_executable(
                bin_path / "git",
                """
                #!/usr/bin/env bash
                set -euo pipefail
                case "$1" in
                  symbolic-ref)
                    echo "fix/premerge-ci"
                    ;;
                  diff)
                    if [[ "$UQA_TEST_DIRTY_STATE" == "working" && "$*" == "diff --quiet" ]]; then
                      exit 1
                    fi
                    if [[ "$UQA_TEST_DIRTY_STATE" == "cached" && "$*" == "diff --cached --quiet" ]]; then
                      exit 1
                    fi
                    if [[ "$*" == *"--name-only"* ]]; then
                      printf '%s\n' "$UQA_TEST_CHANGED_FILES"
                    fi
                    exit 0
                    ;;
                  cat-file)
                    if [[ "$UQA_TEST_BASE_AVAILABLE" == "1" || -e "$UQA_TEST_FETCH_MARKER" ]]; then
                      exit 0
                    fi
                    exit 1
                    ;;
                  fetch)
                    echo "$*" >> "$UQA_TEST_GIT_LOG"
                    : > "$UQA_TEST_FETCH_MARKER"
                    ;;
                  rev-parse)
                    echo "$UQA_TEST_LOCAL_HEAD"
                    ;;
                  push)
                    echo "$*" >> "$UQA_TEST_GIT_LOG"
                    ;;
                  *)
                    echo "unexpected git invocation: $*" >&2
                    exit 2
                    ;;
                esac
                """,
            )
            self.write_executable(
                bin_path / "gh",
                """
                #!/usr/bin/env bash
                set -euo pipefail
                if [[ "$1 $2" == "pr view" ]]; then
                  printf 'OPEN\tfalse\t14\tmain\t%s\tfix/premerge-ci\t%s\thttps://example.test/pr/14\n' \
                    "$UQA_TEST_BASE" "$UQA_TEST_REMOTE_HEAD"
                elif [[ "$1 $2" == "run list" ]]; then
                  [[ "$*" == *"--json url,status,conclusion"* ]] || exit 4
                  case "$UQA_TEST_EXISTING_RUN_STATE" in
                    success|in_progress) echo "https://example.test/run/1" ;;
                    failure_then_success)
                      if [[ "$*" == *"--limit 100"* && "$*" == *"[.[] | select"* ]]; then
                        echo "https://example.test/run/older-success"
                      fi
                      ;;
                  esac
                elif [[ "$1 $2" == "workflow run" ]]; then
                  if [[ "$UQA_TEST_BRANCH_ADVANCED" == "1" && "$*" == *"--ref fix/premerge-ci"* ]]; then
                    echo "mutable branch ref used after branch advance" >&2
                    exit 3
                  fi
                  echo "$*" >> "$UQA_TEST_GH_LOG"
                else
                  echo "unexpected gh invocation: $*" >&2
                  exit 2
                fi
                """,
            )
            environment = os.environ.copy()
            environment.update(
                {
                    "PATH": f"{bin_path}:{environment['PATH']}",
                    "UQA_TEST_BASE": BASE,
                    "UQA_TEST_BASE_AVAILABLE": "1" if base_available else "0",
                    "UQA_TEST_BRANCH_ADVANCED": "1" if branch_advanced else "0",
                    "UQA_TEST_CHANGED_FILES": "\n".join(changed_files),
                    "UQA_TEST_DIRTY_STATE": dirty_state,
                    "UQA_TEST_EXISTING_RUN_STATE": existing_run_state,
                    "UQA_TEST_FETCH_MARKER": str(fetch_marker_path),
                    "UQA_TEST_GH_LOG": str(gh_log_path),
                    "UQA_TEST_GIT_LOG": str(git_log_path),
                    "UQA_TEST_LOCAL_HEAD": HEAD,
                    "UQA_TEST_REMOTE_HEAD": remote_head,
                }
            )
            result = subprocess.run(
                ["bash", str(SCRIPT), *arguments],
                cwd=ROOT,
                env=environment,
                capture_output=True,
                text=True,
                check=False,
            )
            gh_invocations = (
                gh_log_path.read_text(encoding="utf-8").splitlines()
                if gh_log_path.exists()
                else []
            )
            git_invocations = (
                git_log_path.read_text(encoding="utf-8").splitlines()
                if git_log_path.exists()
                else []
            )
            return result, gh_invocations, git_invocations

    @staticmethod
    def write_executable(path: pathlib.Path, body: str) -> None:
        path.write_text(textwrap.dedent(body).lstrip(), encoding="utf-8")
        path.chmod(0o755)

    def test_dispatches_all_suites_for_exact_pull_request_head(self) -> None:
        result, gh_invocations, git_invocations = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(git_invocations), 2)
        create_ref = git_invocations[0].split()[2]
        self.assertTrue(
            create_ref.startswith(
                f"{HEAD}:refs/tags/uqa-premerge/pr-14-{HEAD[:12]}-"
            ),
            create_ref,
        )
        tag = create_ref.split(":refs/tags/", 1)[1]
        self.assertEqual(git_invocations[1], f"push origin :refs/tags/{tag}")
        self.assertEqual(
            gh_invocations,
            [
                f"workflow run ci.yml --ref {tag} -f run_rust=true",
                f"workflow run javascript-bindings.yml --ref {tag}",
                f"workflow run python-wheels.yml --ref {tag}",
            ],
        )

    def test_rust_change_skips_binding_suites(self) -> None:
        result, gh_invocations, git_invocations = self.run_script(
            changed_files=("crates/uqa-engine/src/lib.rs",)
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(gh_invocations), 1)
        self.assertIn("workflow run ci.yml", gh_invocations[0])
        self.assertIn("-f run_rust=true", gh_invocations[0])
        self.assertEqual(len(git_invocations), 2)

    def test_prose_change_skips_expensive_suites(self) -> None:
        result, gh_invocations, git_invocations = self.run_script(
            changed_files=("CONTRIBUTING.md",)
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(gh_invocations), 1)
        self.assertIn("workflow run ci.yml", gh_invocations[0])
        self.assertIn("-f run_rust=false", gh_invocations[0])
        self.assertEqual(len(git_invocations), 2)

    def test_each_continued_pattern_selects_its_suite(self) -> None:
        cases = (
            ("benchmarks/tpch/README.md", ("ci.yml",), "true"),
            ("examples/python/basic.py", ("ci.yml", "python-wheels.yml"), "false"),
            (
                "examples/node/basic.mjs",
                ("ci.yml", "javascript-bindings.yml"),
                "false",
            ),
            (".github/workflows/ci.yml", ("ci.yml",), "true"),
        )

        for changed_file, expected_workflows, run_rust in cases:
            with self.subTest(changed_file=changed_file):
                result, gh_invocations, git_invocations = self.run_script(
                    changed_files=(changed_file,)
                )

                self.assertEqual(result.returncode, 0, result.stderr)
                workflows = tuple(invocation.split()[2] for invocation in gh_invocations)
                self.assertEqual(workflows, expected_workflows)
                self.assertIn(f"-f run_rust={run_rust}", gh_invocations[0])
                self.assertEqual(len(git_invocations), 2)

    def test_fetches_a_missing_base_commit_before_classification(self) -> None:
        result, _, git_invocations = self.run_script(base_available=False)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            git_invocations[0], f"fetch --quiet origin {BASE}"
        )

    def test_rejects_dirty_tracked_state(self) -> None:
        for dirty_state in ("working", "cached"):
            with self.subTest(dirty_state=dirty_state):
                result, gh_invocations, git_invocations = self.run_script(
                    dirty_state=dirty_state
                )

                self.assertEqual(result.returncode, 1)
                self.assertIn("commit tracked changes", result.stderr)
                self.assertEqual(gh_invocations, [])
                self.assertEqual(git_invocations, [])

    def test_rejects_a_local_head_that_is_not_the_pull_request_head(self) -> None:
        result, gh_invocations, git_invocations = self.run_script(remote_head="c" * 40)

        self.assertEqual(result.returncode, 1)
        self.assertIn("does not match remote pull-request HEAD", result.stderr)
        self.assertEqual(gh_invocations, [])
        self.assertEqual(git_invocations, [])

    def test_does_not_duplicate_existing_runs_without_force(self) -> None:
        result, gh_invocations, git_invocations = self.run_script(
            existing_run_state="success"
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(gh_invocations, [])
        self.assertEqual(git_invocations, [])
        self.assertEqual(
            result.stdout.count("already has a successful or in-progress run"), 3
        )

    def test_force_repeats_existing_runs(self) -> None:
        result, gh_invocations, git_invocations = self.run_script(
            "--force", existing_run_state="success"
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(gh_invocations), 3)
        self.assertEqual(len(git_invocations), 2)

    def test_failed_runs_are_retried_without_force(self) -> None:
        result, gh_invocations, git_invocations = self.run_script(
            existing_run_state="failure"
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(gh_invocations), 3)
        self.assertEqual(len(git_invocations), 2)

    def test_newest_failure_does_not_hide_an_older_success(self) -> None:
        result, gh_invocations, git_invocations = self.run_script(
            existing_run_state="failure_then_success"
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(gh_invocations, [])
        self.assertEqual(git_invocations, [])
        self.assertEqual(
            result.stdout.count("already has a successful or in-progress run"), 3
        )

    def test_branch_advance_cannot_retarget_later_suites(self) -> None:
        result, gh_invocations, git_invocations = self.run_script(
            branch_advanced=True
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        refs = [
            invocation.split("--ref ", 1)[1].split()[0]
            for invocation in gh_invocations
        ]
        self.assertEqual(len(set(refs)), 1)
        self.assertNotEqual(refs[0], "fix/premerge-ci")
        self.assertEqual(len(git_invocations), 2)

    def test_dry_run_does_not_create_a_remote_tag(self) -> None:
        result, gh_invocations, git_invocations = self.run_script("--dry-run")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(gh_invocations, [])
        self.assertEqual(git_invocations, [])
        self.assertEqual(result.stdout.count("would dispatch"), 3)


if __name__ == "__main__":
    unittest.main()
