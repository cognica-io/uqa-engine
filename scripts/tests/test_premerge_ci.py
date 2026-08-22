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
HEAD = "a" * 40
BASE = "b" * 40


class PremergeCITest(unittest.TestCase):
    def run_script(
        self,
        *arguments: str,
        remote_head: str = HEAD,
        existing_run: bool = False,
    ) -> tuple[subprocess.CompletedProcess[str], list[str]]:
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = pathlib.Path(temporary)
            bin_path = temporary_path / "bin"
            bin_path.mkdir()
            log_path = temporary_path / "gh.log"
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
                    exit 0
                    ;;
                  rev-parse)
                    echo "$UQA_TEST_LOCAL_HEAD"
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
                  printf 'OPEN\tfalse\tmain\t%s\tfix/premerge-ci\t%s\thttps://example.test/pr/1\n' \
                    "$UQA_TEST_BASE" "$UQA_TEST_REMOTE_HEAD"
                elif [[ "$1 $2" == "run list" ]]; then
                  if [[ "$UQA_TEST_EXISTING_RUN" == "1" ]]; then
                    echo "https://example.test/run/1"
                  fi
                elif [[ "$1 $2" == "workflow run" ]]; then
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
                    "UQA_TEST_EXISTING_RUN": "1" if existing_run else "0",
                    "UQA_TEST_GH_LOG": str(log_path),
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
            invocations = (
                log_path.read_text(encoding="utf-8").splitlines()
                if log_path.exists()
                else []
            )
            return result, invocations

    @staticmethod
    def write_executable(path: pathlib.Path, body: str) -> None:
        path.write_text(textwrap.dedent(body).lstrip(), encoding="utf-8")
        path.chmod(0o755)

    def test_dispatches_all_suites_for_exact_pull_request_head(self) -> None:
        result, invocations = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            invocations,
            [
                f"workflow run ci.yml --ref fix/premerge-ci -f base_revision={BASE}",
                "workflow run javascript-bindings.yml --ref fix/premerge-ci",
                "workflow run python-wheels.yml --ref fix/premerge-ci",
            ],
        )

    def test_rejects_a_local_head_that_is_not_the_pull_request_head(self) -> None:
        result, invocations = self.run_script(remote_head="c" * 40)

        self.assertEqual(result.returncode, 1)
        self.assertIn("does not match remote pull-request HEAD", result.stderr)
        self.assertEqual(invocations, [])

    def test_does_not_duplicate_existing_runs_without_force(self) -> None:
        result, invocations = self.run_script(existing_run=True)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(invocations, [])
        self.assertEqual(result.stdout.count("already has a run"), 3)

    def test_force_repeats_existing_runs(self) -> None:
        result, invocations = self.run_script("--force", existing_run=True)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(invocations), 3)


if __name__ == "__main__":
    unittest.main()
