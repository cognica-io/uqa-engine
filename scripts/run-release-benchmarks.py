#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Run the release Criterion inventory once and retain compact, attributed reports."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "benchmarks/release/manifest.json"
LIMITATION = (
    "Informational measurements on a shared GitHub-hosted runner. Host control "
    "and an independent noise bound are not established; these results do not "
    "accept or reject performance changes or establish cross-release speedups."
)


class BenchmarkError(RuntimeError):
    pass


def read_json(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise BenchmarkError(f"cannot read {path}: {error}") from error


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_manifest() -> dict:
    manifest = read_json(MANIFEST)
    if manifest.get("schema_version") != 1 or not manifest.get("suites"):
        raise BenchmarkError("unsupported or empty release benchmark manifest")
    names = set()
    for suite in manifest["suites"]:
        name = suite["name"]
        target = suite["target"]
        cases = suite["cases"]
        if name in names or not re.fullmatch(r"[a-z][a-z0-9_]*", name) or not re.fullmatch(r"[a-z][a-z0-9_]*", target):
            raise BenchmarkError(f"invalid or repeated suite: {name}")
        if not cases or len(cases) != len(set(cases)):
            raise BenchmarkError(f"empty or repeated benchmark inventory: {target}")
        names.add(name)
    return manifest


def positive(value: object, label: str, *, zero: bool = False) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise BenchmarkError(f"{label} is not a number")
    if not math.isfinite(value) or (value < 0 if zero else value <= 0):
        raise BenchmarkError(f"{label} is not a finite {'nonnegative' if zero else 'positive'} number")
    return value


def estimate(value: dict) -> dict:
    point = positive(value["point_estimate"], "point estimate")
    error = positive(value["standard_error"], "standard error", zero=True)
    interval = value["confidence_interval"]
    low = positive(interval["lower_bound"], "lower bound", zero=True)
    high = positive(interval["upper_bound"], "upper bound")
    confidence = positive(interval["confidence_level"], "confidence level")
    if not low <= point <= high or confidence >= 1:
        raise BenchmarkError("invalid estimate confidence interval")
    return {"point_estimate": point, "standard_error": error,
            "confidence_interval": {"lower_bound": low, "upper_bound": high,
                                    "confidence_level": confidence}}


def collect_results(directory: Path, expected: list[str]) -> list[dict]:
    results = {}
    for path in sorted(directory.glob("**/new/benchmark.json")):
        try:
            identifier = read_json(path)["full_id"]
            if identifier in results:
                raise BenchmarkError(f"duplicate benchmark: {identifier}")
            estimates = read_json(path.with_name("estimates.json"))
            sample = read_json(path.with_name("sample.json"))
            iterations, times = sample["iters"], sample["times"]
            if len(iterations) != len(times) or len(times) < 10:
                raise BenchmarkError(f"incomplete samples: {identifier}")
            for value in iterations + times:
                positive(value, f"sample for {identifier}")
            results[identifier] = {
                "name": identifier, "unit": "ns", "samples": len(times),
                "mean": estimate(estimates["mean"]),
                "median": estimate(estimates["median"]),
            }
        except (KeyError, TypeError, ValueError) as error:
            raise BenchmarkError(f"invalid Criterion result {path}: {error}") from error
    if set(results) != set(expected):
        missing = sorted(set(expected) - results.keys())
        unexpected = sorted(results.keys() - set(expected))
        raise BenchmarkError(f"benchmark inventory differs: missing={missing}, unexpected={unexpected}")
    return [results[name] for name in expected]


def command_text(command: list[str]) -> str:
    return subprocess.check_output(command, cwd=ROOT, text=True).strip()


def provenance(tag: str, manifest: dict) -> dict:
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?", tag):
        raise BenchmarkError("expected a release version tag")
    commit = command_text(["git", "rev-parse", "HEAD"])
    if command_text(["git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"]) != commit:
        raise BenchmarkError("checkout does not match the release tag")
    if command_text(["git", "status", "--porcelain", "--untracked-files=no"]):
        raise BenchmarkError("release sources have tracked changes")
    repository = os.environ.get("GITHUB_REPOSITORY", "cognica-io/uqa-engine")
    run_id = os.environ.get("GITHUB_RUN_ID", "local")
    attempt = os.environ.get("GITHUB_RUN_ATTEMPT", "1")
    if not re.fullmatch(r"[0-9]+|local", run_id) or not attempt.isdigit():
        raise BenchmarkError("invalid workflow run identity")
    cpu_info = Path("/proc/cpuinfo")
    cpu = platform.processor()
    if cpu_info.exists():
        match = re.search(r"^model name\s*:\s*(.+)$", cpu_info.read_text(), re.M)
        cpu = match.group(1) if match else cpu
    return {
        "tag": tag, "commit": commit, "repository": repository,
        "run_id": run_id, "run_attempt": attempt,
        "run_url": f"https://github.com/{repository}/actions/runs/{run_id}",
        "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "rustc": command_text(["rustc", "-vV"]),
        "cargo": command_text(["cargo", "--version"]),
        "cargo_lock_sha256": sha256(ROOT / "Cargo.lock"),
        "cargo_manifest_sha256": sha256(ROOT / "Cargo.toml"),
        "benchmark_manifest_sha256": sha256(MANIFEST),
        "profile": "bench", "features": manifest["features"],
        "build_environment": {key: os.environ.get(key) for key in (
            "RUSTFLAGS", "CARGO_INCREMENTAL", "CARGO_PROFILE_BENCH_DEBUG")},
        "runner": {
            "os": platform.platform(), "architecture": platform.machine(),
            "cpu": cpu, "logical_cpus": os.cpu_count(),
            "image_os": os.environ.get("ImageOS"),
            "image_version": os.environ.get("ImageVersion"),
        },
        "performance_acceptance": False, "limitations": LIMITATION,
    }


def run_command(command: list[str], log: Path, *, env: dict, timeout: int) -> None:
    with log.open("w", encoding="utf-8") as stream:
        process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=stream,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            raise BenchmarkError(f"command timed out; see {log.name}") from error
    if code:
        raise BenchmarkError(f"command exited with {code}; see {log.name}")


def build_command(manifest: dict) -> list[str]:
    command = ["cargo", "bench", "--locked", "--no-run", "--message-format=json-render-diagnostics",
               "-p", manifest["package"]]
    if manifest["features"]:
        command += ["--features", ",".join(manifest["features"])]
    for target in dict.fromkeys(suite["target"] for suite in manifest["suites"]):
        command += ["--bench", target]
    return command


def executables(log: Path, manifest: dict) -> dict[str, Path]:
    found = {}
    wanted = {suite["target"] for suite in manifest["suites"]}
    for line in log.read_text(encoding="utf-8").splitlines():
        try:
            message = json.loads(line)
        except ValueError:
            continue
        if message.get("reason") != "compiler-artifact" or not message.get("executable"):
            continue
        target = message["target"]
        if target["name"] in wanted and "bench" in target["kind"]:
            found[target["name"]] = Path(message["executable"])
    if found.keys() != wanted:
        raise BenchmarkError("Cargo did not produce every requested benchmark executable")
    return found


def markdown(report: dict) -> str:
    source = report["provenance"]
    lines = [f"# Release benchmarks: {source['tag']}", "",
             f"Status: **{report['status']}**. Commit: `{source['commit']}`. "
             f"[Workflow run, attempt {source['run_attempt']}]({source['run_url']}).", "",
             LIMITATION, "",
             f"Build: Cargo `{source['profile']}` profile; features: "
             f"`{','.join(source['features']) or 'default'}`. Runner: "
             f"{source['runner']['os']}, {source['runner']['cpu']}, "
             f"{source['runner']['logical_cpus']} logical CPUs.", "",
             "Existing benchmark fixture assertions run as part of each suite. "
             "A complete measurement inventory does not replace the product correctness suites."]
    for suite in report["suites"]:
        lines += ["", f"## {suite['name']}", "", f"Status: {suite['status']}."]
        if suite.get("error"):
            lines += ["", suite["error"]]
        if suite.get("results"):
            lines += ["", "| Benchmark | Samples | Mean (ns) | Median (ns) | Median confidence interval (ns) |",
                      "| --- | ---: | ---: | ---: | ---: |"]
            for result in suite["results"]:
                interval = result["median"]["confidence_interval"]
                lines.append(f"| `{result['name']}` | {result['samples']} | "
                             f"{result['mean']['point_estimate']:.3f} | "
                             f"{result['median']['point_estimate']:.3f} | "
                             f"{interval['lower_bound']:.3f}–{interval['upper_bound']:.3f} "
                             f"({interval['confidence_level']:.0%}) |")
    if report.get("error"):
        lines += ["", "## Execution error", "", report["error"]]
    return "\n".join(lines) + "\n"


def write_report(report: dict, output: Path) -> None:
    source = report["provenance"]
    name = f"release-benchmarks-{source['tag']}-{source['run_id']}-{source['run_attempt']}"
    reports = output / "reports"
    reports.mkdir(exist_ok=True)
    payloads = {"json": json.dumps(report, indent=2, allow_nan=False) + "\n",
                "md": markdown(report)}
    for suffix, payload in payloads.items():
        path = reports / f"{name}.{suffix}"
        temporary = path.with_name(path.name + ".tmp")
        temporary.write_text(payload, encoding="utf-8")
        temporary.replace(path)


def run(tag: str, output: Path) -> int:
    manifest = load_manifest()
    source = provenance(tag, manifest)
    output.mkdir(parents=True, exist_ok=False)
    report = {"schema_version": 1, "status": "incomplete", "provenance": source,
              "build_command": build_command(manifest),
              "criterion_args": manifest["criterion_args"],
              "suites": [{"name": suite["name"], "target": suite["target"], "status": "not_run", "env": suite["env"],
                          "expected_cases": suite["cases"]} for suite in manifest["suites"]]}
    write_report(report, output)
    try:
        log = output / "build.log"
        print("Building release benchmark executables once", flush=True)
        run_command(report["build_command"], log, env=dict(os.environ), timeout=1800)
        binaries = executables(log, manifest)
        for suite, result in zip(manifest["suites"], report["suites"]):
            name = suite["name"]
            target = suite["target"]
            print(f"Measuring {name} once", flush=True)
            result["status"] = "incomplete"
            result["executable_sha256"] = sha256(binaries[target])
            write_report(report, output)
            criterion_home = output / "criterion" / name
            environment = {**os.environ, **suite["env"], "CRITERION_HOME": str(criterion_home)}
            try:
                run_command([str(binaries[target]), *manifest["criterion_args"]],
                            output / f"{name}.log", env=environment, timeout=600)
                result["results"] = collect_results(criterion_home, suite["cases"])
                result["status"] = "complete"
            except (BenchmarkError, OSError) as error:
                result["status"] = "failed"
                result["error"] = str(error)
            write_report(report, output)
        report["status"] = "complete" if all(s["status"] == "complete" for s in report["suites"]) else "failed"
    except (BenchmarkError, OSError, subprocess.SubprocessError) as error:
        report["status"] = "failed"
        report["error"] = str(error)
    report["finished_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
    write_report(report, output)
    print(f"Release benchmark report: {report['status']}; {output / 'reports'}", flush=True)
    return 0 if report["status"] == "complete" else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        return run(args.tag, args.output.resolve())
    except (BenchmarkError, OSError, subprocess.SubprocessError) as error:
        print(f"release benchmarks: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
