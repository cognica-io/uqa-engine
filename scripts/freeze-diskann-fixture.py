#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Reproduce frozen DiskANN vector inputs from verified, already prepared embeddings."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import math
import pathlib
import struct


ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("prepare_beir", ROOT / "scripts/prepare-beir-benchmark.py")
preparation = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(preparation)


def fixture_bytes(specification: dict, prepared_directory: pathlib.Path) -> dict[str, bytes]:
    """Validate both complete source identities before selecting their frozen prefixes."""
    prepared = preparation.load_object(prepared_directory / "prepared-manifest.json")
    if specification.get("schema_version") != 1:
        raise ValueError("unsupported DiskANN fixture schema")
    if specification.get("encoding") != "row-major little-endian IEEE-754 float32":
        raise ValueError("unexpected DiskANN fixture encoding")
    for key in ("dataset", "embedding"):
        if prepared.get(key) != specification.get(key):
            raise ValueError(f"prepared {key} differs from the frozen specification")
    if prepared.get("artifacts") != specification.get("prepared_sources"):
        raise ValueError("prepared artifact identities differ from the frozen specification")
    dimensions = preparation.require_positive_integer(specification.get("dimensions"), "dimensions")
    artifacts = preparation.require_mapping(specification.get("artifacts"), "artifacts")
    if set(artifacts) != {"corpus", "queries"}:
        raise ValueError("the fixture requires exactly corpus and queries")
    result = {}
    for kind, expected in artifacts.items():
        source = prepared["artifacts"][kind]
        source_name = source["path"]
        output_name = expected["path"]
        if any(name in {"", ".", ".."} or pathlib.Path(name).name != name for name in (source_name, output_name)):
            raise ValueError("fixture artifact paths must be plain filenames")
        path = prepared_directory / source_name
        if preparation.sha256_file(path) != source["sha256"]:
            raise ValueError(f"prepared {kind} content hash differs")
        rows = preparation.require_positive_integer(expected.get("rows"), f"{kind}.rows")
        expected_ids = expected.get("source_ids")
        if not isinstance(expected_ids, list) or len(expected_ids) != rows:
            raise ValueError(f"{kind} requires one frozen source identity per row")
        encoded = bytearray()
        observed_ids = []
        for row in preparation.read_json_lines(path):
            if len(observed_ids) == rows:
                break
            values = row.get("embedding")
            if not isinstance(values, list) or len(values) != dimensions:
                raise ValueError(f"{kind} vector dimension differs")
            if any(isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) for value in values):
                raise ValueError(f"{kind} vectors must contain finite numbers")
            try:
                packed = struct.pack(f"<{dimensions}f", *values)
            except OverflowError as error:
                raise ValueError(f"{kind} float32 conversion overflows") from error
            if not all(math.isfinite(value) for value in struct.unpack(f"<{dimensions}f", packed)):
                raise ValueError(f"{kind} float32 conversion is not finite")
            encoded.extend(packed)
            observed_ids.append(row["source_id"] if kind == "corpus" else row["id"])
        if observed_ids != expected_ids:
            raise ValueError(f"{kind} prefix source identities differ")
        payload = bytes(encoded)
        if len(payload) != expected.get("bytes") or len(payload) != rows * dimensions * 4:
            raise ValueError(f"{kind} binary length differs")
        if hashlib.sha256(payload).hexdigest() != expected.get("sha256"):
            raise ValueError(f"{kind} binary hash differs")
        if output_name in result:
            raise ValueError("fixture artifact filenames must be distinct")
        result[output_name] = payload
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--specification", type=pathlib.Path, required=True)
    parser.add_argument("--prepared", type=pathlib.Path, default=ROOT / "target/benchmark-runs/beir-data")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    payloads = fixture_bytes(preparation.load_object(args.specification), args.prepared)
    args.output.mkdir(parents=True, exist_ok=True)
    for name, payload in payloads.items():
        destination = args.output / name
        temporary = destination.with_suffix(destination.suffix + ".part")
        temporary.write_bytes(payload)
        temporary.replace(destination)
    print(f"Reproduced {len(payloads)} frozen vector artifacts ({sum(map(len, payloads.values()))} bytes): {args.output}")


if __name__ == "__main__":
    main()
