#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Recover original release artifacts without accepting failed product validation."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import subprocess


TARGETS = (
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin", "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc",
)
PLATFORMS = ("linux x86_64", "linux aarch64", "macos x86_64", "macos aarch64", "windows x86_64", "windows aarch64")
BUILD_JOBS = {
    "resolve tag", "python / source distribution", "python / Python bindings",
    "python / minimum supported python", "javascript packages / wasm package",
    "javascript packages / node npm packages", "release benchmarks / record release benchmarks",
    *("python / " + platform for platform in PLATFORMS),
    *("javascript packages / node " + platform for platform in PLATFORMS),
}
PACKAGE_ARTIFACTS = {
    "python-sdist", "node-packages", "wasm-package",
    *("python-wheel-" + target for target in TARGETS),
    *("node-addon-" + target for target in TARGETS),
}

WHEEL_RUNNERS = ("ubuntu-24.04", "ubuntu-24.04-arm", "macos-15-intel", "macos-14", "windows-latest", "windows-11-arm")
WHEEL_MATRIX = {
    "python / " + label: {"label": label, "target": target, "os": runner,
                          "manylinux": "2_28" if label.startswith("linux") else "auto"}
    for label, target, runner in zip(PLATFORMS, TARGETS, WHEEL_RUNNERS)
}
WHEEL_CHECKS = {"Build wheel", "Check wheel license contents", "Assert the wheel targets the stable ABI", "Test wheel"}
UPLOAD_STEPS = {"Retain native Nori diagnostic evidence", "Retain verified wheel", "Run actions/upload-artifact@v4"}


def transport_only_wheel_failure(job: dict) -> bool:
    steps = job.get("steps", [])
    passed = {step["name"] for step in steps if step["conclusion"] == "success"}
    failures = {step["name"] for step in steps if step["conclusion"] == "failure"}
    return (
        job["conclusion"] == "failure" and WHEEL_CHECKS <= passed
        and bool(failures) and failures <= UPLOAD_STEPS
        and all(step["conclusion"] in {"success", "failure", "skipped"} for step in steps)
    )


def api(endpoint: str) -> dict:
    return json.loads(subprocess.check_output(["gh", "api", endpoint], text=True))


def pages(endpoint: str, key: str) -> list[dict]:
    result = []
    page = 1
    while True:
        response = api(f"{endpoint}?per_page=100&page={page}")
        result.extend(response[key])
        if len(result) >= response["total_count"]:
            return result
        if not response[key]:
            raise RuntimeError("incomplete workflow inventory")
        page += 1


def publication_complete(repository: str, tag: str, source_run_id: int) -> bool:
    result = subprocess.run(
        ["gh", "api", f"repos/{repository}/releases/tags/{tag}"],
        text=True, capture_output=True,
    )
    if result.returncode:
        # Only an absent release permits recovery; authentication and service
        # failures must not be mistaken for unpublished packages.
        try:
            status = json.loads(result.stdout).get("status")
        except (ValueError, AttributeError):
            status = None
        if str(status) == "404":
            return False
        raise RuntimeError(f"Cannot check release publication: {result.stderr.strip()}")
    release = json.loads(result.stdout)
    marker = re.search(
        rf"<!-- uqa-release-publication:([0-9]+):{re.escape(tag)}:{source_run_id} -->",
        release.get("body") or "",
    )
    if not marker:
        return False
    publication = api(f"repos/{repository}/actions/runs/{marker[1]}")
    return (
        publication["status"] == "completed" and publication["conclusion"] == "success"
        and publication["path"] in {".github/workflows/release.yml", ".github/workflows/release-recovery.yml"}
        and publication["head_repository"]["full_name"] == repository
    )


def validate(run: dict, tag: str, commit: str, repository: str, jobs: list[dict], artifacts: list[dict], *, allow_python_repair: bool = False) -> list[dict] | None:
    if (
        run["status"] != "completed" or run["conclusion"] != "failure"
        or run["event"] not in {"push", "workflow_dispatch"}
        or run["path"] != ".github/workflows/release.yml"
        or run["head_branch"] != tag or run["head_sha"] != commit
        or run["head_repository"]["full_name"] != repository
    ):
        raise ValueError("release origin, tag or outcome does not qualify for recovery")
    statuses = {job["name"]: job["conclusion"] for job in jobs}
    if len(statuses) != len(jobs):
        raise ValueError("duplicate build job identities")
    repairable = {
        job["name"]: WHEEL_MATRIX[job["name"]] for job in jobs
        if allow_python_repair and job["name"] in WHEEL_MATRIX and transport_only_wheel_failure(job)
    }
    deferred = set(repairable)
    minimum = "python / minimum supported python"
    if repairable and statuses.get(minimum) == "skipped":
        deferred.add(minimum)
    if any(statuses.get(name) != "success" for name in BUILD_JOBS - deferred):
        raise ValueError("every package, example and benchmark build must have passed")
    # A future additional build must not be silently omitted by this inventory.
    if any(job["conclusion"] != "success" for job in jobs if job["name"] not in deferred and job["name"].startswith(("python /", "javascript packages /", "release benchmarks /"))):
        raise ValueError("an additional release build did not pass")
    required = PACKAGE_ARTIFACTS | {f"release-benchmarks-{tag}-{run['id']}-{run['run_attempt']}"}
    available = {artifact["name"] for artifact in artifacts if artifact["expired"] is False and artifact["workflow_run"]["head_sha"] == commit and artifact["workflow_run"]["id"] == run["id"]}
    missing = required - available
    rebuild = {"python-wheel-" + item["target"]: item for item in repairable.values()}
    if missing - rebuild.keys():
        raise ValueError(f"missing, expired or wrong-source artifacts: {sorted(missing - rebuild.keys())}")
    # None means no Python repair; [] still rechecks the skipped minimum-Python job.
    return [rebuild[name] for name in sorted(missing)] if repairable else None


def verify_repaired_artifacts(directory: pathlib.Path, commit: str, tag: str, matrix: list[dict]) -> None:
    expected = {"python-wheel-" + item["target"]: item for item in matrix}
    if len(expected) != len(matrix) or any(item not in WHEEL_MATRIX.values() for item in matrix):
        raise ValueError("invalid repaired wheel matrix")
    actual = {path.name for path in directory.iterdir()}
    if actual != expected.keys():
        raise ValueError("repaired wheel inventory differs from the validated missing artifacts")
    for name, item in expected.items():
        artifact = directory / name
        provenance = json.loads((artifact / "recovery-provenance.json").read_text())
        files = list(artifact.glob("*.whl"))
        if len(files) != 1:
            raise ValueError("expected exactly one recovered wheel")
        wheel = files[0]
        if (provenance.get("commit") != commit or provenance.get("target") != item["target"]
                or provenance.get("filename") != wheel.name
                or not wheel.name.startswith(f"uqa-{tag.removeprefix('v')}-cp38-abi3-")
                or provenance.get("sha256") != hashlib.sha256(wheel.read_bytes()).hexdigest()):
            raise ValueError("recovered wheel source, version or digest mismatch")
    print(f"Verified {len(expected)} repaired wheels from original release {commit}.")


def main() -> None:
    repository = os.environ["GITHUB_REPOSITORY"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("invalid repository")
    versions = re.findall(r'^version = "([0-9]+\.[0-9]+\.[0-9]+)"$', pathlib.Path("Cargo.toml").read_text(), re.M)
    if len(versions) != 1:
        raise ValueError("expected one workspace release version")
    tag = "v" + versions[0]
    runs = api(f"repos/{repository}/actions/workflows/release.yml/runs?branch={tag}&per_page=1")["workflow_runs"]
    if not runs or runs[0]["status"] != "completed" or runs[0]["conclusion"] != "failure":
        print("No failed current-version release to recover.")
        return
    run = runs[0]
    if publication_complete(repository, tag, run["id"]):
        print(f"Publication of {tag} has already completed.")
        return
    reference = api(f"repos/{repository}/git/ref/tags/{tag}")["object"]
    for _ in range(8):
        if reference["type"] == "commit":
            break
        if reference["type"] != "tag":
            raise ValueError("release ref does not identify a commit")
        reference = api(f"repos/{repository}/git/tags/{reference['sha']}")["object"]
    commit = reference["sha"]
    if reference["type"] != "commit" or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("invalid release commit")
    jobs = pages(f"repos/{repository}/actions/runs/{run['id']}/jobs", "jobs")
    artifacts = pages(f"repos/{repository}/actions/runs/{run['id']}/artifacts", "artifacts")
    matrix = validate(run, tag, commit, repository, jobs, artifacts, allow_python_repair=True)
    print(f"Recover {tag} from run {run['id']} at {commit}; missing wheel rebuilds: {matrix}.")
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"ready=true\ntag={tag}\ncommit={commit}\nrun_id={run['id']}\n")
        output.write(f"python_repair={str(matrix is not None).lower()}\n")
        output.write("repair_matrix=" + json.dumps(matrix or [], separators=(",", ":")) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verify-repair", type=pathlib.Path)
    parser.add_argument("--commit")
    parser.add_argument("--tag")
    parser.add_argument("--matrix")
    args = parser.parse_args()
    if args.verify_repair:
        if not args.commit or not args.tag or not args.matrix:
            parser.error("repair verification requires commit, tag and matrix")
        verify_repaired_artifacts(args.verify_repair, args.commit, args.tag, json.loads(args.matrix))
    else:
        main()
