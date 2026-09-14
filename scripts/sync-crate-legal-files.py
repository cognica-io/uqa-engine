#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Copy canonical AGPL legal files into every publishable workspace crate."""

from __future__ import annotations

import argparse
import importlib.util
import json
import pathlib
import re
import shutil
import subprocess
import sys
from urllib.parse import unquote, urlsplit


ROOT = pathlib.Path(__file__).resolve().parents[1]
LICENSE_SPEC = importlib.util.spec_from_file_location("uqa_release_licenses", ROOT / "scripts/check-release-licenses.py")
licenses = importlib.util.module_from_spec(LICENSE_SPEC)
LICENSE_SPEC.loader.exec_module(licenses)
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
    "uqa-nori-data": "the pinned portable Nori dictionary and its provenance",
    "uqa-kuromoji-data": "the pinned portable Lucene 10.5.1 Japanese dictionary and its provenance",
    "uqa-core": "document sets, finite-support relations, posting storage, and value types",
    "uqa-analysis": "tokenizers, character filters, token filters, and analyzers",
    "uqa-storage": "provider-independent storage contracts, shared codecs, and in-memory data structures",
    "uqa-storage-redb": "the redb implementation of the ordered key/value contract",
    "uqa-storage-sqlite": "SQLite connections, catalogs and migrations, document and retrieval indexes, transactions, graph persistence, key/value storage, encryption, and compressed VFS",
    "uqa-scoring": "BM25, Bayesian BM25, WAND, calibration, and parameter learning",
    "uqa-fusion": "Bayesian evidence fusion and multi-signal retrieval pooling",
    "uqa-operators": "retrieval, Boolean, hybrid, staged, and fusion operators",
    "uqa-ml": "model specifications, CPU inference, and an experimental MLX probe",
    "uqa-graph": "named graphs, Cypher, regular path queries, and graph algorithms",
    "uqa-joins": "relational and cross-paradigm join algorithms",
    "uqa-planner": "cardinality, cost, DPccp join enumeration, and unified-plan optimization",
    "uqa-execution": "statement execution, mutation and schema scheduling, physical retrieval, batches, spill, sorting, and joins",
    "uqa-sql": "PostgreSQL 18 SQL parsing, shared plans, static schema binding, type resolution, and value expressions",
    "uqa-pg-wire": "network-independent PostgreSQL v3 message parsing and encoding",
    "uqa-fdw": "foreign-table contracts and DuckDB, Arrow, and memory handlers",
}

CRATE_NOTES = {
    "uqa-analysis": 'The optional `nori` feature exposes a validated immutable Korean dictionary, user-rule compiler, and native rolling Viterbi tokenizer with lossless UTF-16 morphology and graph attributes. `nori-tools` adds the offline packer and complete neutral-model verifier. The standalone tokenizer, default Korean analyzer, POS/reading/simple-lowercase filters, optional exact-decimal number composition, and separate normalization are verified against pinned Lucene fixtures. The native common-token bridge preserves raw terms, morphology, graph/end state, and corrected source spans through generic filters. Korean filters share one implementation across native and common tokens, including absent morphology, retained source provenance, and exact composed spans. `NoriResources` resolves immutable bundles by exact artifact hash, shares bounded dictionary and user-rule caches, and uses the bundled data through the optional `nori` feature without implicit I/O. `Analyzer::compile()` prepares immutable existing pipelines with fixed expressions, stop sets, and synonym maps while uncompiled APIs keep file reload behavior. `AnalyzerDescriptor` snapshots canonical resolved inputs and runtime profiles under a versioned fingerprint; `AnalyzerResources` restores those snapshots and shares compiled revisions under bounded cache ownership. Korean tokenizer/filter configurations now compile through the common pipeline with exact resource identities, user-rule snapshots, normalization, and bounded shared ownership. Memory, SQLite, and redb retain exact durable analyzer revisions and complete token graphs; public SQL and bindings execute graph phrases and highlight original source spans. Ported code retains the Lucene license, notice, and modification attribution in `THIRD-PARTY/`. See the [analyzer reference](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/reference/06-text-analyzers.md#standalone-korean-tokenization) and [bundle format](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/nori-bundle-format.md).\n\nThe independent optional `kuromoji` feature exposes `KuromojiDictionary` and the bundled Japanese dictionary from `uqa-kuromoji-data`. It validates the complete immutable model, including all six Japanese morphology attributes, unknown classes, connection costs, Java Unicode values, default stop sets and completion mappings. `KuromojiResources` resolves exact immutable bundles and interns Japanese user rules with bounded retention; its host resolver has no implicit filesystem or network fallback. `AnalyzerResources::builder` can install both typed language resource owners together, and the existing Nori constructor remains available. `kuromoji-tools` provides an offline packer and exhaustive comparison with the pinned Docker export. Generic and Nori configurations do not acquire the Japanese data dependency unless `kuromoji` is enabled. Native `JapaneseTokenizer` exposes NORMAL/SEARCH/EXTENDED modes, punctuation/compound choices, six morphology attributes and exact graph/end state with bounded resegmentation, cancellation and retained memory. All 209 pinned tokenizer cases match. The common token bridge shares source correction, graph validation and allocation transfer with Nori. `JapaneseMorphology` preserves all six optional attributes through generic token filters, and `tokenize_mapped_budgeted` retains original spans after character filtering. Native N-best cost/example selection matches 98 additional Docker cases, including signed costs, stable alternative graphs, lazy attributes and preparation errors. Byte/count/work allowances and cancellation cover its independent scratch; non-positive costs allocate no alternative arrays. Standalone `JapaneseAnalyzer` and its base-form, exact POS and word stops, Katakana stemmer and simple-lowercase filters, plus optional Hiragana/Katakana small-kana expansion and kana/romaji reading conversion, match 195 additional Docker cases including every UTF-16 unit and the complete Katakana reading context matrix. Native/common streams share one filter algorithm and retain source, graph and terminal attributes. Reading conversion preserves absent/empty attributes and uses bounded modified-Hepburn lookahead separately from completion romanization. Optional `JapaneseFilter::Number` and the scalar/raw UTF-16 number normalizers use shared exact decimals, prefix parsing and retained lookahead streams with Japanese numeral and attribute policies. Their separate 220-case Docker corpus verifies exact numeric grammar, all UTF-16 units in two contexts, graph/terminal propagation, long coefficients and source spans; bounded outputs, cancellation and retained leases are tested on both native and common tokens. Custom chains preserve lazy user-field access errors, and normalization applies only width and simple lowercase. Lookup preparation and runtime mutation have explicit memory/count limits and cancellation. `CharFilter::KuromojiIterationMark` adds independently configurable kanji/kana expansion with exact original-source span and voicing behavior, verified by 189 Docker text/offset cases including every BMP scalar with all five marks under all four flag settings. It uses the common reserved character-edit owner and participates in immutable generic pipeline descriptors. Ordered completion romanization now matches 396 Docker cases through the existing lexical-rank owner, with complete-product counts, retained result buffers and explicit work limits. Completion INDEX/QUERY streams and a dedicated analyzer constructor now bring the complete corpus to 522 cases. They reset graph/keyword/morphology state through owning token adapters, retain terminal/source behavior and select width-only normalization separately from the lowercase analysis chain. Native generated origins and common morphology are absent. Explicit normalization configurations now compile and restore with typed exact profile hashes, separate width-only or width-plus-simple-lowercase plans, strict properties and unchanged legacy omission. Native and compiled normalization share one reserved scalar conversion owner; dictionary aliases resolve once per language pipeline and retained handles preserve their original profiles. Japanese tokenizer configurations now compile through common pipelines and restore exact dictionary/user snapshots and effective N-best costs without repeating example probes. Native/common attribute failures remain deferred through filters, and linear storage adapters reject Japanese graphs. Japanese token-filter configuration, built-ins and binding integration remain in development; the [bundle format](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/kuromoji-bundle-format.md) describes this dictionary API.\n\n`kuromoji::UserDictionary` compiles Japanese CSV against a selected immutable model, preserving exact source identity, phrase/word order, readings/POS, segmentation and overlapping longest lookup. It shares preparation limits and lexical traversal with Nori while retaining Japanese parsing and duplicate errors. Its public API and executed example are in the `kuromoji` module; 62 pinned Docker cases verify complete user-rule outputs and failures independently of the Japanese tokenizer. See the [standalone Japanese tokenization reference](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/reference/06-text-analyzers.md#standalone-japanese-tokenization) for the native runtime API.',
    "uqa-nori-data": 'The Rust wrapper uses the workspace license. The converted dictionary retains its upstream notices in `THIRD-PARTY/`, including the complete Lucene license and notice, MeCab-ko-dic COPYING, and the pinned JDK Unicode notice. `data/resource_manifest.json` records hashes for the bundle, original export manifest, and attribution files. Conversion changes storage layout while preserving the exported model values. This crate exposes immutable bytes only; the optional analysis feature resolves them through `NoriResources`. The optional runtime and official Python, Node.js, and WASM packages use this exact embedded bundle; each binding carries its upstream notices and source-resource manifests. See the [bundle format and regeneration commands](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/nori-bundle-format.md).',
    "uqa-kuromoji-data": 'This no_std crate exposes immutable bytes with no runtime dependencies, build script, dictionary download or JVM requirement. Validate the bytes through `uqa_analysis::kuromoji::KuromojiDictionary::from_bytes` with the `kuromoji` feature.\n\nThe Rust wrapper uses the workspace license. The converted dictionary retains its upstream notices in `THIRD-PARTY/`: complete Lucene LICENSE/NOTICE, original IPADIC COPYING including its unchanged terminal bytes, and the pinned JDK Unicode notice. Conversion changes storage layout and preserves every exported model value. `data/resource_manifest.json` records the exact bundle, model manifest and attribution hashes. The [reference tools](https://github.com/cognica-io/uqa-engine/tree/main/tests/parity/kuromoji) describe source reproduction and complete model verification; the [implementation plan](https://github.com/cognica-io/uqa-engine/blob/main/docs/plans/0007-kuromoji-analyzer.md) tracks tokenization, analyzer integration and binding delivery.',
    "uqa-storage": "Concrete SQLite implementations belong to `uqa-storage-sqlite`; the common storage crate has no runtime dependency on a database provider.\n\nShared occurrence staging and versioned codecs preserve lossless term keys, token graph edges, source offsets, multiplicity, and independent normalization lengths. Memory indexes store these occurrences and original stream-end/revision metadata, expose exact-key lookups, and require an atomic source rebuild to change a populated field's index revision. SQLite and redb retain the same graph representation and migrate incompatible source-backed indexes on open; the [format contract](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/occurrence-posting-format.md) describes the durable occurrence metadata.\n\nMemory and Key/Value analysis retains immutable compiled index/search revisions. Revision rebuilds publish the candidate binding with replacement postings only after successful staging and storage publication; durable descriptor restoration resolves exact bundle and user-rule identities.",
    "uqa-storage-sqlite": "Import concrete types such as `ManagedConnection`, `SQLiteStorageProvider`, `SQLiteCompressionOptions`, `SQLiteError`, and `SQLiteGraphStore` from `uqa_storage_sqlite`. This provider implements the backend-neutral contracts in `uqa-storage` and `uqa-graph`. See the [Rust migration notes](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/reference/10-upgrading.md#sqlite-provider-ownership) for the previous import paths and error-handling changes.",
    "uqa-graph": "Memory graph stores and the backend-neutral persistent graph contract live here. The standalone `SQLiteGraphStore` adapter lives in `uqa-storage-sqlite`; graph algorithms do not depend on a SQLite driver or provider.",
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
    role = CRATE_ROLES.get(name, "an internal UQA Engine component")
    notes = CRATE_NOTES.get(name, "")
    destination.joinpath("README.md").write_text(
        (
            f"# {name}\n"
            "\n"
            f"`{name}` is the UQA Engine crate for {role}.\n"
            "\n"
            + (f"{notes}\n\n" if notes else "")
            + "Applications should depend on `uqa-engine` or `uqa-client`. See the "
            "[repository README](https://github.com/cognica-io/uqa-engine) and the "
            "[manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).\n"
        ),
        encoding="utf-8",
    )


def user_readme(version: str) -> str:
    """Resolve public README links to main for development or the release tag."""
    source = (ROOT / "README.md").read_text(encoding="utf-8")
    prerelease = version.partition("-")[2]
    ref = "main" if prerelease.split(".", 1)[0] == "dev" else f"v{version}"

    def repository_link(match: re.Match[str]) -> str:
        """Resolve relative links against the selected repository ref."""
        target = match.group(1)
        parsed = urlsplit(target)
        if parsed.scheme or target.startswith(("/", "#")):
            return match.group(0)
        kind = "tree" if (ROOT / unquote(parsed.path)).is_dir() else "blob"
        return f"](https://github.com/cognica-io/uqa-engine/{kind}/{ref}/{target})"

    return re.sub(r"\]\(([^)\s]+)\)", repository_link, source)


def write_user_readme(destination: pathlib.Path, version: str) -> None:
    (destination / "README.md").write_text(user_readme(version), encoding="utf-8")


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
    if args.check:
        licenses.check_binding_sources()
    else:
        for directory in licenses.BINDING_PACKAGES:
            for relative, payload in licenses.binding_nori_payloads().items():
                path = directory / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(payload)
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
            if name in USER_FACING and readme.read_text(encoding="utf-8") != user_readme(str(package["version"])):
                raise RuntimeError(f"{readme} differs from the versioned repository README")
        else:
            copy_legal_files(destination)
            if name in USER_FACING:
                write_user_readme(destination, str(package["version"]))
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
