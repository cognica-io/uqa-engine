#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Run the reviewed deterministic regression inventory, rejecting missing tests."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import signal
import subprocess
import sys
import tempfile


ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "benchmarks/regressions/manifest.json"


def inventory(path: pathlib.Path = MANIFEST) -> list[dict]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema_version") != 1 or not document.get("checks"):
        raise ValueError("unsupported or empty performance inventory")
    checks = document["checks"]
    identities = set()
    for check in checks:
        package, test = check["package"], check["test"]
        if not re.fullmatch(r"uqa-[a-z0-9-]+", package) or not re.fullmatch(
            r"[a-z][a-z0-9_]*", test
        ):
            raise ValueError("invalid package or test selector")
        if check["kind"] not in {"lib", "test"}:
            raise ValueError("only the existing library and integration harnesses are allowed")
        if type(check["cases"]) is not int or check["cases"] < 1:
            raise ValueError("each check needs a positive exact case count")
        names = check.get("case_names")
        if (
            not isinstance(names, list)
            or len(names) != check["cases"]
            or any(not isinstance(name, str) or not re.fullmatch(r"(?:[A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)*)?", name) for name in names)
            or len(set(names)) != len(names)
        ):
            raise ValueError("each check needs every distinct expected case name")
        identity = (package, check["kind"], test)
        if identity in identities:
            raise ValueError(f"duplicate check: {identity}")
        identities.add(identity)
        source = pathlib.PurePosixPath(check["source"])
        if source.is_absolute() or ".." in source.parts or source.parts[:2] != (
            "crates", package
        ):
            raise ValueError("test source must belong to the selected crate")
        text = (ROOT / source).read_text(encoding="utf-8")
        if not re.search(r"\bfn " + re.escape(test) + r"\s*\(", text):
            raise ValueError(f"missing test definition: {source}: {test}")
        for field in ("capability", "workload", "invariant"):
            if not isinstance(check.get(field), str) or not check[field].strip():
                raise ValueError(f"check has no {field}")
    return checks


def selector(check: dict) -> str:
    return (
        f"(package(={check['package']}) & kind({check['kind']}) & "
        f"test(/(^|::){check['test']}(::.*)?$/))"
    )


def build_arguments(checks: list[dict]) -> list[str]:
    packages = sorted({check["package"] for check in checks})
    return ["--locked", "--lib", "--tests"] + [
        argument for package in packages for argument in ("-p", package)
    ]


def arguments(checks: list[dict], archive: pathlib.Path | None = None) -> list[str]:
    source = build_arguments(checks) if archive is None else [
        "--archive-file", str(archive.resolve()), "--extract-to", str(ROOT),
        "--extract-overwrite", "--workspace-remap", str(ROOT),
    ]
    return ["--profile", "ci", *source, "-E", " | ".join(selector(check) for check in checks)]


def verify_selection(checks: list[dict], listing: dict) -> list[dict]:
    selected = set()
    results = []
    for check in checks:
        pattern = re.compile(r"(^|::)" + re.escape(check["test"]) + r"(::.*)?$")
        matches = []
        for suite_id, suite in listing["rust-suites"].items():
            if suite["package-name"] != check["package"] or suite["kind"] != check["kind"]:
                continue
            for name, case in suite.get("testcases", {}).items():
                if not pattern.search(name):
                    continue
                if case["ignored"] or case["filter-match"]["status"] != "matches":
                    raise ValueError(f"required test is ignored or filtered out: {name}")
                identity = (suite_id, name)
                if identity in selected:
                    raise ValueError(f"test belongs to overlapping checks: {name}")
                selected.add(identity)
                matches.append(name)
        if len(matches) != check["cases"]:
            raise ValueError(
                f"{check['package']}::{check['test']}: expected {check['cases']} "
                f"cases, found {len(matches)}"
            )
        expected_names = {check["test"] + ("::" + name if name else "") for name in check["case_names"]}
        actual_names = {pattern.search(name).group().lstrip(":") for name in matches}
        if actual_names != expected_names:
            raise ValueError(
                f"{check['package']}::{check['test']}: case identities differ; "
                f"missing {sorted(expected_names - actual_names)}, "
                f"unexpected {sorted(actual_names - expected_names)}"
            )
        results.append({
            **check,
            "selected_cases": len(matches),
            "source_sha256": hashlib.sha256((ROOT / check["source"]).read_bytes()).hexdigest(),
        })
    actual = {
        (suite_id, name)
        for suite_id, suite in listing["rust-suites"].items()
        for name, case in suite.get("testcases", {}).items()
        if case["filter-match"]["status"] == "matches"
    }
    if actual != selected:
        raise ValueError("nextest selected tests outside the reviewed inventory")
    return results


def report_process_group(group: int) -> None:
    """Report only names and states of this invocation's children, never arguments or environment."""
    if os.name != "posix":
        return
    try:
        snapshot = subprocess.check_output(
            ["ps", "-axo", "pid=,ppid=,pgid=,stat=,comm="],
            text=True, stderr=subprocess.DEVNULL, timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return
    for line in snapshot.splitlines():
        fields = line.split(None, 4)
        if len(fields) == 5 and fields[2] == str(group):
            print(f"discovery process {fields[0]} parent {fields[1]} state {fields[3]} executable {pathlib.Path(fields[4]).name}", file=sys.stderr, flush=True)


def capture(*command: str, timeout: float | None = None) -> str:
    if timeout is None:
        return subprocess.check_output(command, cwd=ROOT, text=True).strip()
    # A file prevents a descendant retaining stdout from extending the deadline.
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as output:
        process = subprocess.Popen(command, cwd=ROOT, stdout=output, start_new_session=os.name == "posix")
        try:
            process.wait(timeout=timeout)
        except BaseException as error:
            if isinstance(error, subprocess.TimeoutExpired):
                report_process_group(process.pid)
            try:
                if os.name == "posix":
                    os.killpg(process.pid, signal.SIGKILL)
                else:
                    process.kill()
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
            if isinstance(error, subprocess.TimeoutExpired):
                raise subprocess.TimeoutExpired(command[:3], timeout) from error
            raise
        output.seek(0)
        text = output.read().strip()
        if process.returncode:
            raise subprocess.CalledProcessError(process.returncode, command, output=text)
        return text


def report(output: pathlib.Path, result: dict) -> None:
    output.mkdir(parents=True, exist_ok=True)
    (output / "summary.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    rows = [
        "## Deterministic performance checks", "",
        f"Result: **{result['status']}**. Revision: `{result['revision']}`.", "",
        "Work and resource bounds are assertions in the referenced tests. Wall time is not an acceptance measurement.",
        "", "| Capability | Check | Selected / required cases |", "| --- | --- | --- |",
    ]
    rows.extend(
        f"| {check['capability']} | `{check['test']}` | {check['selected_cases']} / {check['cases']} |"
        for check in result["checks"]
    )
    if result.get("error"):
        rows.extend(["", "```text", result["error"], "```"])
    markdown = "\n".join(rows) + "\n"
    (output / "summary.md").write_text(markdown, encoding="utf-8")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a", encoding="utf-8") as stream:
            stream.write(markdown)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path, default=ROOT / "target/performance-checks")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="validate inventory without building or running")
    mode.add_argument("--build-archive", type=pathlib.Path, help="compile the inventory's test targets without executing them")
    mode.add_argument("--archive-file", type=pathlib.Path, help="verify and run an already compiled nextest archive")
    args = parser.parse_args()
    checks = inventory()
    if args.check:
        print(f"{len(checks)} deterministic checks, {sum(check['cases'] for check in checks)} required cases")
        return 0
    if args.build_archive is not None:
        subprocess.run([
            "cargo", "nextest", "archive", "--profile", "ci", *build_arguments(checks),
            "--archive-file", str(args.build_archive),
        ], cwd=ROOT, check=True)
        return 0
    result = {
        "schema_version": 1,
        "revision": capture("git", "rev-parse", "HEAD"),
        "manifest_sha256": hashlib.sha256(MANIFEST.read_bytes()).hexdigest(),
        "runner": {"system": platform.system(), "machine": platform.machine()},
        "rustc": capture("rustc", "--version"),
        "nextest": capture("cargo", "nextest", "--version"),
        "run_id": os.environ.get("GITHUB_RUN_ID"),
        "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
        "timing_acceptance": False,
        "status": "failed", "stage": "discovery", "checks": [],
    }
    try:
        selection = arguments(checks, args.archive_file)
        # A build/selection failure must never publish JUnit from an earlier run.
        (ROOT / "target/nextest/ci/junit.xml").unlink(missing_ok=True)
        discovered = args.output / "test-inventory.json"
        discovered.unlink(missing_ok=True)
        print("Discovering the exact regression inventory", file=sys.stderr, flush=True)
        listing_text = capture(
            "cargo", "nextest", "list", "--message-format", "json", *selection,
            timeout=120 if args.archive_file is not None else None,
        )
        args.output.mkdir(parents=True, exist_ok=True)
        discovered.write_text(listing_text + "\n", encoding="utf-8")
        result["stage"] = "selection"
        print("Validating the discovered case identities", file=sys.stderr, flush=True)
        listing = json.loads(listing_text)
        result["checks"] = verify_selection(checks, listing)
        result["stage"] = "execution"
        print(f"Executing {sum(check['selected_cases'] for check in result['checks'])} verified regression cases", file=sys.stderr, flush=True)
        subprocess.run(["cargo", "nextest", "run", *selection], cwd=ROOT, check=True)
        result["status"] = "passed"
        result["stage"] = "completed"
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        result["error"] = str(error)
        print(error, file=sys.stderr)
    finally:
        report(args.output, result)
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
