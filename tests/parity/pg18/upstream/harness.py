#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Run the unchanged PostgreSQL core and isolation corpus with its own drivers."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import uuid


HERE = Path(__file__).resolve().parent
SUITES = {
    "core": ("regress", "parallel_schedule", "sql", ".sql"),
    "isolation": ("isolation", "isolation_schedule", "specs", ".spec"),
}
CATEGORIES = {
    "unclassified", "parser", "binder", "type-system", "planner", "executor",
    "catalog", "transaction", "protocol", "administration",
}
NAME = re.compile(r"[a-zA-Z0-9_][a-zA-Z0-9_.-]*\Z")
STATUS = re.compile(r"(not )?ok\s+(\d+)\s+[-+]\s+([a-zA-Z0-9_][a-zA-Z0-9_.-]*)\s+(\d+)\s+ms\Z")


def read_json(path: Path) -> dict:
    return json.loads(path.read_text())


def write_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n")


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def parse_schedule(text: str) -> list[list[str]]:
    groups = []
    seen = set()
    for number, line in enumerate(text.splitlines(), 1):
        line = line.rstrip()
        if not line or line.startswith("#"):
            continue
        if not line.startswith("test: "):
            raise ValueError(f"unknown schedule directive at line {number}: {line}")
        names = line[6:].split()
        if not names or len(names) > 100 or any(not NAME.fullmatch(n) for n in names):
            raise ValueError(f"invalid test group at line {number}")
        if any(name in seen for name in names) or len(set(names)) != len(names):
            raise ValueError(f"duplicate scheduled test at line {number}")
        seen.update(names)
        groups.append(names)
    if not groups:
        raise ValueError("empty upstream schedule")
    return groups


def corpus_files(root: Path) -> list[Path]:
    files = [root / "COPYRIGHT"]
    for folder, _, _, _ in SUITES.values():
        files.extend(path for path in (root / "src/test" / folder).rglob("*") if path.is_file())
    if any(path.is_symlink() for path in files):
        raise ValueError("the imported corpus must not contain symbolic links")
    return sorted(files)


def inventory_for(root: Path) -> dict:
    files = {
        path.relative_to(root).as_posix(): {"sha256": digest(path), "bytes": path.stat().st_size}
        for path in corpus_files(root)
    }
    suites = {}
    for suite, (folder, schedule, inputs, extension) in SUITES.items():
        prefix = f"src/test/{folder}"
        groups = parse_schedule((root / prefix / schedule).read_text())
        positions = {name: index for index, group in enumerate(groups, 1) for name in group}
        mapped = {}
        resultmap = root / prefix / "resultmap"
        if resultmap.exists():
            for line in resultmap.read_text().splitlines():
                if not line or line.startswith("#"):
                    continue
                match = re.fullmatch(r"([^:]+):out:[^=]+=(.+)", line)
                if not match:
                    raise ValueError(f"unrecognized upstream resultmap: {line}")
                mapped.setdefault(match[1], set()).add(match[2])
        tests = {}
        for path in sorted((root / prefix / inputs).glob(f"*{extension}")):
            name = path.stem
            expected = {
                out.name for out in (root / prefix / "expected").glob("*.out")
                if re.fullmatch(re.escape(name) + r"(?:_\d+)?\.out", out.name)
            } | mapped.get(name, set())
            if f"{name}.out" not in expected:
                raise ValueError(f"missing primary expected output for {suite}/{name}")
            expected_paths = [f"{prefix}/expected/{out}" for out in sorted(expected)]
            if any(out not in files for out in expected_paths):
                raise ValueError(f"missing alternate expected output for {suite}/{name}")
            tests[name] = {
                "input": path.relative_to(root).as_posix(),
                "expected": expected_paths,
                "schedule_group": positions.get(name),
            }
        if set(positions) - set(tests):
            raise ValueError(f"scheduled input missing from {suite}")
        suites[suite] = {"schedule": f"{prefix}/{schedule}", "groups": groups, "tests": tests}
    return {"schema_version": 1, "source_sha256": read_json(HERE / "source.json")["sha256"], "files": files, "suites": suites}


def validate_inventory(inventory: dict, coverage: dict) -> None:
    pin = read_json(HERE / "source.json")
    if inventory.get("schema_version") != 1 or inventory.get("source_sha256") != pin["sha256"]:
        raise ValueError("inventory does not match the pinned source archive")
    if set(inventory["suites"]) != set(SUITES) or "COPYRIGHT" not in inventory["files"]:
        raise ValueError("inventory must include both complete suites and COPYRIGHT")
    for name, record in inventory["files"].items():
        path = PurePosixPath(name)
        if path.is_absolute() or ".." in path.parts or not re.fullmatch(r"[0-9a-f]{64}", record["sha256"]):
            raise ValueError(f"invalid corpus file: {name}")
        if type(record["bytes"]) is not int or record["bytes"] < 0:
            raise ValueError(f"invalid corpus file size: {name}")
    ids = set()
    for suite, data in inventory["suites"].items():
        if data["schedule"] not in inventory["files"]:
            raise ValueError(f"missing schedule for {suite}")
        scheduled = [name for group in data["groups"] for name in group]
        if len(set(scheduled)) != len(scheduled) or set(scheduled) - set(data["tests"]):
            raise ValueError(f"invalid scheduled test coverage for {suite}")
        positions = {name: index for index, group in enumerate(data["groups"], 1) for name in group}
        for name, test in data["tests"].items():
            if not NAME.fullmatch(name) or test["schedule_group"] != positions.get(name):
                raise ValueError(f"invalid schedule position for {suite}/{name}")
            if not test["expected"] or any(path not in inventory["files"] for path in [test["input"], *test["expected"]]):
                raise ValueError(f"missing input or output for {suite}/{name}")
            ids.add(f"{suite}/{name}")
    if coverage.get("schema_version") != 1 or set(coverage["tests"]) != ids:
        raise ValueError("burn-down coverage must account for every scheduled and extra test exactly once")
    for name, record in coverage["tests"].items():
        if not record.get("owner") or record.get("category") not in CATEGORIES:
            raise ValueError(f"missing owner or valid category for {name}")
        if record.get("status") not in {"not_audited", "failing", "verified"}:
            raise ValueError(f"invalid compatibility status for {name}")
        if record["status"] != "not_audited" and not record.get("evidence"):
            raise ValueError(f"missing observed evidence for {name}")
        if record["status"] == "verified" and set(record["evidence"]) != {"postgres", "uqa"}:
            raise ValueError(f"verification requires PostgreSQL and UQA evidence for {name}")
        if record["status"] == "verified":
            for backend, evidence in record["evidence"].items():
                if not isinstance(evidence, str) or not evidence.strip():
                    raise ValueError(f"missing report path for {backend} evidence of {name}")
                path = PurePosixPath(evidence)
                if path.is_absolute() or ".." in path.parts:
                    raise ValueError(f"evidence must be relative to the upstream directory: {name}")
                report = read_json(HERE / path)
                if report.get("backend") != backend or report.get("source") != pin or report.get("errors") != [] or report.get("tests", {}).get(name, {}).get("status") != "passed":
                    raise ValueError(f"report does not prove a {backend} pass for {name}")


def verify_corpus(root: Path) -> dict:
    inventory = read_json(HERE / "inventory.json")
    validate_inventory(inventory, read_json(HERE / "coverage.json"))
    actual = inventory_for(root)
    if actual != inventory:
        changed = sorted(name for name in set(actual["files"]) | set(inventory["files"]) if actual["files"].get(name) != inventory["files"].get(name))
        raise ValueError(f"imported corpus differs from the pinned inventory: {changed[:10]}")
    return inventory


def verify_archive(archive: Path, pin: dict) -> None:
    if archive.stat().st_size != pin["bytes"] or digest(archive) != pin["sha256"]:
        raise ValueError("PostgreSQL source archive size or SHA-256 mismatch")


def extract_corpus(archive: Path, destination: Path, pin: dict) -> Path:
    verify_archive(archive, pin)
    seen = set()
    with tarfile.open(archive, "r:bz2") as source:
        for member in source:
            parts = PurePosixPath(member.name).parts
            if not parts or parts[0] != pin["root"]:
                raise ValueError(f"unexpected archive root: {member.name}")
            relative = PurePosixPath(*parts[1:])
            selected = str(relative) == "COPYRIGHT" or relative.parts[:3] in {("src", "test", "regress"), ("src", "test", "isolation")}
            if not selected or member.isdir():
                continue
            if ".." in parts or not member.isfile() or member.name in seen:
                raise ValueError(f"unsafe or duplicate corpus archive member: {member.name}")
            seen.add(member.name)
            target = destination.joinpath(*parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            stream = source.extractfile(member)
            assert stream is not None
            with stream, target.open("xb") as output:
                shutil.copyfileobj(stream, output)
    return destination / pin["root"]


def fetch(cache: Path, archive: Path | None = None) -> Path:
    pin = read_json(HERE / "source.json")
    cache.mkdir(parents=True, exist_ok=True)
    target = cache / f"{pin['root']}.tar.bz2"
    if archive is not None:
        verify_archive(archive, pin)
        if archive.resolve() != target.resolve():
            if target.exists():
                verify_archive(target, pin)
            else:
                shutil.copyfile(archive, target)
    elif not target.exists():
        with tempfile.TemporaryDirectory(prefix="download-", dir=cache) as work:
            partial = Path(work) / target.name
            with urllib.request.urlopen(pin["url"], timeout=60) as response, partial.open("wb") as output:
                shutil.copyfileobj(response, output)
            verify_archive(partial, pin)
            partial.replace(target)
    verify_archive(target, pin)
    root = cache / pin["root"]
    if not root.exists():
        with tempfile.TemporaryDirectory(prefix="extract-", dir=cache) as work:
            extracted = extract_corpus(target, Path(work), pin)
            verify_corpus(extracted)
            extracted.rename(root)
    verify_corpus(root)
    return root


def batches(inventory: dict, suite: str) -> list[dict]:
    result = []
    for name in SUITES:
        if suite not in {"all", name}:
            continue
        data = inventory["suites"][name]
        scheduled = [test for group in data["groups"] for test in group]
        extras = sorted(set(data["tests"]) - set(scheduled))
        # Upstream documents reindex_catalog's interference with recently exited
        # sessions. Run it in its own fresh cluster, with its original SQL intact.
        separate = [test for test in extras if name == "core" and test == "reindex_catalog"]
        appended = [test for test in extras if test not in separate]
        result.append({"id": name, "suite": name, "schedule": data["schedule"], "extras": appended, "tests": scheduled + appended})
        for test in separate:
            result.append({"id": f"{name}-{test}", "suite": name, "schedule": None, "extras": [test], "tests": [test]})
    return result


def parse_tap(text: str, tests: list[str], returncode: int) -> tuple[dict, list[str]]:
    observed = {}
    numbers = []
    plans = []
    errors = []
    for line in text.splitlines():
        match = STATUS.fullmatch(line)
        if match:
            failed, number, name, elapsed = match.groups()
            if name in observed or name not in tests:
                errors.append(f"unexpected or duplicate TAP test: {name}")
            observed[name] = {"status": "failed" if failed else "passed", "milliseconds": int(elapsed)}
            numbers.append(int(number))
        elif re.fullmatch(r"1\.\.\d+", line):
            plans.append(int(line[3:]))
        elif line.startswith(("ok ", "not ok ", "Bail out!")):
            errors.append(f"invalid or aborted upstream TAP: {line}")
    missing = sorted(set(tests) - set(observed))
    if missing:
        errors.append(f"missing TAP results: {', '.join(missing)}")
    if plans != [len(tests)] or numbers != list(range(1, len(tests) + 1)):
        errors.append("incomplete or inconsistent upstream TAP plan/numbering")
    failed = sum(result["status"] == "failed" for result in observed.values())
    if returncode not in {0, 1} or (returncode == 0 and failed) or (returncode == 1 and not failed):
        errors.append(f"driver exit status {returncode} disagrees with test outcomes")
    return observed, errors


def initial_report(inventory: dict, backend: str, revision: str) -> dict:
    coverage = read_json(HERE / "coverage.json")["tests"]
    return {
        "schema_version": 1,
        "kind": "postgres_reference_validation" if backend == "postgres" else "uqa_compatibility",
        "backend": backend,
        "revision": revision,
        "revision_provenance": "pinned upstream source" if backend == "postgres" else "provided by caller",
        "source": read_json(HERE / "source.json"),
        "inventory_sha256": digest(HERE / "inventory.json"),
        "harness_sha256": digest(HERE / "harness.py"),
        "configuration_sha256": {name: digest(HERE / name) for name in ("coverage.json", "postgresql.conf")},
        "all_cases_passed": False,
        "errors": [],
        "batches": [],
        "tests": {
            f"{suite}/{name}": {
                "status": "not_run", "owner": coverage[f"{suite}/{name}"]["owner"],
                "category": coverage[f"{suite}/{name}"]["category"], "evidence": None,
            }
            for suite, data in inventory["suites"].items() for name in data["tests"]
        },
    }


def finish_report(report: dict) -> None:
    report["counts"] = {status: sum(row["status"] == status for row in report["tests"].values()) for status in ("passed", "failed", "not_run")}
    report["all_cases_passed"] = not report["errors"] and report.get("container_exit_code", 0) == 0 and all(row["status"] == "passed" for row in report["tests"].values())


def validate_report(report: dict, inventory: dict, backend: str) -> None:
    expected = initial_report(inventory, backend, "")
    for field in ("schema_version", "kind", "backend", "source", "inventory_sha256", "harness_sha256", "configuration_sha256"):
        if report.get(field) != expected[field]:
            raise ValueError(f"runner evidence {field} differs from this checkout; rebuild the image")
    if set(report.get("tests", {})) != set(expected["tests"]):
        raise ValueError("runner evidence must include every upstream test")
    if any(row.get("status") not in {"passed", "failed", "not_run"} for row in report["tests"].values()):
        raise ValueError("runner evidence has an unknown test status")


def validate_target(args: argparse.Namespace) -> None:
    if args.timeout <= 0 or not 1 <= args.port <= 65535:
        raise ValueError("timeout and port must be positive and valid")
    if args.backend == "postgres":
        if args.host or args.catalog_host or args.revision:
            raise ValueError("PostgreSQL reference runs always use fresh local clusters and the pinned revision")
    else:
        if not args.host or not args.revision or not re.fullmatch(r"[0-9a-f]{40}", args.revision):
            raise ValueError("UQA runs require an explicit disposable --host and full commit --revision")
        if args.suite in {"all", "core"} and (not args.catalog_host or args.catalog_host == args.host):
            raise ValueError("core UQA runs require a separate fresh --catalog-host for reindex_catalog")


def verify_uqa_server(psql: Path, host: str, args: argparse.Namespace) -> str:
    version = subprocess.check_output([
        str(psql), "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-d", "postgres",
        "--host", host, "--port", str(args.port), "--username", args.user,
        "-c", "SELECT current_setting('server_version')",
    ], text=True, timeout=30).strip()
    if not re.fullmatch(r"18\.[0-9.]+-uqa", version):
        raise ValueError(f"target does not identify as UQA: server_version={version!r}")
    return version


def container_run(args: argparse.Namespace) -> int:
    validate_target(args)
    source = Path("/opt") / read_json(HERE / "source.json")["root"]
    inventory = verify_corpus(source)
    report = initial_report(inventory, args.backend, args.revision or read_json(HERE / "source.json")["commit"])
    output = Path("/work")
    shutil.copyfile(source / "COPYRIGHT", output / "COPYRIGHT")
    bindir = Path("/usr/lib/postgresql/18/bin")
    testdir = Path("/usr/lib/postgresql/18/lib/pgxs/src/test")
    programs = {"psql": bindir / "psql", "postgres": bindir / "postgres", "pg_regress": testdir / "regress/pg_regress", "pg_isolation_regress": testdir / "isolation/pg_isolation_regress", "isolationtester": testdir / "isolation/isolationtester"}
    report["tools"] = {name: {"sha256": digest(path), "path": str(path)} for name, path in programs.items()}
    report["postgres_version"] = subprocess.check_output([str(programs["postgres"]), "--version"], text=True).strip()
    if not report["postgres_version"].startswith("postgres (PostgreSQL) 18.4 "):
        raise ValueError("the reference server must be the pinned PostgreSQL 18.4 build")
    if args.backend == "uqa":
        report["server_versions"] = {
            host: verify_uqa_server(programs["psql"], host, args)
            for host in dict.fromkeys([args.host] + ([args.catalog_host] if args.suite in {"all", "core"} else []))
        }
    for batch in batches(inventory, args.suite):
        name = batch["id"]
        folder = SUITES[batch["suite"]][0]
        directory = output / name
        directory.mkdir()
        driver = programs["pg_regress" if batch["suite"] == "core" else "pg_isolation_regress"]
        command = [str(driver), f"--bindir={bindir}", f"--inputdir={source / 'src/test' / folder}", f"--outputdir={directory}", "--dlpath=/usr/lib/postgresql/18/lib", "--encoding=UTF8"]
        if args.backend == "postgres":
            command += [f"--temp-instance=/tmp/{name}-instance", "--no-locale", f"--temp-config={HERE / 'postgresql.conf'}"]
        else:
            host = args.catalog_host if name == "core-reindex_catalog" else args.host
            command += [f"--host={host}", f"--port={args.port}", f"--user={args.user}"]
        if batch["schedule"]:
            command.append(f"--schedule={source / batch['schedule']}")
        command += batch["extras"]
        environment = os.environ.copy()
        # These variables can change upstream comparisons or replace the client.
        # The harness records the real programs and accepts no output overrides.
        for key in ("PG_REGRESS_DIFF_OPTS", "PG_TEST_INITDB_EXTRA_OPTS", "PG_REGRESS_SOCK_DIR", "PGOPTIONS", "PGSERVICE", "PGSERVICEFILE"):
            environment.pop(key, None)
        environment["PATH"] = f"{testdir / 'isolation'}:{bindir}:" + environment.get("PATH", "")
        started = time.monotonic()
        print(f"Running {name}: {len(batch['tests'])} upstream tests", flush=True)
        with (directory / "driver.log").open("w") as log:
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, env=environment, cwd=directory)
            try:
                returncode = process.wait(timeout=args.timeout)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                returncode = 124
        tap = (directory / "driver.log").read_text(errors="replace")
        observed, errors = parse_tap(tap, batch["tests"], returncode)
        server_logs = Path(f"/tmp/{name}-instance/log")
        if args.backend == "postgres" and server_logs.exists():
            shutil.copytree(server_logs, directory / "server-logs")
        for test in batch["tests"]:
            if test not in observed:
                continue
            row = report["tests"][f"{batch['suite']}/{test}"]
            row.update(observed[test])
            result = directory / "results" / f"{test}.out"
            if not result.is_file():
                errors.append(f"missing driver result artifact: {test}")
            row["evidence"] = {"driver_log": f"{name}/driver.log", "result": f"{name}/results/{test}.out", "result_sha256": digest(result) if result.is_file() else None, "diff": f"{name}/regression.diffs" if (directory / "regression.diffs").exists() else None}
        report["errors"].extend(f"{name}: {error}" for error in errors)
        report["batches"].append({"id": name, "command": command, "returncode": returncode, "seconds": round(time.monotonic() - started, 3), "tests": batch["tests"]})
        finish_report(report)
        write_json(output / "report.json", report)
        print(f"{name}: {sum(x['status'] == 'passed' for x in observed.values())}/{len(batch['tests'])} passed; {len(errors)} runner errors", flush=True)
        if returncode == 124:
            # A killed driver cannot guarantee its sessions/cluster are stopped.
            # Retain the evidence and end this disposable container immediately.
            break
    finish_report(report)
    write_json(output / "report.json", report)
    print(json.dumps(report["counts"]), flush=True)
    selected_passed = all(report["tests"][f"{b['suite']}/{test}"]["status"] == "passed" for b in batches(inventory, args.suite) for test in b["tests"])
    return 0 if selected_passed and not report["errors"] else 1


def build_image(args: argparse.Namespace) -> None:
    root = fetch(args.cache, args.archive)
    with tempfile.TemporaryDirectory(prefix="pg18-upstream-build-") as work:
        context = Path(work)
        shutil.copyfile(root.parent / (root.name + ".tar.bz2"), context / "postgresql-18.4.tar.bz2")
        for name in ("Dockerfile", "harness.py", "source.json", "inventory.json", "coverage.json", "regress.mk", "postgresql.conf"):
            shutil.copyfile(HERE / name, context / name)
        subprocess.run([args.docker, "build", "--tag", args.image, str(context)], check=True)


def run_image(args: argparse.Namespace) -> int:
    validate_target(args)
    if args.backend == "uqa" and not args.network:
        raise ValueError("UQA runs require a disposable --network")
    if args.backend == "postgres" and (args.host or args.network):
        raise ValueError("reference runs create isolated clusters without external networking")
    args.output.mkdir(parents=True, exist_ok=False)
    name = "uqa-pg18-upstream-" + uuid.uuid4().hex
    image_id = subprocess.check_output([args.docker, "image", "inspect", "--format={{.Id}}", args.image], text=True).strip()
    command = [args.docker, "create", "--name", name, "--network", args.network or "none", "--shm-size=256m"]
    if "PGPASSWORD" in os.environ:
        command += ["-e", "PGPASSWORD"]
    command += [image_id, "--backend", args.backend, "--suite", args.suite, "--timeout", str(args.timeout)]
    if args.backend == "uqa":
        command += ["--host", args.host, "--port", str(args.port), "--user", args.user, "--revision", args.revision]
        if args.catalog_host:
            command += ["--catalog-host", args.catalog_host]
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL)
    try:
        with (args.output / "container.log").open("w") as log:
            with subprocess.Popen([args.docker, "start", "--attach", name], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True) as process:
                assert process.stdout is not None
                for line in process.stdout:
                    log.write(line)
                    print(line, end="", flush=True)
                returncode = process.wait()
        subprocess.run([args.docker, "cp", f"{name}:/work/.", str(args.output)], check=True)
    finally:
        subprocess.run([args.docker, "rm", "--force", "--volumes", name], check=True, stdout=subprocess.DEVNULL)
    report_path = args.output / "report.json"
    if not report_path.exists():
        report = initial_report(read_json(HERE / "inventory.json"), args.backend, args.revision or read_json(HERE / "source.json")["commit"])
        report["errors"].append("container failed before producing a runner report; every test remains not_run")
        report["container_exit_code"] = returncode
        report["diagnostic_log"] = "container.log"
        report["image_id"] = image_id
        finish_report(report)
        write_json(report_path, report)
        return 2
    report = read_json(report_path)
    try:
        validate_report(report, read_json(HERE / "inventory.json"), args.backend)
    except ValueError as error:
        report.setdefault("errors", []).append(str(error))
        report["all_cases_passed"] = False
        write_json(report_path, report)
        raise
    report["image_id"] = image_id
    report["container_exit_code"] = returncode
    report["diagnostic_log"] = "container.log"
    finish_report(report)
    if returncode:
        report["all_cases_passed"] = False
    write_json(report_path, report)
    print(f"Evidence: {report_path}")
    return returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    validate = sub.add_parser("validate", help="validate the checked-in inventory and complete burn-down ledger")
    validate.add_argument("--source", type=Path, help="also check every imported byte")
    for command in ("fetch", "build"):
        child = sub.add_parser(command)
        child.add_argument("--cache", type=Path, default=Path("target/pg18-upstream"))
        child.add_argument("--archive", type=Path, help="use a downloaded archive after checksum verification")
        if command == "build":
            child.add_argument("--docker", default="docker")
            child.add_argument("--image", default="uqa-pg18-upstream:18.4")
    for command in ("run", "container-run"):
        child = sub.add_parser(command)
        child.add_argument("--backend", choices=("postgres", "uqa"), default="postgres")
        child.add_argument("--suite", choices=("all", *SUITES), default="all")
        child.add_argument("--timeout", type=int, default=1800, help="maximum seconds per official driver invocation")
        child.add_argument("--host")
        child.add_argument("--catalog-host", help="separate fresh UQA server for the catalog-reindex test")
        child.add_argument("--port", type=int, default=5432)
        child.add_argument("--user", default="postgres")
        child.add_argument("--revision")
        if command == "run":
            child.add_argument("--docker", default="docker")
            child.add_argument("--image", default="uqa-pg18-upstream:18.4")
            child.add_argument("--output", type=Path, required=True, help="new directory for complete driver artifacts")
            child.add_argument("--network", help="Docker network containing the disposable UQA server")
    args = parser.parse_args()
    try:
        if args.command == "validate":
            inventory = read_json(HERE / "inventory.json")
            validate_inventory(inventory, read_json(HERE / "coverage.json"))
            if args.source:
                verify_corpus(args.source)
            print(f"Validated {sum(len(s['tests']) for s in inventory['suites'].values())} upstream tests and {len(inventory['files'])} source files")
        elif args.command == "fetch":
            print(fetch(args.cache, args.archive))
        elif args.command == "build":
            build_image(args)
        elif args.command == "run":
            return run_image(args)
        else:
            return container_run(args)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"upstream harness error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
