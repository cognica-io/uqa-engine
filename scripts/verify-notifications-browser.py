#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify notification Fetch behavior through the generated package in real Chrome."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import pathlib
import subprocess
import sys
import uuid


ROOT = pathlib.Path(__file__).resolve().parents[1]
EXPECTED = [
    "bearer preflight, exact values, cookie/referrer omission and joined iterator return",
    "readiness is gated by the complete ready frame",
    "AbortSignal joins headers-pending",
    "AbortSignal joins ready-pending",
    "pending receipt cancellation retains one consumer and terminal failure",
    "typed rejection: retry-pending",
    "typed rejection: hidden-identity",
    "typed rejection: gzip",
    "typed rejection: malformed",
    "typed rejection: redirect",
    "typed rejection: unsupported",
    "gap and new identity precede replacement data",
    "cancellation interrupts real Retry-After backoff without another request",
    "one overflowing consumer leaves the healthy iterator usable",
    "observed authorization loss discards queued notifications",
    "silent stream produces the shared idle timeout",
]


def verify_report(observation: dict) -> None:
    if not isinstance(observation, dict) or not isinstance(observation.get("browserVersion"), str) or not observation["browserVersion"]:
        raise RuntimeError("Missing actual browser version")
    report = observation.get("report")
    if not isinstance(report, dict) or type(report.get("schema_version")) is not int or report.get("schema_version") != 1 or report.get("error") is not None:
        raise RuntimeError(f"Invalid notification browser report: {report}")
    if report.get("status") != "Passed" or report.get("passed") != EXPECTED:
        raise RuntimeError(f"Incomplete notification browser acceptance: {report}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    bundle = ROOT / "crates/uqa-wasm/js"
    spec = importlib.util.spec_from_file_location("notification_bundle", ROOT / "scripts/build-browser-notifications.py")
    builder = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(builder)
    if (bundle / "notification-core.mjs").read_text() != builder.bundle():
        raise RuntimeError("Generated notification module is stale")
    if (bundle / "notifications.d.ts").read_bytes() != (ROOT / "crates/uqa-node/notifications.d.ts").read_bytes():
        raise RuntimeError("Generated notification declarations are stale")
    paths = [pathlib.Path(__file__), ROOT / "scripts/build-browser-notifications.py",
             ROOT / "crates/uqa-client/tests/fixtures/notifications-v1.json",
             *sorted((ROOT / "tests/wasm/notifications").glob("*")),
             *(ROOT / "crates/uqa-node" / name for name in builder.MODULES),
             *(bundle / name for name in ("index.mjs", "index.d.ts", "notification-fetch.mjs", "notification-core.mjs", "notifications.d.ts", "uqa.js"))]
    identities = {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}
    session = "notifications-" + uuid.uuid4().hex[:10]
    config = output.parent / "playwright-cli.json"
    config.write_text(json.dumps({"browser": {"launchOptions": {"headless": True}}}) + "\n")
    log = output.parent / f"{session}.log"

    def cli(*arguments: str) -> str:
        result = subprocess.run(
            ["npx", "--yes", "--package", "@playwright/cli@0.1.19", "playwright-cli", "--session", session, *arguments],
            cwd=output.parent, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60,
        )
        with log.open("a") as stream:
            stream.write(result.stdout + "\n")
        if result.returncode or "### Error" in result.stdout:
            raise RuntimeError(f"Playwright {arguments[0]} failed; see {log}")
        return result.stdout

    with (output.parent / "server.log").open("w") as server_log:
        server = subprocess.Popen(["node", "tests/wasm/notifications/server.mjs"], cwd=ROOT,
                                  text=True, stdout=subprocess.PIPE, stderr=server_log)
        try:
            url = server.stdout.readline().strip()
            if not url.startswith("http://127.0.0.1:") or not url.endswith("/browser.html"):
                raise RuntimeError("Notification fixture failed to start")
            print("Verifying notification Fetch/CORS in Chrome", flush=True)
            cli("open", url, "--browser", "chrome", "--config", str(config))
            observed = cli("run-code", """async (page) => {
              await page.waitForFunction(() => {
                try { return JSON.parse(document.querySelector('#report').textContent).status !== 'Running'; }
                catch { return false; }
              }, null, { timeout: 30000 });
              return { browserVersion: page.context().browser().version(),
                report: JSON.parse(await page.locator('#report').textContent()) };
            }""")
            if "### Result\n" not in observed:
                raise RuntimeError("Chrome returned no notification report")
            observation = json.JSONDecoder().raw_decode(observed.split("### Result\n", 1)[1])[0]
            verify_report(observation)
            observation["source_files_sha256"] = identities
            output.write_text(json.dumps(observation, indent=2) + "\n")
            cli("screenshot", "--filename", str(output.parent / "notifications.png"))
            print(f"Passed: {len(EXPECTED)} browser notification cases; Chrome {observation['browserVersion']}", flush=True)
        finally:
            failing = sys.exc_info()[0] is not None
            try:
                cli("close")
            except (RuntimeError, subprocess.TimeoutExpired):
                if not failing:
                    raise
            finally:
                server.terminate()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=10)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
