#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify overlapping browser transactions and committed IndexedDB restore."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import pathlib
import subprocess
import sys
import threading
import uuid
from functools import partial
from http.server import ThreadingHTTPServer
from urllib.parse import urlencode


ROOT = pathlib.Path(__file__).resolve().parents[1]
CLI_VERSION = "0.1.19"
ISOLATION_LEVELS = ("READ COMMITTED", "READ UNCOMMITTED", "REPEATABLE READ", "SERIALIZABLE")


def digest(path: pathlib.Path) -> str:
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def verify_report(report: dict) -> None:
    oracle = json.loads((ROOT / "tests/parity/pg18/concurrent_writes.expected.json").read_text())
    expected = [
        f"{mode}/{isolation}/{case['name']}"
        for mode in ("sqlite", "compressed")
        for isolation in ISOLATION_LEVELS
        for case in oracle["cases"]
    ]
    if report.get("schema_version") != 1 or report.get("error"):
        raise RuntimeError(f"Invalid browser transaction report: {report}")
    if report.get("status") != f"Passed: {len(expected)} schedules and fresh-page restores":
        raise RuntimeError(f"Browser transactions failed: {report.get('status')}")
    if report.get("completed_cases") != expected or report.get("restored_cases") != expected:
        raise RuntimeError("The browser omitted or repeated an overlapping transaction or restored file")
    if len(report.get("pages", [])) != 2 or len(set(report["pages"])) != 2:
        raise RuntimeError("Transaction restore did not use two fresh page/module instances")
    if "/uqa" not in report.get("checkpoint_databases", []):
        raise RuntimeError("The browser did not checkpoint the closed databases to IndexedDB")


def run_browser(url: str, workdir: pathlib.Path) -> dict:
    session = f"transactions-{uuid.uuid4().hex[:10]}"
    config = workdir / "playwright-cli.json"
    config.write_text(json.dumps({"browser": {"launchOptions": {"headless": True}}}) + "\n")
    log = workdir / f"{session}.log"

    def cli(*arguments: str) -> str:
        result = subprocess.run(
            ["npx", "--yes", "--package", f"@playwright/cli@{CLI_VERSION}", "playwright-cli",
             "--session", session, *arguments],
            cwd=workdir, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=240,
        )
        with log.open("a") as stream:
            stream.write(result.stdout + "\n")
        if result.returncode or "### Error" in result.stdout:
            raise RuntimeError(f"Playwright {arguments[0]} failed; see {log}:\n{result.stdout[-2000:]}")
        return result.stdout

    try:
        cli("open", url, "--browser", "chrome", "--config", str(config))
        for page_number in (1, 2):
            observed = cli("run-code", """async (page) => {
              await page.waitForFunction((expectedPage) => {
                try {
                  const report = JSON.parse(document.querySelector('#report').textContent);
                  return report.status === 'Failed' || (report.pages.length === expectedPage &&
                    (report.status === 'Awaiting page reload' || report.status.startsWith('Passed:')));
                } catch { return false; }
              }, PAGE_NUMBER, { timeout: 180000 });
              return { browser_version: page.context().browser().version(),
                report: JSON.parse(await page.locator('#report').textContent()) };
            }""".replace("PAGE_NUMBER", str(page_number)))
            if "### Result\n" not in observed:
                raise RuntimeError(f"Playwright returned no browser result:\n{observed[-3000:]}")
            result = json.JSONDecoder().raw_decode(observed.split("### Result\n", 1)[1])[0]
            (workdir / f"{session}-page-{page_number}.json").write_text(json.dumps(result, indent=2) + "\n")
            report = result["report"]
            if report["status"] == "Failed":
                raise RuntimeError(report["error"])
            print(f"Concurrent browser transactions: page {page_number}/2: {report['status']}", flush=True)
            if page_number == 1:
                if report["status"] != "Awaiting page reload":
                    raise RuntimeError("The browser did not pause after its IndexedDB checkpoint")
                cli("reload")
        verify_report(report)
        cli("screenshot", "--filename", str(workdir / f"{session}.png"))
        console = cli("console", "error")
        if "Errors: 0" not in console:
            raise RuntimeError(f"Browser console contains errors: {console}")
        return result
    finally:
        failing = sys.exc_info()[0] is not None
        try:
            cli("close")
        except (RuntimeError, subprocess.TimeoutExpired):
            if not failing:
                raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--bundle", type=pathlib.Path, default=ROOT / "crates/uqa-wasm/js/index.mjs")
    args = parser.parse_args()
    bundle = args.bundle.resolve()
    relative_bundle = bundle.relative_to(ROOT)
    workdir = args.output.resolve().parent
    workdir.mkdir(parents=True, exist_ok=True)
    source_paths = (
        pathlib.Path(__file__), ROOT / "scripts/serve-wasm-tests.py",
        ROOT / "tests/wasm/browser_concurrent_transactions.html",
        ROOT / "tests/wasm/browser_concurrent_transactions.mjs",
        ROOT / "tests/parity/concurrent_transactions.mjs",
        ROOT / "tests/parity/pg18/concurrent_writes.expected.json",
        ROOT / "examples/javascript/common.mjs",
    )
    artifact_paths = (bundle, bundle.with_name("uqa.js"), bundle.with_name("uqa.wasm"))
    identities = {str(path.relative_to(ROOT)): digest(path) for path in (*source_paths, *artifact_paths)}
    spec = importlib.util.spec_from_file_location("serve_wasm_tests", ROOT / "scripts/serve-wasm-tests.py")
    server_module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(server_module)
    server = ThreadingHTTPServer(("127.0.0.1", 0), partial(server_module.Handler, directory=ROOT))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        query = urlencode({"run": uuid.uuid4().hex, "bundle": "/" + relative_bundle.as_posix()})
        url = f"http://127.0.0.1:{server.server_port}/tests/wasm/browser_concurrent_transactions.html?{query}"
        result = run_browser(url, workdir)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    if identities != {str(path.relative_to(ROOT)): digest(path) for path in (*source_paths, *artifact_paths)}:
        raise RuntimeError("Browser transaction sources or runtime artifacts changed during verification")
    result["sources_and_artifacts"] = identities
    result["playwright_cli"] = CLI_VERSION
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(f"Verified real-browser concurrent transactions and IndexedDB restore: {args.output}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
