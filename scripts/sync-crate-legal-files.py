#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Copy canonical AGPL legal files into every publishable workspace crate."""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import subprocess
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
LEGAL_FILES = (
    "LICENSE",
    "LICENSING.md",
    "LICENSE-NOTICE.md",
    "LICENSES/UQA-FOSS-EXCEPTION-1.0.txt",
    "LICENSES/UQA-NONCOMMERCIAL-EXCEPTION-1.0.txt",
)
SKIP_PACKAGES = {"uqa-pg-query"}
USER_FACING = {"uqa", "uqa-engine", "uqa-client", "uqa-cli", "uqa-api"}
CRATE_ROLES = {
    "uqa-core": "document sets, finite-support relations, posting storage, and value types",
    "uqa-analysis": "tokenizers, character filters, token filters, and analyzers",
    "uqa-storage": "document, inverted, vector, catalog, and key/value storage contracts",
    "uqa-storage-redb": "the redb implementation of the ordered key/value contract",
    "uqa-storage-sqlite": "the SQLite implementation of the ordered key/value contract",
    "uqa-scoring": "BM25, Bayesian BM25, WAND, calibration, and parameter learning",
    "uqa-fusion": "Bayesian evidence fusion and multi-signal retrieval pooling",
    "uqa-operators": "retrieval, Boolean, hybrid, staged, and fusion operators",
    "uqa-ml": "model specifications, CPU inference, and optional MLX backends",
    "uqa-graph": "named graphs, Cypher, regular path queries, and graph algorithms",
    "uqa-joins": "relational and cross-paradigm join algorithms",
    "uqa-planner": "cardinality, cost, DPccp join enumeration, and unified-plan optimization",
    "uqa-execution": "physical operators, batches, spill, sorting, and joins",
    "uqa-sql": "PostgreSQL-oriented SQL compilation on the imported libpg_query pin",
    "uqa-pg-wire": "network-independent PostgreSQL v3 message parsing and encoding",
    "uqa-fdw": "foreign-table contracts and DuckDB, Arrow, and memory handlers",
}


def workspace_packages() -> list[dict[str, object]]:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    metadata = json.loads(result.stdout)
    members = set(metadata["workspace_members"])
    return [package for package in metadata["packages"] if package["id"] in members]


def crate_root(package: dict[str, object]) -> pathlib.Path:
    manifest = pathlib.Path(str(package["manifest_path"]))
    return manifest.parent


def is_publishable(package: dict[str, object]) -> bool:
    publish = package.get("publish")
    if publish is None:
        return True
    if publish is False:
        return False
    if isinstance(publish, list) and len(publish) == 0:
        return False
    return True


def copy_legal_files(destination: pathlib.Path) -> None:
    notice_source = ROOT / "crates" / "uqa-node" / "LICENSE-NOTICE.md"
    for relative in LEGAL_FILES:
        if relative == "LICENSE-NOTICE.md":
            source = notice_source
        else:
            source = ROOT / relative
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)


def write_internal_readme(destination: pathlib.Path, name: str) -> None:
    role = CRATE_ROLES.get(name, "an internal UQA-RS component")
    destination.joinpath("README.md").write_text(
        (
            f"# {name}\n"
            "\n"
            f"`{name}` is the UQA-RS crate for {role}.\n"
            "\n"
            "Applications should depend on `uqa-engine` or `uqa-client`. See the "
            "[repository README](https://github.com/cognica-io/uqa-engine) and the "
            "[manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).\n"
        ),
        encoding="utf-8",
    )


def write_user_readme(destination: pathlib.Path) -> None:
    shutil.copyfile(ROOT / "README.md", destination / "README.md")


def check_legal_files(destination: pathlib.Path) -> None:
    notice_source = (ROOT / "crates" / "uqa-node" / "LICENSE-NOTICE.md").read_bytes()
    for relative in LEGAL_FILES:
        path = destination / relative
        if not path.is_file():
            raise RuntimeError(f"{path} is missing")
        if relative == "LICENSE-NOTICE.md":
            expected = notice_source
        else:
            expected = (ROOT / relative).read_bytes()
        if path.read_bytes() != expected:
            raise RuntimeError(f"{path} differs from the canonical legal file")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify crate-local legal files instead of writing them",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    checked = 0
    for package in workspace_packages():
        name = str(package["name"])
        if name in SKIP_PACKAGES or not is_publishable(package):
            continue
        destination = crate_root(package)
        if args.check:
            check_legal_files(destination)
            readme = destination / "README.md"
            if not readme.is_file():
                raise RuntimeError(f"{readme} is missing")
            if name in USER_FACING and readme.read_bytes() != (ROOT / "README.md").read_bytes():
                raise RuntimeError(f"{readme} differs from the repository README")
        else:
            copy_legal_files(destination)
            if name in USER_FACING:
                write_user_readme(destination)
            else:
                write_internal_readme(destination, name)
        checked += 1
    mode = "checked" if args.check else "updated"
    print(f"Crate legal files {mode}: {checked} publishable crates")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        raise SystemExit(1) from error
