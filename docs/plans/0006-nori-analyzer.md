# Nori analyzer implementation plan

Status: Active implementation

Update rule: Reconcile this plan in every Nori implementation PR that changes a task, public contract, dictionary input, verification result, or remaining gate. Completion requires the full [Nori design](../design/nori-analyzer.md), including consumers of morphological token graphs, rather than tokenizer compilation alone.

## Objective and fixed decisions

Implement the Lucene 10.5.1 Nori analyzer in native Rust for UQA Engine and its supported bindings. Preserve exact reference token attributes and dictionary interpretation, integrate positional graphs and source offsets into retrieval and highlighting, and make analyzer revisions durable and transactional. Keep the existing `standard_cjk` n-gram analyzer distinct.

The compatibility reference is Lucene commit `64ce863a2bea79c69c19c4d56268c26710ff0ff9` and the dictionary/JVM hashes in the [reference manifest](../../tests/parity/nori/manifest.json). Every JVM operation runs in Docker, including Java compilation, fixture generation, and dictionary export. Production execution is Rust. A changed reference requires a reviewed input/output diff and a new semantic fingerprint.

Work starts from UQA commit `e913303cd9ffd6662706053cc39caaaeb8f38b23` on `feature/nori-analyzer-design`, tracked by [PR #106](https://github.com/cognica-io/uqa-engine/pull/106). The existing 31 Docker reference cases establish examples, not complete UQA parity. Keep source changes and their verification in logical commits and update the PR description around the implemented scope.

## Work and dependencies

| Work item | Owner | Dependencies | Required result | Status |
| --- | --- | --- | --- | --- |
| Reference examples | `tests/parity/nori` | Pinned Docker and jars | Reproducible 31-case baseline with full token/end attributes and input hashes | Verified baseline |
| Source coordinates and character-edit maps | `uqa-analysis` | Existing character-filter behavior | Checked UTF-8/UTF-16 spans, original end state, and composable replacement/deletion/insertion maps | Verified; 144 analysis tests including 10 new integration cases and the source-map doctest |
| Rich token stream contracts | `uqa-analysis` | Source coordinates | Explicit token increments, lengths, end state, and metadata-preserving stage interfaces | Verified generic contract; Korean morphology attributes remain in the runtime item |
| Generic stage migration | `uqa-analysis` | Rich contracts | All existing tokenizers and filters preserve metadata; `analyze` remains the ordered term projection; explicit metadata policy for every expansion/removal | Verified; 160 analysis tests and strict package Clippy |
| Complete model export | Docker reference tools | Pinned resources | Every lexicon entry, entry order, context ID, cost, reading, morpheme, unknown class, matrix cell, and Unicode value exported and compared | Verified neutral export on Docker arm64 and amd64; repeated hashes and read-only exhaustive verification |
| Portable dictionary bundle | `uqa-analysis`, `uqa-nori-data` | Complete model export | Deterministic versioned packer, strict loader, content identity, shared immutable resources, corruption tests, and package provenance | Verified codec, full-model reconstruction, and data-crate archives; cross-binding delivery and measured defaults pending |
| Korean morphology runtime | `uqa-analysis::nori` | Rich contracts and bundle | User dictionary compilation, UTF-16 lattice, exact candidate/tie/backtrace behavior, all compound modes and unknown/punctuation options | Pending |
| Korean filters and normalization | `uqa-analysis` | Morphology runtime | POS stops, reading forms, simple Unicode lowercase, separate normalization entry point, optional exact-decimal Korean numbers | Pending |
| Compilation and resources | `uqa-analysis`, Engine composition | Generic stages and bundle | Immutable compiled handles, explicit resource resolver, canonical descriptors and bounded shared caches | Pending |
| Positional occurrence storage | Core/storage owners and SQLite provider | Rich contracts | Multiplicity, graph edges, offsets, explicit length policy, versioned codecs, atomic rebuilds, and provider conformance | Pending |
| Graph phrases and source highlighting | SQL, execution, operators, analysis | Occurrence storage and compilation | Whole-phrase analysis, graph path matching with holes, safe corrected source highlighting, and optimizer preservation | Pending |
| Catalog revisions | Native lifecycle owners and Engine state | Compilation and occurrence storage | Durable independent index/search descriptors, owner validation, name-collision preflight, atomic rebind, rollback/reopen/epoch tests | Pending |
| Public surfaces and bindings | SQL, facade, CLI, Python, Node.js, WASM | All runtime integration items | `nori` configuration, diagnostics, explicit-analyzer highlighting, real binding execution, feature-disabled failures, and bundled resources | Pending |
| Complete parity and release gates | Owning tests, CI, package tools | All preceding items | Exhaustive export checks, expanded differential fixtures, fuzz/regression coverage, measured benchmarks, licenses and publishable artifacts | Pending |

Implement the rich contracts and existing-stage migration first so Korean token emission has one real destination. Model export can advance independently of storage work. Introduce Nori registration only after the native tokenizer and its required filters consume the validated model. Do not make unsupported phases or providers return flattened terms under a Nori capability label.

## Contract checklist

- [x] Preserve existing term-only analyzer outputs, JSON tags and serialized aliases, fallible configuration behavior, and reloadable synonym-file semantics.
- [ ] Carry UTF-8 and UTF-16 source coordinates, original-source edit maps, graph increments and lengths, optional Korean morphology, keyword state, and final skipped positions.
- [ ] Reproduce every exported dictionary value and the reference's stable homograph ordering, user entry rules, Java classification, space penalties, integer/tie decisions, and bounded backtrace behavior.
- [ ] Support `none`, `discard`, and `mixed`, optional unknown unigrams and punctuation retention, POS filtering, Hanja readings, simple lowercase, normalization, and Korean number composition.
- [ ] Preserve same-position multiplicity and complete graph edges in every provider; declare normalization length policy and rebuild incompatible positional formats from source.
- [ ] Match actual quoted phrases and highlight original spans, including compound alternatives, removed-position holes, inflections, user nouns, Hanja, and character-filter replacements.
- [ ] Persist exact analyzer/resource revisions and independent index/search bindings, and prove atomic write/rebuild/rollback/reopen behavior across sessions.
- [ ] Execute equivalent scenarios through Rust, Python, Node.js, and browser WASM; verify missing-feature and missing-resource errors.
- [ ] Ship dictionaries and notices without auxiliary build-time or runtime dictionary downloads; validate all package archives and resource identities.
- [ ] Publish benchmark baselines and enforce recorded regression gates without claiming unmeasured throughput or memory results.

## Verification strategy

Each implementation commit records the commands actually run and what they prove. Pure analysis changes run the complete `uqa-analysis` test target and library tests, strict Clippy for the affected package, and formatting. Storage or retrieval changes add the owning provider/operator/SQL tests and actual Engine integration scenarios. Public manual SQL changes run the existing manual compilation/execution harness. A green reference run alone never proves that Rust matches Lucene.

```sh
python3 tests/parity/nori/run_reference.py
cargo test -p uqa-analysis --locked
cargo clippy -p uqa-analysis --all-targets --locked -- -D warnings
bash scripts/check-rustfmt.sh
python3 scripts/check-integration-test-harnesses.py
python3 scripts/check-workspace-dependencies.py
```

Add test modules under each crate's existing single integration target and preserve the original test inventory. A new dictionary data crate may have one integration target. Use minimized cross-boundary regressions for metadata corruption and failure atomicity rather than tests that merely repeat implementation details.

The expanded Docker oracle compares complete tokenizer, filter, analyzer, and stream-end output, not just term sets. Validate the entire exported model separately. Differential inputs cover long/ambiguous paths, all Unicode boundaries used by the model, user-dictionary corner cases, stacked tokens, and exact decimal behavior. Native and WASM runs must agree with the same fixtures. A changed expected result is permitted only when its pinned reference changes intentionally.

Before feature completion, run the change-aware pre-merge suites against the final remote PR head, actual binding examples, package-license checks, and package build/dry-run checks. Record the exact commands and artifacts. Benchmark dictionary decode and sharing, memory, native/WASM analysis, indexing, scoring, graph phrases, and cancellation with fixed corpora and host metadata.

## Progress ledger

| Checkpoint | Evidence | Remaining work |
| --- | --- | --- |
| Design and executable examples | Commits `78a82b3d` and `e913303c`; Docker reference contains 31 cases; PR Checks run `34667416290` passed for that design head | No UQA Nori implementation at that checkpoint |
| Original-source mapping | [`source.rs`](../../crates/uqa-analysis/src/source.rs), character-filter integration, and [`source_offsets.rs`](../../crates/uqa-analysis/tests/analysis/source_offsets.rs); `cargo test -p uqa-analysis --locked --quiet` passed 40 unit, 102 integration, and 2 doc tests; strict package Clippy passed | Rich token and analyzer pipeline propagation, morphology, and every downstream item remain active or pending |
| Generic token graph pipeline | [`token.rs`](../../crates/uqa-analysis/src/token.rs), migrated analyzer/tokenizer/filter execution, and [`rich_tokens.rs`](../../crates/uqa-analysis/tests/analysis/rich_tokens.rs); 44 unit, 113 integration, and 3 doc tests passed; complete `uqa-storage`, `uqa-storage-sqlite`, and `uqa-operators` tests passed; strict package Clippy and repository format/header/line/harness/dependency checks passed; the 31-case Docker arm64 reference was reproduced with cached offline inputs | Korean morphology and model export, graph-aware consumers, durable revisions, bindings, and the remaining release gates |
| Engine consumers of migrated analysis | Existing `uqa-engine` integration filters `analyzer`, `highlight`, and `fts` passed 22, 9, and 37 tests respectively; `queries::manual_sql_examples::manual_sql_examples_compile_or_execute` passed | Existing term-based behavior is verified; positional storage and morphological search remain pending |
| Complete neutral model export | [`NoriModel.java`](../../tests/parity/nori/NoriModel.java), [format and commands](../../tests/parity/nori/MODEL.md), and [reviewed manifest](../../tests/parity/nori/model_manifest.json); 774,582 surfaces, 816,283 system entries, 14 unknown entries, 10,292,646 matrix cells, 65,536 character definitions, and 1,114,112 Unicode values exported and compared in Docker; independent arm64 and amd64 exports reproduced all five hashes; read-only verification passed and direct JVM verification rejected corruption in each of the five files; all 133 repository tooling tests passed, including six new model-export regressions | Rust packer/loader and analysis parity, original CSV regeneration, distributable resources, and remaining consumers |

| Portable runtime codec | [Format and commands](../design/nori-bundle-format.md); 9,829,534-byte bundle with artifact SHA-256 `0d920523991ed60909972d85df630c65747ff5d49e1138e4998ca39f0079538d`; arm64 and amd64 neutral inputs packed identically, both passing all five reconstructed hashes; standalone verification passed; `cargo test -p uqa-analysis --features nori-tools --locked --offline --quiet` passed 63 unit, 113 integration, and 3 doc tests; loader-only feature passed 60/113/3 and feature-disabled passed 44/113/3; strict Clippy passed for tools and loader-only builds | Bundled data distribution, shared resource interning, native morphology, actual WASM execution, CSV regeneration, and all downstream work remain pending |

| Bundled dictionary distribution | [`uqa-nori-data`](../../crates/uqa-nori-data/README.md) includes the exact bundle, model/resource manifests, and upstream Lucene, MeCab, and Unicode notices; the analysis integration harness reconstructs all five neutral hashes from embedded bytes; data-crate identity test passed; all 23 Cargo archives passed offline packaging and license checks; the 9,682,047-byte data archive passed native Cargo verification and an extracted-library WASM build; 135 tooling tests, strict package Clippy, and repository policy checks passed | Actual WASM decoding/analysis, binding-specific delivery, default resource composition and interning, CSV regeneration, and all morphology/retrieval/lifecycle work remain pending |

| Optional-feature CI coverage | Linux/macOS Rust test jobs explicitly execute `uqa-analysis/nori-tools` and the embedded model; both Clippy jobs explicitly lint the optional codec/tool paths alongside the default workspace; the equivalent focused commands passed locally | Remote full Rust suites remain a pre-merge gate; ordinary PR Checks are policy/format checks |

Never mark a work item verified based on intent, parser acceptance, a narrow test from another owner, or a successful compilation alone. Keep incomplete requirements visible here until their authoritative evidence is recorded.
