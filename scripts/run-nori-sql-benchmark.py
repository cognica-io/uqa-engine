#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Measure public SQL/session costs and verify complete provider results and reviewed limits."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import pathlib
import statistics
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("nori_phrase", ROOT / "scripts/run-nori-phrase-benchmark.py")
phrase = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(phrase)
common = phrase.common
LIMITS = ROOT / "benchmarks/nori/sql-limits.json"
BENCHMARK = ROOT / "crates/uqa/benches/nori_sql.rs"
SUPPORT = ROOT / "crates/uqa/benches/nori_sql/fixture.rs"
SEED_SUPPORT = ROOT / "crates/uqa/benches/nori_sql/seeds.rs"
OWNERS = ("uqa", "uqa-engine", "uqa-core", "uqa-analysis", "uqa-nori-data", "uqa-storage",
          "uqa-storage-sqlite", "uqa-sql", "uqa-pg-query", "uqa-planner", "uqa-execution",
          "uqa-operators", "uqa-scoring", "uqa-fusion", "uqa-graph", "uqa-joins", "uqa-ml", "uqa-fdw")
PROTOCOL = {"samples": 7, "warmup": 1, "timed_operations_per_sample": 1, "insert_batch_rows": 64}
MUTATIONS = {"commit_256/0": 256, "commit_16/256": 272, "commit_16/2048": 2064, "rollback_16/256": 256}
MODES = ("none", "discard", "mixed")
DOCUMENTS = 2048


def target_key(report: dict) -> str:
    return "/".join(str(report.get(key, "")) for key in ("target_os", "target_arch", "pointer_bits"))


def expected_names(providers: list) -> set:
    names = {f"{provider}/{mutation}" for provider in providers for mutation in MUTATIONS}
    for provider in providers:
        stages = ("sql_query",) if provider == "memory" else ("sql_query", "session_and_query")
        names.update(f"{provider}/{stage}/{mode}/{case['name']}"
                     for stage in stages for mode in MODES for case in phrase.cases())
    return names


def identity(value: object) -> bool:
    return isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)


def source_hash(count: int) -> str:
    fixture = phrase.cases()
    checksum = hashlib.sha256()
    for doc_id in range(count):
        case = fixture[doc_id % len(fixture)]
        text = (case["text"] * case["repeat"]).encode()
        checksum.update(doc_id.to_bytes(8, "little", signed=True))
        checksum.update(len(text).to_bytes(8, "little"))
        checksum.update(text)
    return checksum.hexdigest()


def scored_rows(rows: list, case_index: int, count: int, controls: bool) -> None:
    expected = [*range(case_index, count, len(phrase.cases()))]
    if controls:
        expected.append(count + case_index)
    if not isinstance(rows, list) or len(rows) != len(expected):
        raise RuntimeError("SQL query omitted or added a scored document")
    for entry, expected_id in zip(rows, expected):
        if not isinstance(entry, list) or len(entry) != 2 or type(entry[0]) is not int or entry[0] != expected_id:
            raise RuntimeError("SQL query has a wrong, duplicate, or unordered document")
        if type(entry[1]) not in (int, float) or not math.isfinite(entry[1]) or entry[1] <= 0:
            raise RuntimeError("SQL query has an invalid calibrated score")


def snapshot(value: dict, count: int) -> None:
    if set(value) != {"documents", "rows_sha256", "analyzer_fingerprint", "queries"}:
        raise RuntimeError("SQL transaction snapshot is incomplete")
    if value["documents"] != count or value["rows_sha256"] != source_hash(count):
        raise RuntimeError("SQL transaction changed the complete source rows")
    if not identity(value["analyzer_fingerprint"]) or len(value["queries"]) != len(phrase.cases()):
        raise RuntimeError("SQL transaction omitted analyzer or query evidence")
    for i, rows in enumerate(value["queries"]):
        scored_rows(rows, i, count, False)


def same_snapshot(left: dict, right: dict) -> bool:
    return all(left[key] == right[key] for key in ("documents", "rows_sha256", "analyzer_fingerprint")) and \
        len(left["queries"]) == len(right["queries"]) and \
        all(phrase.same_rows(a, b) for a, b in zip(left["queries"], right["queries"]))


def output_key(name: str) -> str:
    parts = name.split("/")
    return "/".join(parts[2:]) if parts[1] in ("sql_query", "session_and_query") else "/".join(parts[1:])


def output(row: dict) -> dict:
    if "snapshot" in row:
        return row["snapshot"]
    return {key: row[key] for key in ("rows", "query_sha256", "analyzer_fingerprint")}


def same_output(left: dict, right: dict) -> bool:
    if set(left) != set(right):
        return False
    if "queries" in left:
        return same_snapshot(left, right)
    return left["query_sha256"] == right["query_sha256"] and \
        left["analyzer_fingerprint"] == right["analyzer_fingerprint"] and phrase.same_rows(left["rows"], right["rows"])


def sample_counters(row: dict) -> None:
    name = row["name"]
    samples = row["elapsed_ns"]
    if len(samples) != PROTOCOL["samples"] or any(type(value) is not int or value <= 0 for value in samples):
        raise RuntimeError(f"invalid SQL timing samples: {name}")
    if type(row.get("median_ns")) is not int or row["median_ns"] != statistics.median(samples) or row.get("verified_samples") != 9:
        raise RuntimeError(f"invalid SQL estimator or repeated output verification: {name}")
    allocation = row["allocation"]
    if set(allocation) != set(common.ALLOCATION_KEYS) or any(type(value) is not int for value in allocation.values()):
        raise RuntimeError(f"invalid SQL allocation counters: {name}")
    for unit in ("count", "bytes"):
        # Retention is a signed delta: these operations can release pre-existing state.
        if not allocation[f"{unit}_retained"] <= allocation[f"{unit}_peak"] <= allocation[f"{unit}_total"] or allocation[f"{unit}_peak"] < 0:
            raise RuntimeError(f"inconsistent SQL allocation counters: {name}")


def transaction_snapshot(row: dict, provider: str, count: int) -> None:
    name = row["name"]
    snapshot(row["snapshot"], count)
    if row.get("reopened_samples") != (0 if provider == "memory" else 9):
        raise RuntimeError(f"SQL mutation lacks repeated reopen verification: {name}")
    if "reopened_snapshot" not in row:
        raise RuntimeError(f"SQL mutation lacks its actual reopened result: {name}")
    if provider == "memory":
        if row["reopened_snapshot"] is not None:
            raise RuntimeError("Memory does not provide durable reopen evidence")
    else:
        snapshot(row["reopened_snapshot"], count)
        if not same_snapshot(row["snapshot"], row["reopened_snapshot"]):
            raise RuntimeError(f"live and reopened SQL results differ: {name}")


def transaction_probe(report: dict) -> dict:
    if report.get("schema_version") != 1 or report.get("owner") != "uqa" or report.get("purpose") != "transaction_fixture_probe":
        raise RuntimeError("unsupported transaction fixture experiment")
    if report.get("corpus_sha256") != common.digest(common.CORPUS):
        raise RuntimeError("transaction fixture experiment changed its corpus")
    provider = report.get("provider")
    if provider not in ("sqlite", "redb") or report.get("protocol") != {"samples": 7, "warmup": 1, "allocation_samples": 1}:
        raise RuntimeError("invalid transaction fixture experiment protocol")
    seed = report.get("empty_seed")
    if seed is not None and (set(seed) != {"bytes", "sha256"} or type(seed["bytes"]) is not int or seed["bytes"] <= 0 or not identity(seed["sha256"])):
        raise RuntimeError("invalid transaction fixture identity")
    rows = report.get("measurements", [])
    indexed = {row["name"]: row for row in rows}
    if len(rows) != len(indexed) or set(indexed) != {f"{provider}/{name}" for name in MUTATIONS}:
        raise RuntimeError("transaction fixture experiment requires every commit and rollback workload")
    for name, row in indexed.items():
        sample_counters(row)
        transaction_snapshot(row, provider, MUTATIONS[output_key(name)])
    if len({row["snapshot"]["analyzer_fingerprint"] for row in rows}) != 1:
        raise RuntimeError("transaction fixture experiment changed its analyzer")
    if report.get("gate") != {"allocation_and_rows_passed": False, "timing_compared": False}:
        raise RuntimeError("a transaction fixture experiment cannot claim a complete regression pass")
    return indexed


def measurements(report: dict) -> dict:
    if report.get("schema_version") != 1 or report.get("owner") != "uqa":
        raise RuntimeError("unsupported SQL benchmark schema or owner")
    if report.get("protocol") != PROTOCOL or report.get("foreground_threads") != 1:
        raise RuntimeError("invalid SQL sampling protocol")
    wasm = report.get("target_os") == "emscripten"
    providers = ["memory", "sqlite"] + ([] if wasm else ["redb"])
    if report.get("providers") != providers or report.get("pointer_bits") != (32 if wasm else 64):
        raise RuntimeError("SQL benchmark omitted a supported provider or has the wrong target width")
    if not report.get("target_arch") or report.get("query_documents") != DOCUMENTS + len(phrase.cases()):
        raise RuntimeError("SQL benchmark changed target or fixture size")
    if report.get("work_mem_bytes") != 256 * 1024 * 1024 or report.get("corpus_sha256") != common.digest(common.CORPUS):
        raise RuntimeError("SQL benchmark changed its allowance or corpus")
    if not identity(report.get("provenance", {}).get("flags_sha256")):
        raise RuntimeError("SQL measurements require compiler-flag identity")
    for key in ("provider_settings", "timing_scope", "allocation_scope", "background_statistics"):
        if not report.get(key):
            raise RuntimeError(f"SQL measurements require their {key}")
    rows = report.get("measurements", [])
    indexed = {row["name"]: row for row in rows}
    if set(indexed) != expected_names(providers) or len(indexed) != len(rows):
        raise RuntimeError("missing, duplicate, or unknown SQL workload")
    fingerprints, outputs = {}, {}
    for name, row in indexed.items():
        sample_counters(row)
        provider, stage, *rest = name.split("/")
        key = output_key(name)
        if key in MUTATIONS:
            mode = "mixed"
            transaction_snapshot(row, provider, MUTATIONS[key])
        else:
            mode, case_name = rest
            i, case = next((i, case) for i, case in enumerate(phrase.cases()) if case["name"] == case_name)
            scored_rows(row["rows"], i, DOCUMENTS, True)
            if row["query_sha256"] != hashlib.sha256(case["text"].encode()).hexdigest():
                raise RuntimeError(f"SQL benchmark changed its complete quoted phrase: {name}")
        value = output(row)
        fingerprint = value["analyzer_fingerprint"]
        if not identity(fingerprint) or fingerprints.setdefault(mode, fingerprint) != fingerprint:
            raise RuntimeError(f"SQL analyzer identity differs within mode: {mode}")
        if key in outputs and not same_output(value, outputs[key]):
            raise RuntimeError(f"SQL provider or independent-session results differ: {name}")
        outputs[key] = value
    if len(set(fingerprints.values())) != len(MODES):
        raise RuntimeError("SQL modes must retain distinct analyzer identities")
    return indexed


def check(report: dict, limits: dict, baseline: dict | None = None) -> dict:
    rows = measurements(report)
    if limits.get("schema_version") != 1 or limits.get("corpus_sha256") != report["corpus_sha256"]:
        raise RuntimeError("unsupported SQL limit schema or corpus")
    ceilings = limits.get("allocation_ceilings", {}).get(target_key(report))
    if ceilings is None or set(ceilings) != set(rows):
        raise RuntimeError("SQL allocation limits do not cover this complete target")
    if set(limits["outputs"]) != {output_key(name) for name in rows}:
        raise RuntimeError("SQL output limits do not cover every workload")
    for name, row in rows.items():
        if not same_output(output(row), limits["outputs"][output_key(name)]):
            raise RuntimeError(f"SQL analyzer, complete source rows, or calibrated scores changed: {name}")
        if set(ceilings[name]) != set(common.ALLOCATION_KEYS):
            raise RuntimeError(f"incomplete SQL allocation ceiling: {name}")
        for key, value in row["allocation"].items():
            ceiling = ceilings[name][key]
            if type(ceiling) is not int:
                raise RuntimeError(f"invalid SQL allocation ceiling: {name}/{key}")
            if value > ceiling:
                raise RuntimeError(f"SQL allocation regression: {name}/{key}: {value} > {ceiling}")
    maximum = limits["timing_max_ratio"]
    if type(maximum) not in (int, float) or not math.isfinite(maximum) or maximum <= 1:
        raise RuntimeError("SQL timing ceiling must be finite and greater than one")
    ratios = {}
    if baseline is not None:
        before = measurements(baseline)
        for key in ("protocol", "pointer_bits", "target_arch", "target_os", "corpus_sha256", "providers", "provider_settings",
                    "query_documents", "work_mem_bytes", "foreground_threads", "background_statistics", "timing_scope", "allocation_scope"):
            if report.get(key) != baseline.get(key):
                raise RuntimeError(f"incomparable SQL timing scope: {key}")
        for key in ("cpu", "platform", "rustc", "flags", "flags_sha256", "node", "emcc", "benchmark_sha256", "cargo_lock_sha256"):
            if key not in report["provenance"] or key not in baseline["provenance"] or report["provenance"][key] != baseline["provenance"][key]:
                raise RuntimeError(f"incomparable SQL timing environment: {key}")
        if report["provenance"]["cpu"] == "unknown":
            raise RuntimeError("SQL timing comparisons require an identified CPU")
        for name, row in rows.items():
            if not same_output(output(row), output(before[name])):
                raise RuntimeError(f"incomparable SQL timing output: {name}")
            ratios[name] = row["median_ns"] / before[name]["median_ns"]
            if ratios[name] > maximum:
                raise RuntimeError(f"SQL timing regression: {name}: {ratios[name]:.3f} > {maximum}")
    return {"allocation_and_rows_passed": True, "timing_compared": baseline is not None, "timing_ratios": ratios}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=("native", "wasm"), default="native")
    parser.add_argument("--report", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--limits", type=pathlib.Path, default=LIMITS)
    parser.add_argument("--measure-only", action="store_true")
    parser.add_argument("--baseline", type=pathlib.Path)
    parser.add_argument("--capture-empty-seeds", type=pathlib.Path,
                        help="capture native empty SQL databases with their original catalog identities")
    parser.add_argument("--transaction-probe", choices=("sqlite", "redb"),
                        help="compare complete SQL transactions while isolating their initial catalog fixture")
    parser.add_argument("--empty-seeds", type=pathlib.Path, help="use captured empty databases in a transaction probe")
    args = parser.parse_args()
    if args.measure_only and args.baseline:
        parser.error("--baseline requires reviewed gates")
    if args.capture_empty_seeds and args.transaction_probe:
        parser.error("capture and transaction experiments are separate executions")
    if (args.capture_empty_seeds or args.transaction_probe) and (args.target != "native" or args.report or args.measure_only or args.baseline):
        parser.error("fixture experiments require a native execution without measurement or timing options")
    if args.empty_seeds and not args.transaction_probe:
        parser.error("--empty-seeds requires a transaction fixture experiment")
    protected = [args.limits, common.CORPUS, BENCHMARK, SUPPORT, SEED_SUPPORT] + ([args.baseline] if args.baseline else [])
    if args.capture_empty_seeds:
        protected += [args.capture_empty_seeds, *(args.capture_empty_seeds / f"{provider}-empty.db" for provider in ("sqlite", "redb"))]
    if args.empty_seeds:
        protected += [args.empty_seeds, *(args.empty_seeds / f"{provider}-empty.db" for provider in ("sqlite", "redb"))]
    if args.output.resolve() in [path.resolve() for path in protected]:
        parser.error("output must not overwrite reviewed limits, corpus, benchmark sources, or baseline")
    owners = OWNERS + (("uqa-storage-redb",) if args.target == "native" else ())
    if args.capture_empty_seeds:
        if args.capture_empty_seeds.exists():
            parser.error("empty seed capture requires a new output directory")
        report = common.execute_benchmark(
            "native", "uqa", "nori_sql", "nori", owners, (SUPPORT, SEED_SUPPORT),
            arguments=("--capture-empty-seeds", str(args.capture_empty_seeds.resolve())))
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
        print(f"Empty SQL seed capture: {args.output}")
        return 0
    if args.transaction_probe:
        arguments = ("--transaction-probe", args.transaction_probe)
        if args.empty_seeds:
            arguments += (str(args.empty_seeds.resolve()),)
        report = common.execute_benchmark(
            "native", "uqa", "nori_sql", "nori", owners, (SUPPORT, SEED_SUPPORT), arguments=arguments)
        transaction_probe(report)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
        print(f"SQL transaction fixture experiment: {args.output}")
        return 0
    report = json.loads(args.report.read_text()) if args.report else common.execute_benchmark(
        args.target, "uqa", "nori_sql", "nori", owners, (SUPPORT, SEED_SUPPORT), wasm_c_headers=True)
    measurements(report)
    report["gate"] = {"allocation_and_rows_passed": False, "timing_compared": False}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    if not args.measure_only:
        baseline = json.loads(args.baseline.read_text()) if args.baseline else None
        report["gate"] = check(report, json.loads(args.limits.read_text()), baseline)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Nori SQL {'candidate measurement' if args.measure_only else 'gate passed'}: {args.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, OSError, KeyError, TypeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
