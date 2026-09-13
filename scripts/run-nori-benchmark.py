#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Measure the analysis owner on native/WASM targets and check reviewed resource ceilings."""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import math
import os
import pathlib
import platform
import statistics
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
CORPUS = ROOT / "crates/uqa-analysis/benches/nori/corpus.json"
LIMITS = ROOT / "benchmarks/nori/limits.json"
WASM_TARGET = "wasm32-unknown-emscripten"
WASM_FLAGS = "-C link-arg=-sALLOW_MEMORY_GROWTH=1 -C link-arg=-sMAXIMUM_MEMORY=2147483648 -C link-arg=-sDEFAULT_TO_CXX -C link-arg=-sSTACK_SIZE=5242880"
PROTOCOL = {"samples": 7, "cold_samples": 3, "warmup": 2, "pilot": 1, "clock": "per_batch", "sample_ms": 75}
ALLOCATION_KEYS = ("count_total", "count_retained", "count_peak", "bytes_total", "bytes_retained", "bytes_peak")
SHARING = ("cached_name_resolve_64_shared_handles", "cached_identity_resolve_64_shared_handles")


def command(*args: str, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(args, cwd=ROOT, env=env, text=True, stdout=subprocess.PIPE)
    if result.returncode:
        for line in result.stdout.splitlines():
            if line.startswith("{"):
                try:
                    item = json.loads(line)
                    if item.get("reason") == "compiler-message":
                        print(item["message"]["rendered"], file=sys.stderr)
                except (ValueError, KeyError):
                    print(line, file=sys.stderr)
            else:
                print(line, file=sys.stderr)
        result.check_returncode()
    return result.stdout.strip()


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def runtime_sources_hash() -> str:
    checksum = hashlib.sha256()
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock"]
    for crate in ("uqa-analysis", "uqa-core", "uqa-nori-data"):
        directory = ROOT / "crates" / crate
        paths.append(directory / "Cargo.toml")
        paths.extend((directory / "src").rglob("*.rs"))
    for path in sorted(paths):
        checksum.update(path.relative_to(ROOT).as_posix().encode())
        checksum.update(b"\0")
        checksum.update(path.read_bytes())
        checksum.update(b"\0")
    return checksum.hexdigest()


def cpu_model() -> str:
    cpuinfo = pathlib.Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text().splitlines():
            if line.startswith("model name"):
                return line.partition(":")[2].strip()
    if sys.platform == "darwin":
        return command("sysctl", "-n", "machdep.cpu.brand_string")
    return platform.processor() or "unknown"


def measurements(report: dict) -> dict[str, dict]:
    if report.get("schema_version") != 1 or report.get("protocol") != PROTOCOL:
        raise RuntimeError("unsupported Nori measurement schema or sampling protocol")
    if report.get("threads") != 1 or report.get("pointer_bits") not in (32, 64):
        raise RuntimeError("Nori measurements require one thread and a known pointer width")
    cases = json.loads(CORPUS.read_text())["cases"]
    expected = {f"{stage}/{mode}/{case['name']}" for stage in ("tokenizer", "analyzer") for mode in ("None", "Discard", "Mixed") for case in cases}
    expected.update(("cold_decode_validate_drop", *SHARING))
    entries = report.get("measurements", [])
    indexed = {entry["name"]: entry for entry in entries}
    if set(indexed) != expected or len(entries) != len(expected):
        raise RuntimeError("missing, duplicate, or unknown Nori measurements")
    for name, entry in indexed.items():
        timing = entry["timing"]
        sample_count = PROTOCOL["cold_samples"] if name == "cold_decode_validate_drop" else PROTOCOL["samples"]
        elapsed, iterations = timing["elapsed_ns"], timing["iterations"]
        if len(elapsed) != sample_count or len(iterations) != sample_count:
            raise RuntimeError(f"incomplete timing samples: {name}")
        if any(type(value) is not int or value <= 0 for value in elapsed + iterations):
            raise RuntimeError(f"invalid timing samples: {name}")
        if any(value < PROTOCOL["sample_ms"] * 1_000_000 for value in elapsed):
            raise RuntimeError(f"short timing sample: {name}")
        median = statistics.median(left / right for left, right in zip(elapsed, iterations))
        if not math.isfinite(timing["median_ns"]) or not math.isclose(median, timing["median_ns"], rel_tol=1e-12):
            raise RuntimeError(f"incorrect timing estimator: {name}")
        allocation = entry["allocation"]
        if set(allocation) != set(ALLOCATION_KEYS) or any(type(value) is not int or value < 0 for value in allocation.values()):
            raise RuntimeError(f"invalid allocation counters: {name}")
        for unit in ("count", "bytes"):
            if not allocation[f"{unit}_retained"] <= allocation[f"{unit}_peak"] <= allocation[f"{unit}_total"]:
                raise RuntimeError(f"inconsistent allocation counters: {name}")
            if name != "cold_decode_validate_drop" and allocation[f"{unit}_retained"] != 0:
                raise RuntimeError(f"unexpected retained allocation: {name}")
    for case in cases:
        text = case["text"] * case["repeat"]
        identity = {"input_bytes": len(text.encode()), "input_utf16": len(text.encode("utf-16-le")) // 2, "input_sha256": hashlib.sha256(text.encode()).hexdigest()}
        for stage in ("tokenizer", "analyzer"):
            for mode in ("None", "Discard", "Mixed"):
                entry = indexed[f"{stage}/{mode}/{case['name']}"]
                if any(entry.get(key) != value for key, value in identity.items()):
                    raise RuntimeError(f"workload identity changed: {entry['name']}")
    if any(indexed[name].get("handles") != 64 for name in SHARING):
        raise RuntimeError("dictionary sharing measurement is incomplete")
    return indexed


def check(report: dict, limits: dict, baseline: dict | None = None) -> dict:
    entries = measurements(report)
    if limits.get("schema_version") != 1:
        raise RuntimeError("unsupported Nori resource limit schema")
    for key in ("corpus_sha256", "bundle_sha256", "bundle_bytes", "allocation_counter"):
        if report.get(key) != limits.get(key):
            raise RuntimeError(f"reviewed Nori identity mismatch: {key}")
    if report["corpus_sha256"] != digest(CORPUS):
        raise RuntimeError("Nori report does not measure the checked-out corpus")
    maximum = limits["timing_max_ratio"]
    if isinstance(maximum, bool) or not math.isfinite(maximum) or maximum <= 1:
        raise RuntimeError("timing ceiling must be a finite ratio greater than one")
    ceilings = limits["allocation_ceilings"][str(report["pointer_bits"])]
    if set(ceilings) != set(entries):
        raise RuntimeError("resource ceilings do not cover every Nori measurement")
    for name, entry in entries.items():
        if set(ceilings[name]) != set(ALLOCATION_KEYS):
            raise RuntimeError(f"resource ceilings omit allocation counters: {name}")
        for key, value in entry["allocation"].items():
            ceiling = ceilings[name][key]
            if type(ceiling) is not int or ceiling < 0:
                raise RuntimeError(f"invalid allocation ceiling: {name}/{key}")
            if value > ceiling:
                raise RuntimeError(f"Nori allocation regression: {name}/{key}: {value} > {ceilings[name][key]}")
        if "/" in name:
            expected = limits["outputs"].get(name)
            if not isinstance(expected, dict) or set(expected) != {"tokens", "output_sha256"}:
                raise RuntimeError(f"missing reviewed token output: {name}")
            if any(entry.get(key) != expected[key] for key in ("tokens", "output_sha256")):
                raise RuntimeError(f"Nori token output changed: {name}")
    expected_outputs = {name for name in entries if "/" in name}
    if set(limits["outputs"]) != expected_outputs:
        raise RuntimeError("reviewed token outputs do not cover every workload")
    ratios = {}
    if baseline is not None:
        bases = measurements(baseline)
        for key in ("protocol", "pointer_bits", "target_arch", "target_os", "corpus_sha256", "bundle_sha256", "allocation_counter", "timing_scope"):
            if report.get(key) != baseline.get(key):
                raise RuntimeError(f"incomparable Nori timing baseline: {key}")
        for key in ("cpu", "platform", "rustc", "flags", "node", "emcc", "benchmark_sha256"):
            if key not in report["provenance"] or key not in baseline["provenance"] or report["provenance"][key] != baseline["provenance"][key]:
                raise RuntimeError(f"incomparable Nori timing environment: {key}")
        if report["provenance"]["cpu"] == "unknown":
            raise RuntimeError("timing comparison requires an identified CPU")
        for name, entry in entries.items():
            if "/" in name and any(entry[key] != bases[name].get(key) for key in ("tokens", "output_sha256")):
                raise RuntimeError(f"incomparable Nori timing outputs: {name}")
            ratio = entry["timing"]["median_ns"] / bases[name]["timing"]["median_ns"]
            ratios[name] = ratio
            if ratio > maximum:
                raise RuntimeError(f"Nori timing regression: {name}: {ratio:.3f} > {maximum}")
    return {"allocation_and_output_passed": True, "timing_compared": baseline is not None, "timing_ratios": ratios}


def run(target: str) -> dict:
    env = os.environ.copy()
    cpu = cpu_model()
    runtime_hash = runtime_sources_hash()
    benchmark_hash = digest(ROOT / "crates/uqa-analysis/benches/nori.rs")
    target_args = []
    if target == "wasm":
        if "EMSDK_PYTHON" not in env:
            if sys.version_info < (3, 10):
                raise RuntimeError("WASM requires EMSDK_PYTHON pointing to Python 3.10 or newer")
            env["EMSDK_PYTHON"] = sys.executable
        flag_key = "CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUSTFLAGS"
        if env.get("RUSTFLAGS") or env.get("CARGO_ENCODED_RUSTFLAGS"):
            raise RuntimeError("use target-scoped Rust flags for WASM; global flags override required linker options")
        env[flag_key] = f"{env.get(flag_key, '')} {WASM_FLAGS}".strip()
        target_args = ["--target", WASM_TARGET]
    build = command("cargo", "bench", "--locked", "-p", "uqa-analysis", "--features", "nori", "--bench", "nori", "--no-run", "--message-format=json", *target_args, env=env)
    artifacts = [json.loads(line) for line in build.splitlines() if line.startswith("{")]
    binaries = [pathlib.Path(item["executable"]) for item in artifacts if item.get("reason") == "compiler-artifact" and item["target"]["name"] == "nori" and item.get("executable")]
    if len(binaries) != 1:
        raise RuntimeError("Cargo did not produce exactly one Nori benchmark executable")
    executable = binaries[0]
    paths = [executable]
    invocation = [str(executable)]
    if target == "wasm":
        paths.append(executable.with_suffix(".wasm"))
        invocation.insert(0, "node")
    report = json.loads(command(*invocation, env=env))
    if runtime_sources_hash() != runtime_hash or digest(ROOT / "crates/uqa-analysis/benches/nori.rs") != benchmark_hash:
        raise RuntimeError("Nori sources changed during measurement; rerun with a stable checkout")
    report["provenance"] = {
        "measured_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "revision": command("git", "rev-parse", "HEAD"),
        "worktree_dirty": bool(command("git", "status", "--porcelain")),
        "cpu": cpu,
        "platform": platform.platform(),
        "rustc": command("rustc", "-Vv"),
        "flags": {key: value for key, value in sorted(env.items()) if "RUSTFLAGS" in key or key.startswith("CARGO_PROFILE_")},
        "node": command("node", "--version") if target == "wasm" else None,
        "emcc": command("emcc", "--version", env=env) if target == "wasm" else None,
        "benchmark_sha256": benchmark_hash,
        "cargo_lock_sha256": digest(ROOT / "Cargo.lock"),
        "runtime_sources_sha256": runtime_hash,
        "profile": "bench (workspace release, thin LTO, one codegen unit, debug information)",
        "container": None,
        "jvm": None,
        "artifacts": [{"name": path.name, "bytes": path.stat().st_size, "sha256": digest(path)} for path in paths],
    }
    measurements(report)
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=("native", "wasm"), default="native")
    parser.add_argument("--report", type=pathlib.Path, help="check an existing report instead of running")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--limits", type=pathlib.Path, default=LIMITS)
    parser.add_argument("--baseline", type=pathlib.Path, help="compare timing on the same host/toolchain")
    parser.add_argument("--measure-only", action="store_true", help="collect candidate evidence without accepting it as a passing gate")
    args = parser.parse_args()
    if args.measure_only and args.baseline:
        parser.error("--baseline requires the reviewed gate")
    protected = [CORPUS, args.limits] + ([args.baseline] if args.baseline else [])
    if args.output.resolve() in [path.resolve() for path in protected]:
        parser.error("output must not overwrite the corpus, reviewed limits, or timing baseline")
    report = json.loads(args.report.read_text()) if args.report else run(args.target)
    measurements(report)
    report["gate"] = {"allocation_and_output_passed": False, "timing_compared": False}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    if not args.measure_only:
        baseline = json.loads(args.baseline.read_text()) if args.baseline else None
        report["gate"] = check(report, json.loads(args.limits.read_text()), baseline)
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(f"Nori {'candidate measurement' if args.measure_only else 'gate passed'}: {args.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, OSError, KeyError, TypeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
