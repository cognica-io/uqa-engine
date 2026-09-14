#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("nori_persistent_measurement", ROOT / "scripts/run-nori-persistent-benchmark.py")
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


def fixture(provider="sqlite"):
    rows = [{"name": name, "documents_after": count, "elapsed_ns": [1000] * 7, "median_ns": 1000,
             "allocation": {"count_total": 4, "count_peak": 3, "count_net": 2, "bytes_total": 40, "bytes_peak": 30, "bytes_net": 20},
             "graph_sha256": "a" * 64, "field_length": count * 2, "posting_count": count * 2,
             "verified_live_and_reopened_samples": 9, "closed_seed_file_bytes": 4096, "closed_result_file_bytes": 8192}
            for name, count in benchmark.EXPECTED.items()]
    report = {"schema_version": 1, "owner": f"uqa-storage-{provider}", "threads": 1, "pointer_bits": 64,
              "target_arch": "aarch64", "target_os": "macos", "timing_scope": "mutation", "allocation_scope": "mutation",
              "filesystem": "host filesystem", "durability": "provider default",
              "protocol": dict(benchmark.index.PROTOCOL), "corpus_sha256": benchmark.common.digest(benchmark.common.CORPUS),
              "analyzer_fingerprint": "b" * 64, "measurements": rows,
              "provenance": {"cpu": "CPU", "platform": "platform", "rustc": "rustc", "flags": {}, "flags_sha256": benchmark.common.flags_signature({}), "node": None, "emcc": None, "benchmark_sha256": "c" * 64}}
    limits = {key: report[key] for key in ("schema_version", "corpus_sha256", "analyzer_fingerprint")}
    limits.update(schema_version=2, timing_max_ratio=1.25,
                  allocation_ceilings={benchmark.target_key(report): {row["name"]: dict(row["allocation"]) for row in rows}},
                  outputs={row["name"]: {key: row[key] for key in ("graph_sha256", "field_length", "posting_count")} for row in rows})
    return report, limits


class NoriPersistentBenchmarkTest(unittest.TestCase):
    def test_reviewed_provider_contracts_cover_targets_and_match_memory_graphs(self):
        limits = json.loads(benchmark.LIMITS.read_text())
        memory = json.loads(benchmark.index.LIMITS.read_text())["outputs"]
        mapping = dict(zip(benchmark.EXPECTED, ("build_points/256", "append_batch_16/256", "append_batch_16/2048", "build_points/2048")))
        self.assertEqual(set(limits), {"sqlite", "redb"})
        for provider, rule in limits.items():
            targets = {"macos/aarch64/64", "linux/x86_64/64"}
            if provider == "sqlite":
                targets.add("emscripten/wasm32/32")
            self.assertEqual(set(rule["allocation_ceilings"]), targets)
            self.assertEqual(set(rule["outputs"]), set(benchmark.EXPECTED))
            for ceilings in rule["allocation_ceilings"].values():
                self.assertEqual(set(ceilings), set(benchmark.EXPECTED))
            for name, expected in rule["outputs"].items():
                self.assertEqual(expected, memory[mapping[name]])

    def test_each_provider_uses_the_complete_transaction_contract(self):
        for provider in ("sqlite", "redb"):
            report, limits = fixture(provider)
            self.assertTrue(benchmark.check(report, limits, provider)["allocation_and_graph_passed"])
            self.assertTrue(benchmark.check(report, limits, provider, copy.deepcopy(report))["timing_compared"])

    def test_wrong_owner_and_unsupported_wasm_provider_fail(self):
        report, limits = fixture()
        with self.assertRaisesRegex(RuntimeError, "schema or owner"):
            benchmark.check(report, limits, "redb")
        report, limits = fixture("redb")
        report["target_os"] = "emscripten"
        with self.assertRaisesRegex(RuntimeError, "Emscripten"):
            benchmark.check(report, limits, "redb")

    def test_missing_workloads_samples_and_reopen_verification_fail(self):
        for change in ("workload", "samples", "reopen"):
            report, limits = fixture()
            if change == "workload":
                report["measurements"].pop()
            elif change == "samples":
                report["measurements"][0]["elapsed_ns"].pop()
            else:
                report["measurements"][0]["verified_live_and_reopened_samples"] = 8
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                benchmark.check(report, limits, "sqlite")

    def test_rollback_must_preserve_original_graph_and_count(self):
        for key in ("graph_sha256", "field_length", "posting_count", "documents_after"):
            report, limits = fixture()
            report["measurements"][-1][key] = "changed"
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                benchmark.check(report, limits, "sqlite")

    def test_allocation_and_timing_regressions_fail(self):
        report, limits = fixture()
        for key in benchmark.index.ALLOCATION_KEYS:
            changed = copy.deepcopy(limits)
            changed["allocation_ceilings"][benchmark.target_key(report)][report["measurements"][0]["name"]][key] -= 1
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "allocation regression"):
                benchmark.check(report, changed, "sqlite")
        baseline = copy.deepcopy(report)
        report["measurements"][0].update(elapsed_ns=[2000] * 7, median_ns=2000)
        with self.assertRaisesRegex(RuntimeError, "timing regression"):
            benchmark.check(report, limits, "sqlite", baseline)

    def test_platform_limits_do_not_raise_another_platforms_ceiling(self):
        report, limits = fixture("redb")
        linux = copy.deepcopy(report)
        linux.update(target_os="linux", target_arch="x86_64")
        for row in linux["measurements"]:
            row["allocation"]["bytes_net"] += 1
        limits["allocation_ceilings"][benchmark.target_key(linux)] = {
            row["name"]: dict(row["allocation"]) for row in linux["measurements"]}
        self.assertTrue(benchmark.check(linux, limits, "redb")["allocation_and_graph_passed"])
        self.assertTrue(benchmark.check(report, limits, "redb")["allocation_and_graph_passed"])
        report["measurements"][0]["allocation"]["bytes_net"] += 1
        with self.assertRaisesRegex(RuntimeError, "allocation regression"):
            benchmark.check(report, limits, "redb")

    def test_unknown_targets_and_width_only_limits_require_calibration(self):
        report, limits = fixture()
        for key, value in (("target_os", "linux"), ("target_arch", "x86_64"), ("pointer_bits", 32)):
            changed = {**report, key: value}
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "no reviewed.*target"):
                benchmark.check(changed, limits, "sqlite")
        limits["schema_version"] = 1
        with self.assertRaisesRegex(RuntimeError, "target-specific calibration"):
            benchmark.check(report, limits, "sqlite")

    def test_redb_reviewed_ceilings_reject_even_one_additional_byte_or_allocation(self):
        rule = json.loads(benchmark.LIMITS.read_text())["redb"]
        for target, ceilings in rule["allocation_ceilings"].items():
            report, _ = fixture("redb")
            target_os, target_arch, pointer_bits = target.split("/")
            report.update(target_os=target_os, target_arch=target_arch, pointer_bits=int(pointer_bits),
                          analyzer_fingerprint=rule["analyzer_fingerprint"], corpus_sha256=rule["corpus_sha256"])
            for row in report["measurements"]:
                row.update(rule["outputs"][row["name"]])
                row["allocation"] = dict(ceilings[row["name"]])
            self.assertTrue(benchmark.check(report, rule, "redb")["allocation_and_graph_passed"])
            for offset, row in enumerate(report["measurements"]):
                for key in benchmark.index.ALLOCATION_KEYS:
                    changed = copy.deepcopy(report)
                    changed["measurements"][offset]["allocation"][key] += 1
                    with self.subTest(target=target, name=row["name"], key=key), self.assertRaisesRegex(RuntimeError, "allocation regression"):
                        benchmark.check(changed, rule, "redb")

    def test_disk_observations_and_comparison_scope_cannot_be_omitted(self):
        for key in ("closed_seed_file_bytes", "closed_result_file_bytes"):
            report, limits = fixture()
            del report["measurements"][0][key]
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "file-size"):
                benchmark.check(report, limits, "sqlite")
        for key in ("durability", "filesystem"):
            report, limits = fixture()
            baseline = copy.deepcopy(report)
            baseline[key] = "different"
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "timing scope"):
                benchmark.check(report, limits, "sqlite", baseline)
            del report[key]
            with self.assertRaisesRegex(RuntimeError, "durability and filesystem"):
                benchmark.check(report, limits, "sqlite")

    def test_supporting_source_edits_change_timing_identity(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(benchmark.common, "ROOT", Path(directory)):
            entry, support = Path(directory) / "entry.rs", Path(directory) / "support.rs"
            entry.write_text("fn main() {}")
            support.write_text("original")
            self.assertEqual(benchmark.common.benchmark_sources_hash(entry, ()), benchmark.common.digest(entry))
            before = benchmark.common.benchmark_sources_hash(entry, (support,))
            support.write_text("changed")
            self.assertNotEqual(before, benchmark.common.benchmark_sources_hash(entry, (support,)))

    def test_foreign_compiler_flags_participate_in_timing_identity(self):
        flags = {"CC": "clang", "CFLAGS": "-O2", "CFLAGS_wasm32_unknown_emscripten": "-O3",
                 "HOST_CFLAGS": "-g", "TARGET_CXX": "em++", "EMCC_CFLAGS": "-sASSERTIONS=1",
                 "CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_LINKER": "emcc", "RUSTFLAGS": "-D warnings"}
        self.assertEqual(benchmark.common.compiler_flags({**flags, "UNRELATED_SECRET": "hidden"}), flags)

    def test_public_flags_preserve_identity_without_publishing_home_paths(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(benchmark.common.pathlib.Path, "home", return_value=Path(directory)):
            flags = {"CFLAGS": f"-I{directory}/include"}
            published = benchmark.common.public_flags(flags)
            self.assertEqual(published, {"CFLAGS": "-I${HOME}/include"})
            self.assertNotEqual(benchmark.common.flags_signature(flags), benchmark.common.flags_signature(published))
            report, limits = fixture()
            baseline = copy.deepcopy(report)
            report["provenance"]["flags_sha256"] = benchmark.common.flags_signature(flags)
            with self.assertRaisesRegex(RuntimeError, "flags_sha256"):
                benchmark.check(report, limits, "sqlite", baseline)
            del report["provenance"]["flags_sha256"]
            with self.assertRaisesRegex(RuntimeError, "compiler-flag identity"):
                benchmark.check(report, limits, "sqlite")

    def test_ci_requires_supported_provider_targets(self):
        native = (ROOT / ".github/workflows/ci.yml").read_text()
        wasm = (ROOT / ".github/workflows/javascript-bindings.yml").read_text()
        for provider in ("sqlite", "redb"):
            self.assertIn(f"run-nori-persistent-benchmark.py --provider {provider} --output", native)
        self.assertIn("run-nori-persistent-benchmark.py --provider sqlite --target wasm --output", wasm)
        self.assertNotIn("run-nori-persistent-benchmark.py --provider redb", wasm)
        self.assertNotIn("--measure-only", native + wasm)

if __name__ == "__main__":
    unittest.main()
