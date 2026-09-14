#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify morphology SQL persistence in real Chrome; optional Nori memory observations are explicit."""

from __future__ import annotations

import argparse
import datetime
import importlib.util
import json
import os
import pathlib
import platform
import re
import statistics
import subprocess
import sys
import threading
import uuid
from functools import partial
from http.server import ThreadingHTTPServer
from urllib.parse import urlencode


ROOT = pathlib.Path(__file__).resolve().parents[1]
CLI_VERSION = "0.1.19"
MEMORY_FLAG = "--enable-blink-features=ForceEagerMeasureMemory"


def load_script(name: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


benchmark = load_script("run-nori-benchmark")
server_module = load_script("serve-wasm-tests")


def result_from_cli(output: str) -> dict:
    if "### Result\n" not in output:
        raise RuntimeError(f"Playwright returned no result:\n{output[-3000:]}")
    return json.JSONDecoder().raw_decode(output.split("### Result\n", 1)[1])[0]


def verify_run(report: dict, feature: str, language: str = "nori", observe_memory: bool = True) -> None:
    if report.get("schema_version") != 1 or report.get("feature") != feature:
        raise RuntimeError("Unexpected browser report schema or feature configuration")
    if report.get("language", "nori") != language or report.get("observe_memory", True) != observe_memory:
        raise RuntimeError("Unexpected browser language or memory-observation configuration")
    fixture = json.loads((ROOT / f"tests/parity/{language}/bindings.json").read_text())
    steps = fixture[feature]
    page_count = 1 + sum(bool(step.get("reopen")) for step in steps)
    expected_status = f"Passed: {len(steps)} steps across {page_count} page loads"
    if report.get("status") != expected_status or report.get("error"):
        raise RuntimeError(f"Browser verification failed: {report.get('error', report.get('status'))}")
    if report["completed_steps"] != [step["name"] for step in steps]:
        raise RuntimeError("Incomplete or repeated browser SQL fixture steps")
    if len(report["pages"]) != page_count or len(set(report["pages"])) != page_count:
        raise RuntimeError("The fixture did not use fresh page/module instances")
    checkpoints = report["checkpoints"]
    if [item["next_step"] for item in checkpoints] != [i + 1 for i, step in enumerate(steps) if step.get("reopen")]:
        raise RuntimeError("The browser skipped a required persistent reopen")
    for checkpoint in checkpoints:
        if not any(database["name"] == "/uqa" for database in checkpoint["databases"]):
            raise RuntimeError("The fixture did not persist an IDBFS database")
        if checkpoint["storage"].get("usageDetails", {}).get("indexedDB", 0) <= 0:
            raise RuntimeError("The browser reported no IndexedDB storage")
    contract = json.loads((ROOT / "benchmarks/nori/browser-contract.json").read_text())
    if report["analyses"] != (contract["checks"] if language == "nori" and feature == "enabled" else []):
        raise RuntimeError("Browser diagnostics differ from the native SQL corpus")
    if not observe_memory:
        if report["memory"]:
            raise RuntimeError("Functional verification must not collect memory observations")
        return
    if language != "nori":
        raise RuntimeError("Only Nori defines a host memory observation corpus")
    expected_memory = []
    for page in range(1, page_count + 1):
        expected_memory.extend([(page, "before_load"), (page, "opened")])
        if feature == "enabled":
            expected_memory.append((page, ["after_complete_graph_and_original_offsets", "after_recreated_descriptor_identity", "after_graphs_survive_second_reopen"][page - 1]))
        elif page == 1:
            expected_memory.append((page, "after_korean_tokenizer_fails"))
        if page < page_count:
            expected_memory.append((page, "closed_and_persisted"))
    if feature == "enabled":
        for check in contract["checks"]:
            expected_memory.extend((page_count, f"{state}/{check['mode']}/{check['name']}") for state in ["retained", "released"])
    expected_memory.append((page_count, "finished_and_persisted"))
    if [(sample["page"], sample["label"]) for sample in report["memory"]] != expected_memory:
        raise RuntimeError("Incomplete browser memory checkpoints")
    if any(not isinstance(sample["bytes"], int) or sample["bytes"] <= 0 for sample in report["memory"]):
        raise RuntimeError("Invalid browser memory estimate")


def run_browser(url: str, workdir: pathlib.Path, feature: str, language: str, observe_memory: bool) -> tuple[str, dict]:
    session = f"{language}-{uuid.uuid4().hex[:10]}"
    config = workdir / "playwright-cli.json"
    config.write_text(json.dumps({"browser": {"launchOptions": {"headless": True, "args": [MEMORY_FLAG] if observe_memory else []}}}) + "\n")
    log = workdir / f"{session}.log"

    def cli(*args: str) -> str:
        command = ["npx", "--yes", "--package", f"@playwright/cli@{CLI_VERSION}", "playwright-cli", "--session", session, *args]
        result = subprocess.run(command, cwd=workdir, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=240)
        with log.open("a") as stream:
            stream.write(result.stdout + "\n")
        if result.returncode or "### Error" in result.stdout:
            raise RuntimeError(f"Playwright {' '.join(args[:1])} failed; see {log}:\n{result.stdout[-2000:]}")
        return result.stdout

    try:
        cli("open", url, "--browser", "chrome", "--config", str(config))
        fixture = json.loads((ROOT / f"tests/parity/{language}/bindings.json").read_text())[feature]
        pages = 1 + sum(bool(step.get("reopen")) for step in fixture)
        for page_number in range(1, pages + 1):
            code = """async (page) => {
              await page.waitForFunction((expectedPage) => {
                try {
                  const report = JSON.parse(document.querySelector('#report').textContent);
                  return report.status === 'Failed' || (report.pages.length === expectedPage &&
                    (report.status === 'Awaiting page reload' || report.status.startsWith('Passed:')));
                } catch { return false; }
              }, PAGE_NUMBER, { timeout: 180000 });
              return { browser: page.context().browser().version(),
                report: JSON.parse(await page.locator('#report').textContent()) };
            }""".replace("PAGE_NUMBER", str(page_number))
            observed = result_from_cli(cli("run-code", code))
            (workdir / f"{session}-page-{page_number}.json").write_text(json.dumps(observed, ensure_ascii=False, indent=2) + "\n")
            if observed["report"]["status"] == "Failed":
                raise RuntimeError(observed["report"]["error"])
            print(f"{session}: page {page_number}/{pages}: {observed['report']['status']}", flush=True)
            if page_number < pages:
                snapshot = cli("snapshot")
                match = re.search(r"\[Snapshot\]\(([^)]+)\)", snapshot)
                if match:
                    content = (workdir / match.group(1)).read_text()
                elif "```yaml\n" in snapshot:
                    content = snapshot.split("```yaml\n", 1)[1].split("\n```", 1)[0]
                else:
                    raise RuntimeError("Playwright did not provide a fresh page snapshot")
                button = re.search(r'button "Reload and restore from IndexedDB" \[ref=([^\]]+)\](?! \[disabled\])', content)
                if not button:
                    raise RuntimeError("The persistent reload button is not enabled in the current snapshot")
                cli("click", button.group(1))
        cli("screenshot", "--filename", str(workdir / f"{session}.png"))
        messages = cli("console", "error")
        if "Errors: 0" not in messages:
            raise RuntimeError(f"Browser console contains errors: {messages}")
        verify_run(observed["report"], feature, language, observe_memory)
        return observed["browser"], observed["report"]
    finally:
        failing = sys.exc_info()[0] is not None
        try:
            cli("close")
        except RuntimeError:
            if not failing:
                raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--runs", type=int, default=1)
    parser.add_argument("--language", choices=["nori", "kuromoji"], default="nori")
    parser.add_argument("--feature", choices=["enabled", "disabled"], default="enabled")
    parser.add_argument("--observe-memory", action="store_true", help="explicitly run the Nori host-memory observation corpus")
    parser.add_argument("--bundle", type=pathlib.Path, default=ROOT / "crates/uqa-wasm/js/index.mjs")
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    if args.observe_memory and args.language != "nori":
        parser.error("--observe-memory requires --language nori")
    bundle = args.bundle.resolve()
    relative_bundle = bundle.relative_to(ROOT)
    workdir = args.output.resolve().parent
    workdir.mkdir(parents=True, exist_ok=True)
    sources = [pathlib.Path(__file__), ROOT / "scripts/serve-wasm-tests.py", ROOT / "scripts/run-nori-benchmark.py", ROOT / "scripts/build-wasm.sh",
               ROOT / "crates/uqa-wasm/build.rs", ROOT / "crates/uqa-wasm/js/callback-library.js",
               ROOT / "tests/wasm/browser_morphology.html", ROOT / "tests/wasm/browser_morphology.mjs", ROOT / "tests/wasm/export_nori_diagnostics.py", ROOT / "tests/parity/bindings.core.mjs",
               ROOT / f"tests/parity/{args.language}/bindings.json", ROOT / "examples/javascript/common.mjs", ROOT / "benchmarks/nori/browser-contract.json",
               ROOT / "crates/uqa-analysis/benches/nori/corpus.json"]
    identities = {path.relative_to(ROOT).as_posix(): benchmark.digest(path) for path in sources}
    tree_args = ["cargo", "tree", "--locked", "-p", "uqa-wasm", "--target", "wasm32-unknown-emscripten", "--edges", "normal", "--prefix", "none"]
    if args.feature == "disabled":
        tree_args.append("--no-default-features")
    tree = benchmark.command(*tree_args)
    owners = tuple(sorted(set(re.findall(r"^(uqa[\w-]*) v", tree, re.MULTILINE))))
    for language in ("nori", "kuromoji"):
        if (f"uqa-{language}-data" in owners) != (args.feature == "enabled"):
            raise RuntimeError(f"The resolved WASM runtime dependency tree disagrees with the requested {language} feature")
    runtime_hash = benchmark.runtime_sources_hash(owners)
    artifact_paths = [bundle, bundle.with_name("uqa.js"), bundle.with_name("uqa.wasm")]
    artifacts = [{"name": path.relative_to(ROOT).as_posix(), "bytes": path.stat().st_size, "sha256": benchmark.digest(path)} for path in artifact_paths]
    server = ThreadingHTTPServer(("127.0.0.1", 0), partial(server_module.Handler, directory=ROOT))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    runs, versions = [], []
    try:
        for _ in range(args.runs):
            query = urlencode({"run": uuid.uuid4().hex, "language": args.language, "feature": args.feature, "observe_memory": str(args.observe_memory).lower(), "bundle": "/" + relative_bundle.as_posix()})
            url = f"http://127.0.0.1:{server.server_port}/tests/wasm/browser_morphology.html?{query}"
            version, report = run_browser(url, workdir, args.feature, args.language, args.observe_memory)
            versions.append(version)
            runs.append(report)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    if len(set(versions)) != 1:
        raise RuntimeError("Browser version changed during verification")
    if identities != {path.relative_to(ROOT).as_posix(): benchmark.digest(path) for path in sources}:
        raise RuntimeError("Browser verification sources changed during verification")
    if runtime_hash != benchmark.runtime_sources_hash(owners):
        raise RuntimeError("Rust runtime sources changed during verification")
    if artifacts != [{"name": path.relative_to(ROOT).as_posix(), "bytes": path.stat().st_size, "sha256": benchmark.digest(path)} for path in artifact_paths]:
        raise RuntimeError("Browser artifacts changed during verification")
    summary = {}
    for sample in runs[0]["memory"]:
        key = f"{sample['page']}/{sample['label']}"
        values = [next(item["bytes"] for item in run["memory"] if item["page"] == sample["page"] and item["label"] == sample["label"]) for run in runs]
        summary[key] = {"min_bytes": min(values), "median_bytes": statistics.median(values), "max_bytes": max(values)}
    flags = benchmark.compiler_flags(os.environ)
    result = {"schema_version": 1, "feature": args.feature, "language": args.language, "observe_memory": args.observe_memory, "runs": runs, "memory_summary": summary,
              "provenance": {"verified_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                             "revision": benchmark.command("git", "rev-parse", "HEAD"),
                             "worktree_dirty": bool(benchmark.command("git", "status", "--porcelain")),
                             "cpu": benchmark.cpu_model(), "platform": platform.platform(),
                             "browser": "Chrome", "browser_version": versions[0], "headless": True,
                             "browser_flags": [MEMORY_FLAG] if args.observe_memory else [], "playwright_cli": CLI_VERSION,
                             "rustc": benchmark.command("rustc", "-Vv"), "emcc": benchmark.command("emcc", "--version"),
                             "node": benchmark.command("node", "--version"), "cargo_features": "nori,kuromoji" if args.feature == "enabled" else "none",
                             "requested_compiler_flags": benchmark.public_flags(flags), "flags_sha256": benchmark.flags_signature(flags),
                             "runtime_crates": owners, "runtime_sources_sha256": runtime_hash,
                             "artifacts": artifacts, "sources": identities,
                             "cargo_lock_sha256": benchmark.digest(ROOT / "Cargo.lock"), "jvm": None},
              "gate": {"passed": True, "scope": "SQL, fresh page/WASM reload, IndexedDB and complete diagnostic output; memory completeness only when explicitly requested"}}
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(f"Verified {args.runs} real-browser runs: {args.output}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
