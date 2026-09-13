#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("nori_benchmark", ROOT / "scripts/run-nori-benchmark.py")
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def fixture():
    allocation = dict(zip(benchmark.ALLOCATION_KEYS, (4, 0, 2, 40, 0, 20)))

    def entry(name):
        samples = 3 if name == "cold_decode_validate_drop" else 7
        return {"name": name, "allocation": dict(allocation), "timing": {"elapsed_ns": [100_000_000] * samples, "iterations": [100] * samples, "median_ns": 1_000_000.0}}

    entries = [entry("cold_decode_validate_drop"), *(entry(name) for name in benchmark.SHARING)]
    entries[0]["allocation"].update(count_retained=1, bytes_retained=10)
    entries[1]["handles"] = 64
    entries[2]["handles"] = 64
    for case in json.loads(benchmark.CORPUS.read_text())["cases"]:
        text = case["text"] * case["repeat"]
        for stage in ("tokenizer", "analyzer"):
            for mode in ("None", "Discard", "Mixed"):
                item = entry(f"{stage}/{mode}/{case['name']}")
                item.update(input_bytes=len(text.encode()), input_utf16=len(text.encode("utf-16-le")) // 2, input_sha256=hashlib.sha256(text.encode()).hexdigest(), tokens=3, output_sha256="a" * 64)
                entries.append(item)
    report = {
        "schema_version": 1, "protocol": dict(benchmark.PROTOCOL), "pointer_bits": 64,
        "threads": 1, "target_arch": "aarch64", "target_os": "macos",
        "corpus_sha256": benchmark.digest(benchmark.CORPUS), "bundle_sha256": "b" * 64,
        "bundle_bytes": 123, "allocation_counter": "counter", "timing_scope": "scope",
        "measurements": entries,
        "provenance": {"cpu": "CPU", "platform": "platform", "rustc": "rustc", "flags": {}, "node": None, "emcc": None, "benchmark_sha256": "c" * 64},
    }
    limits = {key: report[key] for key in ("schema_version", "corpus_sha256", "bundle_sha256", "bundle_bytes", "allocation_counter")}
    limits.update(
        allocation_ceilings={"64": {row["name"]: dict(row["allocation"]) for row in entries}},
        outputs={row["name"]: {"tokens": row["tokens"], "output_sha256": row["output_sha256"]} for row in entries[3:]},
        timing_max_ratio=1.25,
    )
    return report, limits


class NoriBenchmarkTest(unittest.TestCase):
    def test_empty_global_flags_do_not_shadow_wasm_link_options(self):
        class BuildCaptured(Exception):
            pass

        flag_key = "CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUSTFLAGS"
        for empty in ({"RUSTFLAGS": ""}, {"CARGO_ENCODED_RUSTFLAGS": ""}, {"RUSTFLAGS": "", "CARGO_ENCODED_RUSTFLAGS": ""}):
            environment = {"EMSDK_PYTHON": "/python", flag_key: "-D warnings", **empty}

            def capture(*args, env=None):
                if args == ("em-config", "CACHE"):
                    return "/emsdk/cache"
                self.assertEqual(args[:2], ("cargo", "bench"))
                self.assertEqual(args[-2:], ("--target", benchmark.WASM_TARGET))
                self.assertNotIn("RUSTFLAGS", env)
                self.assertNotIn("CARGO_ENCODED_RUSTFLAGS", env)
                self.assertEqual(env[flag_key], "-D warnings " + benchmark.WASM_FLAGS + " -C link-arg=-sINITIAL_HEAP=16777216")
                self.assertEqual(env["BINDGEN_EXTRA_CLANG_ARGS_wasm32_unknown_emscripten"], "--sysroot=/emsdk/cache/sysroot -fvisibility=default")
                self.assertEqual(env["CFLAGS_wasm32_unknown_emscripten"], "")
                self.assertEqual(dict(benchmark.os.environ), environment)
                raise BuildCaptured

            with self.subTest(empty=empty), patch.dict(benchmark.os.environ, environment, clear=True), patch.object(benchmark, "cpu_model", return_value="CPU"), patch.object(benchmark, "runtime_sources_hash", return_value="sources"), patch.object(benchmark, "digest", return_value="digest"), patch.object(benchmark, "command", side_effect=capture):
                with self.assertRaises(BuildCaptured):
                    benchmark.execute_benchmark("wasm", "uqa-analysis", "nori", "nori", ("uqa-analysis",), wasm_c_headers=True)

    def test_nonempty_global_flags_are_rejected_before_wasm_build(self):
        for key in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"):
            environment = {"EMSDK_PYTHON": "/python", key: "-D warnings"}
            with self.subTest(key=key), patch.dict(benchmark.os.environ, environment, clear=True), patch.object(benchmark, "cpu_model", return_value="CPU"), patch.object(benchmark, "runtime_sources_hash", return_value="sources"), patch.object(benchmark, "digest", return_value="digest"), patch.object(benchmark, "command") as command:
                with self.assertRaisesRegex(RuntimeError, "global flags override required linker options"):
                    benchmark.execute_benchmark("wasm", "uqa-analysis", "nori", "nori", ("uqa-analysis",))
                command.assert_not_called()

    def test_ci_requires_native_and_wasm_resource_checks(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        gate = workflow.split("  gate:\n", 1)[1]
        self.assertIn("nori-benchmark", gate.split("needs: [", 1)[1].split("]", 1)[0])
        self.assertIn("run-nori-benchmark.py --output", workflow)
        wasm = (ROOT / ".github/workflows/javascript-bindings.yml").read_text()
        self.assertIn("run-nori-benchmark.py --target wasm --output", wasm)
        self.assertNotIn("--measure-only", workflow + wasm)

    def test_separates_resource_pass_from_optional_timing_comparison(self):
        report, limits = fixture()
        result = benchmark.check(report, limits)
        self.assertTrue(result["allocation_and_output_passed"])
        self.assertFalse(result["timing_compared"])
        result = benchmark.check(report, limits, copy.deepcopy(report))
        self.assertEqual(set(result["timing_ratios"].values()), {1.0})

    def test_rejects_missing_duplicate_or_unknown_workloads(self):
        for mutation in (lambda rows: rows.pop(), lambda rows: rows.append(rows[0]), lambda rows: rows[0].update(name="unknown")):
            report, limits = fixture()
            mutation(report["measurements"])
            with self.assertRaisesRegex(RuntimeError, "missing, duplicate, or unknown"):
                benchmark.check(report, limits)

    def test_rejects_short_nonfinite_or_forged_timing_samples(self):
        changes = [{"median_ns": float("nan")}, {"median_ns": 100.0}, {"elapsed_ns": [1] * 3}, {"iterations": [0] * 3}, {"iterations": [True] * 3}, {"elapsed_ns": [100_000_000]}]
        for change in changes:
            report, limits = fixture()
            report["measurements"][0]["timing"].update(change)
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                benchmark.check(report, limits)

    def test_every_allocation_metric_has_a_strict_ceiling(self):
        for key in benchmark.ALLOCATION_KEYS:
            report, limits = fixture()
            name = report["measurements"][0]["name"]
            limits["allocation_ceilings"]["64"][name][key] = report["measurements"][0]["allocation"][key] - 1
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "allocation regression"):
                benchmark.check(report, limits)

    def test_rejects_retained_leaks_and_inconsistent_allocation_counters(self):
        for change in ({"bytes_retained": 1}, {"bytes_peak": 41}, {"count_total": -1}, {"bytes_total": float("nan")}):
            report, limits = fixture()
            report["measurements"][2]["allocation"].update(change)
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                benchmark.check(report, limits)

    def test_rejects_changed_corpus_bundle_and_token_graph(self):
        for key in ("corpus_sha256", "bundle_sha256", "bundle_bytes", "allocation_counter"):
            report, limits = fixture()
            report[key] = "changed"
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "identity mismatch"):
                benchmark.check(report, limits)
        for key in ("input_bytes", "input_utf16", "input_sha256", "tokens", "output_sha256"):
            report, limits = fixture()
            report["measurements"][3][key] = "changed"
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                benchmark.check(report, limits)

    def test_rejects_uncovered_outputs_and_resource_metrics(self):
        for path in ("outputs", "ceilings", "metric"):
            report, limits = fixture()
            name = report["measurements"][3]["name"]
            if path == "outputs":
                del limits["outputs"][name]
            elif path == "ceilings":
                del limits["allocation_ceilings"]["64"][name]
            else:
                del limits["allocation_ceilings"]["64"][name]["bytes_peak"]
            with self.subTest(path=path), self.assertRaises(RuntimeError):
                benchmark.check(report, limits)

    def test_rejects_invalid_limits_instead_of_disabling_the_gate(self):
        for invalid in (float("nan"), float("inf"), -1, True):
            report, limits = fixture()
            limits["allocation_ceilings"]["64"]["cold_decode_validate_drop"]["bytes_peak"] = invalid
            with self.subTest(invalid=invalid), self.assertRaisesRegex(RuntimeError, "invalid allocation ceiling"):
                benchmark.check(report, limits)
        report, limits = fixture()
        limits["outputs"][report["measurements"][3]["name"]] = None
        with self.assertRaisesRegex(RuntimeError, "missing reviewed token output"):
            benchmark.check(report, limits)

    def test_timing_comparison_rejects_changed_or_missing_environment(self):
        for key in ("cpu", "platform", "rustc", "flags", "node", "emcc", "benchmark_sha256"):
            for missing in (False, True):
                report, limits = fixture()
                baseline = copy.deepcopy(report)
                if missing:
                    del baseline["provenance"][key]
                else:
                    baseline["provenance"][key] = "changed"
                with self.subTest(key=key, missing=missing), self.assertRaisesRegex(RuntimeError, "incomparable"):
                    benchmark.check(report, limits, baseline)

    def test_timing_comparison_detects_regression_using_measured_samples(self):
        report, limits = fixture()
        baseline = copy.deepcopy(report)
        timing = report["measurements"][0]["timing"]
        timing["elapsed_ns"] = [200_000_000] * 3
        timing["median_ns"] *= 2
        with self.assertRaisesRegex(RuntimeError, "timing regression"):
            benchmark.check(report, limits, baseline)

    def test_timing_baseline_must_perform_the_same_token_work(self):
        report, limits = fixture()
        baseline = copy.deepcopy(report)
        baseline["measurements"][3]["tokens"] = 0
        with self.assertRaisesRegex(RuntimeError, "incomparable Nori timing outputs"):
            benchmark.check(report, limits, baseline)

    def test_checked_in_gate_covers_native_and_wasm(self):
        limits = json.loads(benchmark.LIMITS.read_text())
        self.assertEqual(limits["corpus_sha256"], benchmark.digest(benchmark.CORPUS))
        self.assertEqual(set(limits["allocation_ceilings"]), {"32", "64"})
        self.assertEqual(set(limits["allocation_ceilings"]["32"]), set(limits["allocation_ceilings"]["64"]))
        self.assertEqual(len(limits["outputs"]), 36)

    def test_calibration_is_backed_by_complete_hashed_measurements(self):
        limits = json.loads(benchmark.LIMITS.read_text())
        self.assertEqual(len(limits["calibration"]["reports"]), 4)
        for record in limits["calibration"]["reports"]:
            path = ROOT / record["path"]
            self.assertEqual(benchmark.digest(path), record["sha256"])
            report = json.loads(path.read_text())
            self.assertTrue(benchmark.check(report, limits)["allocation_and_output_passed"])
            self.assertTrue(report["gate"]["allocation_and_output_passed"])


if __name__ == "__main__":
    unittest.main()
