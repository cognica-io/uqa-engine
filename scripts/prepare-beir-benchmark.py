#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Download a pinned BEIR dataset and generate real sentence embeddings."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import pathlib
import shutil
import stat
import sys
import tempfile
import time
import urllib.error
import urllib.request
import zipfile
from collections.abc import Iterable
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT / "benchmarks" / "beir" / "manifest.json"
DEFAULT_CACHE = ROOT / "target" / "benchmark-runs" / "beir-cache"
DEFAULT_OUTPUT = ROOT / "target" / "benchmark-runs" / "beir-data"
CHUNK_BYTES = 1024 * 1024


class PreparationError(RuntimeError):
    """A reproducibility or input-integrity failure."""


def load_object(path: pathlib.Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PreparationError(f"cannot read JSON {path}: {error}") from error
    if not isinstance(value, dict):
        raise PreparationError(f"JSON root must be an object: {path}")
    return value


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def manifest_sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require_mapping(value: object, context: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise PreparationError(f"{context} must be an object")
    return value


def require_positive_integer(value: object, context: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise PreparationError(f"{context} must be a positive integer")
    return value


def validate_benchmark_manifest(manifest: dict[str, Any]) -> None:
    if manifest.get("schema_version") != 1:
        raise PreparationError("unsupported BEIR benchmark manifest schema")
    dataset = require_mapping(manifest.get("dataset"), "dataset")
    embedding = require_mapping(manifest.get("embedding"), "embedding")
    for key in ("name", "url", "archive_sha256", "qrels_split"):
        if not isinstance(dataset.get(key), str) or not dataset[key]:
            raise PreparationError(f"dataset.{key} must be a non-empty string")
    if not dataset["url"].startswith("https://"):
        raise PreparationError("dataset.url must use HTTPS")
    archive_hash = dataset["archive_sha256"]
    if len(archive_hash) != 64 or any(ch not in "0123456789abcdef" for ch in archive_hash):
        raise PreparationError("dataset.archive_sha256 must be lowercase SHA-256")
    require_positive_integer(dataset.get("expected_corpus_count"), "dataset.expected_corpus_count")
    require_positive_integer(dataset.get("expected_query_count"), "dataset.expected_query_count")
    for key in ("provider", "package_version", "model", "revision", "device"):
        if not isinstance(embedding.get(key), str) or not embedding[key]:
            raise PreparationError(f"embedding.{key} must be a non-empty string")
    if embedding["provider"] != "sentence-transformers":
        raise PreparationError("embedding.provider must be sentence-transformers")
    for key in ("dimensions", "batch_size", "max_sequence_length"):
        require_positive_integer(embedding.get(key), f"embedding.{key}")
    if embedding.get("normalize_embeddings") is not True:
        raise PreparationError("embedding.normalize_embeddings must be true")


def download_archive(url: str, destination: pathlib.Path, expected_hash: str) -> tuple[bool, float]:
    if destination.is_file() and sha256_file(destination) == expected_hash:
        return True, 0.0
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix(destination.suffix + ".part")
    temporary.unlink(missing_ok=True)
    started = time.perf_counter()
    digest = hashlib.sha256()
    try:
        request = urllib.request.Request(url, headers={"User-Agent": "uqa-engine-beir-benchmark/1"})
        with urllib.request.urlopen(request) as response, temporary.open("wb") as output:
            while chunk := response.read(CHUNK_BYTES):
                output.write(chunk)
                digest.update(chunk)
    except Exception:
        temporary.unlink(missing_ok=True)
        raise
    actual_hash = digest.hexdigest()
    if actual_hash != expected_hash:
        temporary.unlink(missing_ok=True)
        raise PreparationError(
            f"downloaded archive SHA-256 {actual_hash} does not match {expected_hash}"
        )
    os.replace(temporary, destination)
    return False, time.perf_counter() - started


def validate_zip_members(archive: zipfile.ZipFile) -> None:
    for member in archive.infolist():
        path = pathlib.PurePosixPath(member.filename)
        mode = member.external_attr >> 16
        if path.is_absolute() or ".." in path.parts or not path.parts:
            raise PreparationError(f"unsafe ZIP member path: {member.filename!r}")
        if stat.S_ISLNK(mode):
            raise PreparationError(f"ZIP symlink is not allowed: {member.filename!r}")


def extract_dataset(archive_path: pathlib.Path, cache: pathlib.Path, dataset_name: str) -> pathlib.Path:
    destination = cache / "datasets" / archive_path.stem
    required = ("corpus.jsonl", "queries.jsonl", "qrels")
    if all((destination / item).exists() for item in required):
        return destination
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary_name:
        temporary = pathlib.Path(temporary_name)
        with zipfile.ZipFile(archive_path) as archive:
            validate_zip_members(archive)
            archive.extractall(temporary)
        extracted = temporary / dataset_name
        if not all((extracted / item).exists() for item in required):
            raise PreparationError(f"archive does not contain the expected {dataset_name} layout")
        if destination.exists():
            shutil.rmtree(destination)
        shutil.move(str(extracted), destination)
    return destination


def read_json_lines(path: pathlib.Path) -> Iterable[dict[str, Any]]:
    with path.open(encoding="utf-8") as stream:
        for line_number, line in enumerate(stream, start=1):
            try:
                row = json.loads(line)
            except json.JSONDecodeError as error:
                raise PreparationError(f"{path}:{line_number}: {error}") from error
            if not isinstance(row, dict):
                raise PreparationError(f"{path}:{line_number}: row must be an object")
            yield row


def load_beir(dataset_dir: pathlib.Path, split: str) -> tuple[list[tuple[str, str]], list[tuple[str, str]], dict[str, dict[str, float]]]:
    corpus: list[tuple[str, str]] = []
    for row in read_json_lines(dataset_dir / "corpus.jsonl"):
        identifier = row.get("_id")
        text = row.get("text")
        title = row.get("title", "")
        if not isinstance(identifier, str) or not isinstance(text, str) or not isinstance(title, str):
            raise PreparationError("BEIR corpus rows require string _id, title, and text")
        corpus.append((identifier, f"{title} {text}".strip()))

    qrels: dict[str, dict[str, float]] = {}
    qrels_path = dataset_dir / "qrels" / f"{split}.tsv"
    with qrels_path.open(encoding="utf-8") as stream:
        header = stream.readline().rstrip("\n").split("\t")
        if header != ["query-id", "corpus-id", "score"]:
            raise PreparationError(f"unexpected qrels header in {qrels_path}: {header}")
        for line_number, line in enumerate(stream, start=2):
            fields = line.rstrip("\n").split("\t")
            if len(fields) != 3:
                raise PreparationError(f"{qrels_path}:{line_number}: expected three columns")
            query_id, document_id, raw_score = fields
            score = float(raw_score)
            if not math.isfinite(score) or score <= 0.0:
                raise PreparationError(f"{qrels_path}:{line_number}: relevance must be positive")
            qrels.setdefault(query_id, {})[document_id] = score

    queries: list[tuple[str, str]] = []
    for row in read_json_lines(dataset_dir / "queries.jsonl"):
        identifier = row.get("_id")
        text = row.get("text")
        if isinstance(identifier, str) and isinstance(text, str) and identifier in qrels:
            queries.append((identifier, text))
    return corpus, queries, qrels


def validate_count(actual: int, expected: object, context: str) -> None:
    if actual != expected:
        raise PreparationError(f"{context} count {actual} does not match manifest {expected}")


def import_sentence_transformer(expected_version: str):
    try:
        import sentence_transformers
        from sentence_transformers import SentenceTransformer
    except ImportError as error:
        raise PreparationError(
            "sentence-transformers is required; install benchmarks/beir/requirements.txt"
        ) from error
    if sentence_transformers.__version__ != expected_version:
        raise PreparationError(
            f"sentence-transformers {sentence_transformers.__version__} is installed, expected {expected_version}"
        )
    return SentenceTransformer


def normalized_rows(matrix: Any, dimensions: int, context: str) -> list[list[float]]:
    if len(matrix.shape) != 2 or matrix.shape[1] != dimensions:
        raise PreparationError(f"{context} embeddings have unexpected shape {matrix.shape}")
    rows: list[list[float]] = []
    for index, vector in enumerate(matrix):
        values = [float(value) for value in vector]
        norm = math.sqrt(sum(value * value for value in values))
        if not all(math.isfinite(value) for value in values) or abs(norm - 1.0) > 1.0e-4:
            raise PreparationError(f"{context} embedding {index} is not a finite unit vector")
        rows.append(values)
    return rows


def write_json_lines(path: pathlib.Path, rows: Iterable[dict[str, Any]]) -> tuple[int, str]:
    count = 0
    temporary = path.with_suffix(path.suffix + ".part")
    with temporary.open("w", encoding="utf-8") as stream:
        for row in rows:
            stream.write(json.dumps(row, ensure_ascii=True, separators=(",", ":")))
            stream.write("\n")
            count += 1
    os.replace(temporary, path)
    return count, sha256_file(path)


def prepared_output_is_current(
    output: pathlib.Path, dataset: dict[str, Any], embedding: dict[str, Any]
) -> bool:
    prepared_path = output / "prepared-manifest.json"
    if not prepared_path.is_file():
        return False
    try:
        prepared = load_object(prepared_path)
        if prepared.get("dataset") != dataset or prepared.get("embedding") != embedding:
            return False
        artifacts = require_mapping(prepared.get("artifacts"), "prepared artifacts")
        for artifact in artifacts.values():
            artifact = require_mapping(artifact, "prepared artifact")
            path = output / str(artifact["path"])
            if not path.is_file() or sha256_file(path) != artifact.get("sha256"):
                return False
    except (KeyError, OSError, PreparationError):
        return False
    return True


def prepare(manifest_path: pathlib.Path, cache: pathlib.Path, output: pathlib.Path, force: bool) -> pathlib.Path:
    benchmark = load_object(manifest_path)
    validate_benchmark_manifest(benchmark)
    benchmark_hash = manifest_sha256(manifest_path)
    dataset = require_mapping(benchmark["dataset"], "dataset")
    embedding = require_mapping(benchmark["embedding"], "embedding")
    output.mkdir(parents=True, exist_ok=True)
    prepared_path = output / "prepared-manifest.json"
    if not force and prepared_output_is_current(output, dataset, embedding):
        print(f"BEIR preparation is current: {prepared_path}")
        return prepared_path

    archive_path = cache / "archives" / f"{dataset['name']}-{dataset['archive_sha256'][:12]}.zip"
    print(f"BEIR download: {dataset['url']}")
    archive_cache_hit, download_seconds = download_archive(
        dataset["url"], archive_path, dataset["archive_sha256"]
    )
    dataset_dir = extract_dataset(archive_path, cache, dataset["name"])
    corpus, queries, qrels = load_beir(dataset_dir, dataset["qrels_split"])
    validate_count(len(corpus), dataset["expected_corpus_count"], "corpus")
    validate_count(len(queries), dataset["expected_query_count"], "query")
    print(f"BEIR rows: corpus={len(corpus)} queries={len(queries)}")

    model_cache = cache / "models"
    model_cache.mkdir(parents=True, exist_ok=True)
    hugging_face_home = cache / "huggingface"
    os.environ["HF_HOME"] = str(hugging_face_home)
    os.environ["HF_XET_CACHE"] = str(hugging_face_home / "xet")
    transformer_type = import_sentence_transformer(embedding["package_version"])
    model = transformer_type(
        embedding["model"],
        revision=embedding["revision"],
        device=embedding["device"],
        cache_folder=str(model_cache),
        trust_remote_code=False,
    )
    model.max_seq_length = embedding["max_sequence_length"]
    if model.get_sentence_embedding_dimension() != embedding["dimensions"]:
        raise PreparationError("loaded model embedding dimension differs from the manifest")
    encode_options = {
        "batch_size": embedding["batch_size"],
        "normalize_embeddings": True,
        "convert_to_numpy": True,
        "show_progress_bar": True,
    }
    corpus_started = time.perf_counter()
    corpus_matrix = model.encode([text for _, text in corpus], **encode_options)
    corpus_seconds = time.perf_counter() - corpus_started
    query_started = time.perf_counter()
    query_matrix = model.encode([text for _, text in queries], **encode_options)
    query_seconds = time.perf_counter() - query_started
    corpus_vectors = normalized_rows(corpus_matrix, embedding["dimensions"], "corpus")
    query_vectors = normalized_rows(query_matrix, embedding["dimensions"], "query")

    document_numbers = {identifier: index + 1 for index, (identifier, _) in enumerate(corpus)}
    corpus_count, corpus_hash = write_json_lines(
        output / "corpus.jsonl",
        (
            {"id": index + 1, "source_id": identifier, "body": text, "embedding": vector}
            for index, ((identifier, text), vector) in enumerate(zip(corpus, corpus_vectors))
        ),
    )
    query_count, query_hash = write_json_lines(
        output / "queries.jsonl",
        (
            {
                "id": identifier,
                "text": text,
                "judgments": {
                    str(document_numbers[document_id]): score
                    for document_id, score in qrels[identifier].items()
                    if document_id in document_numbers
                },
                "embedding": vector,
            }
            for (identifier, text), vector in zip(queries, query_vectors)
        ),
    )
    prepared = {
        "schema_version": 1,
        "benchmark_manifest_sha256_at_preparation": benchmark_hash,
        "dataset": dataset,
        "embedding": embedding,
        "preparation": {
            "archive_cache_hit": archive_cache_hit,
            "download_seconds": download_seconds,
            "corpus_embedding_seconds": corpus_seconds,
            "query_embedding_seconds": query_seconds,
        },
        "artifacts": {
            "corpus": {"path": "corpus.jsonl", "rows": corpus_count, "sha256": corpus_hash},
            "queries": {"path": "queries.jsonl", "rows": query_count, "sha256": query_hash},
        },
    }
    temporary_manifest = prepared_path.with_suffix(".json.part")
    temporary_manifest.write_text(json.dumps(prepared, indent=2) + "\n", encoding="utf-8")
    os.replace(temporary_manifest, prepared_path)
    print(
        f"BEIR embeddings: corpus={corpus_seconds:.3f}s queries={query_seconds:.3f}s output={output}"
    )
    return prepared_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=pathlib.Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--cache", type=pathlib.Path, default=DEFAULT_CACHE)
    parser.add_argument("--output", type=pathlib.Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    try:
        prepare(args.manifest.resolve(), args.cache.resolve(), args.output.resolve(), args.force)
    except (OSError, ValueError, PreparationError, urllib.error.URLError, zipfile.BadZipFile) as error:
        print(f"BEIR preparation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
