# Kuromoji analyzer implementation plan

Status: Active implementation in [PR #108](https://github.com/cognica-io/uqa-engine/pull/108), based on UQA 0.3.0. The shared dictionary byte reader/writer, UTF-16 lexicon and builder, and connection-cost matrix are implemented in `uqa-analysis::morphology` and used by Nori. Japanese tokenization, dictionary packaging, analyzer integration, and delivery are incomplete. This plan does not certify Japanese parity or performance. The tagged 0.3.0 release is independent of this PR.

Update rule: Update this plan with every logical implementation unit that changes ownership, interfaces, reference inputs, completed work, remaining work, or acceptance evidence. Update the PR description and push each verified commit. Record final remote CI outcomes in the PR so recording a successful run does not change the commit it checked. A completed helper or passing constructor is not completion of its larger work item.

## Objective and constraints

Implement the complete [Kuromoji design](../design/kuromoji-analyzer.md): Lucene 10.5.1 Japanese tokenization, all documented analysis components, separate normalization and completion, immutable revisions, existing retrieval consumers, and actual Rust/Python/Node.js/WASM delivery. Preserve generic and Nori results, public Korean attributes, dictionary identities, canonical descriptors, and provider behavior while sharing proven mechanisms.

The reference is Lucene commit `64ce863a2bea79c69c19c4d56268c26710ff0ff9`. The design records the inspected official artifacts and exact defaults. Every JVM invocation, including Java compilation, resource regeneration, model export, and differential verification, must run in Docker. Production execution and packaging must not download dictionaries or require a JVM.

Before each implementation or move, inspect the affected Cargo manifests and enabled features, the [dependency policy](../../scripts/workspace-dependency-policy.json), the [manual ownership map](../manual/internals/04-analyzer-pipeline.md), and existing implementations/tests. Keep analysis algorithms in `uqa-analysis`; the new data crate owns immutable bytes and notices. Engine only supplies existing state, session, transaction, and retained-resource adapters. A private shared module must not import a language module, and Kuromoji must not import Nori.

## Execution order and acceptance ledger

The rows identify reviewable work units and their dependencies. Independent reference preparation can run while source checks or the separate release are pending. Runtime work follows its required representations and resources; do not register an incomplete Japanese analyzer to make an integration test pass.

| Work unit | Owner and dependencies | Completion evidence | Current state |
| --- | --- | --- | --- |
| Shared dictionary primitives | `uqa-analysis::morphology`; existing Nori codec and lexicon | Nori uses one shared reader/writer, lexical-rank builder/lookup, and cost matrix; original errors, full model hashes, fixtures, and controls pass | Implemented and locally verified in `a28199b6`; details below |
| CJK width character filtering | `uqa-analysis::char_filter`; existing source edits and memory owner | Restricted fullwidth ASCII/halfwidth kana conversion, voiced-mark composition, exact corrected UTF-16/UTF-8 spans, chained edits, cancellation and retained budgets agree with the pinned character-filter oracle | Next implementation unit; source and existing edit interfaces inspected |
| Pinned Japanese reference and model export | `tests/parity/kuromoji`; pinned Docker image, jars, source resources | Complete input inventory and hashes; source-generation recipe; streamed export and read-only comparison of every surface, ordered word, attribute, matrix cell, unknown class, Unicode property, stop set, and completion mapping | Artifact/source inspection exists in the design; exporter, generation hashes, and verification remain |
| Remaining shared morphology mechanisms | `uqa-analysis::morphology`; correspondence between both language implementations | Shared Unicode profiles, frame codec, resource/cache ownership, bounded lattice, and exact-decimal machinery where both consumers need them; Nori byte/descriptor/control compatibility remains unchanged | Not implemented beyond dictionary primitives; extract at each Japanese consumer's introduction, without duplicate algorithms or unused abstractions |
| Japanese bundle, loader, and resources | `uqa-analysis::kuromoji`, `uqa-kuromoji-data`; complete model and common codecs | Separate schema and identity; exhaustive reconstructed model equality; malformed/truncated/oversized/hash-invalid inputs rejected before publication; bounded resource caches and complete licenses | Not implemented |
| Japanese user dictionary | `uqa-analysis::kuromoji`; model and shared lexicon | Exact CSV quoting, comments, duplicates, segmentation/readings/POS, word order/IDs, invalid input and preparation-limit behavior compared with Lucene | Not implemented |
| Japanese tokenizer and rich token bridge | `uqa-analysis::kuromoji`, shared lattice and token/source owners; model and user rules | NORMAL/SEARCH/EXTENDED, both discard flags, known/unknown/user precedence, penalties, forced backtrace, resegmentation, ordered graph output, all six Japanese morphology attributes and terminal state match reference | Not implemented |
| N-best analysis | `uqa-analysis::kuromoji`; validated tokenizer and bounded candidate representation | Cost and example-derived settings, combined maximum, invalid/empty examples, path deduplication, fixups, deterministic ordering, cancellation and bounded output agree with Lucene | Not implemented; Nori and Japanese single-path calls must not allocate N-best state |
| Default Japanese analyzer | `uqa-analysis`; tokenizer, width stage, rich attributes and pinned profiles | Width, SEARCH with both discard flags, base form, POS stops, exact Japanese stopwords, Katakana stemmer and Java simple lowercase match the complete analyzer oracle | Not implemented |
| Optional filters and completion | `uqa-analysis`; rich stream, shared decimal/ownership mechanisms and exact reference resources | Reading/kana/romaji, Japanese numbers, iteration marks, small-kana expansion, completion INDEX/QUERY, keyword behavior, graph/source/end attributes and absent morphology rules match separate component oracles | Not implemented; reading romaji and completion romanization remain distinct algorithms |
| Normalization and durable compilation | `uqa-analysis`; components, typed resources and profiles | Explicit ordinary Japanese width-plus-simple-lowercase and completion width-only plans; canonical configuration/defaults/resource hashes; snapshot restoration; legacy generic/Nori descriptor bytes and fingerprints unchanged | Not implemented; document the planned Rust configuration source migration when it lands |
| Provider and retrieval integration | Existing storage/provider, operators, scoring and analysis owners; compiled Japanese revisions | Complete occurrences, overlap length policy, exact terms, scores and bounds, phrase paths and original-source highlighting agree across Memory/SQLite/redb and relevant search paths | Not implemented; reuse occurrence v2/schema 48 unless an actual representational deficiency is demonstrated |
| SQL lifecycle and runtime controls | `uqa-sql`, `uqa-execution`, existing Engine adapters; compiled revisions and provider support | Diagnostics, normalization, registration/rebinding, independent index/search revisions, prepared queries, savepoints, rollback, sibling sessions, reopen/backup restore and recovery preserve state; cancellation `57014`, allocation failure `53200` | Not implemented |
| Feature isolation and bindings | Cargo owners, facade/CLI/Python/Node.js/WASM; runtime/public contracts | Neither/Nori-only/Kuromoji-only/both compile and execute; normal dependency trees exclude disabled data; same durable SQL scenarios execute through actual bindings and browser IndexedDB reopen | Not implemented; add exact forwarding/allowlist edges when introducing the data crate |
| Package and final PR acceptance | Existing CI, legal and publication tools; completed runtime/integration | Final-head required CI, owner checks, manual examples, source/wheel/npm/WASM inventory, embedded dictionary bytes, notices, package order and documented upgrade behavior verified | Not implemented; PR remains draft during implementation |

## Logical commits and document updates

Commit mechanical Nori extraction separately from Japanese semantic changes. A runtime unit includes its owning tests, required feature/dependency updates, and current contract documentation. A dictionary unit includes its provenance and package notices. Avoid committing temporary experiments or cleanup-only chains that obscure the final implementation. Push each completed logical unit and update this ledger when its acceptance status changes.

Keep this ledger current instead of appending repetitive chronological reports. The design defines the intended interfaces and invariants; this plan records what is implemented, its dependencies, and outstanding gates; the manual describes verified public behavior. When an implementation decision changes, update the affected design and plan together. Add public manual examples only with their existing compilation/execution checks.

## Compatibility and completion checklist

- [x] Move checked binary I/O, minimized UTF-16 lexicon construction/lookup/enumeration, and connection costs into their analysis owner without reverse dependencies or Cargo graph changes.
- [x] Preserve Nori public dictionary error variants and offsets; pass existing full-model, token/filter, descriptor, cancellation, memory and source-projection tests after the extraction.
- [ ] Prove Nori packed-byte reproduction and legacy descriptor identity through all remaining shared-resource, token and normalization refactors; keep `UQANORI\0`, its seven sections, version 1 and hash domains unchanged.
- [ ] Pin and exhaustively verify the complete Japanese model and resources using Docker; no Rust parity claim is made from source inspection or oracle execution alone.
- [ ] Match full ordered tokenizer/filter/analyzer output, lossless UTF-16, six Japanese attributes, source/graph coordinates, keyword behavior, terminal state and error outcomes, including N-best and completion.
- [ ] Carry one runtime allowance and cancellation callback through input, lattice, alternatives, filters, diagnostics and highlighting; preserve unrelated leases and committed state on every failure.
- [ ] Preserve independent persistent revisions, provider graph/score behavior, original-source highlighting, transaction/savepoint/reopen and real backup restoration.
- [ ] Execute the shared public contract through actual Rust, Python, Node.js and browser WASM artifacts with all four language feature configurations covered at the appropriate owners.
- [ ] Verify final PR-head CI, complete packaged resource/notices inventory, and public/upgrade documentation before marking this work complete.

## Verification and evidence retention

For shared analysis changes, run the owning analysis tests and required Nori reference/model regressions, strict package Clippy, relevant feature configurations, formatting and ownership/dependency checks. Add tests to the owning crate's existing test executable; do not create extra top-level integration targets. Extend to provider, SQL, binding or packaging suites when those boundaries change. Successful compilation is not runtime or parity evidence. Do not repeatedly rerun a passing suite unless a subsequent change or unresolved failure requires it.

Use fixed and seeded differential cases covering compound/inflection ambiguity, unknown group/invoke classes, forced-backtrace boundaries, surrogate handling, halfwidth kana and voiced marks, iteration marks, user CSV corner cases, stopped positions, numeric lookahead, N-best graph geometry, and completion modes. Compare complete outputs and errors against the pinned reference and retain minimized regressions. Test standalone components separately from default analyzer chains and normalization. Never generate expected fixtures from the candidate Rust implementation.

Routine CI gates deterministic semantics, model identities, counts, allocation limits, cleanup, and bounded cancellation work. Uncontrolled workstation/shared-runner timing is not a completion gate. No timing collection is scheduled by this plan. A later timing study requires a stated question, qualified host/noise bound, fixed input, maximum runs, stopping condition and compact result schema before execution; stop if the environment fails qualification.

Track only compact manifests, deterministic bounded fixtures, hashes and conclusions. Export the full model to temporary binary files and compare it as a stream. Keep raw measurement reports outside Git; remove temporary exports after the verified packed artifact and compact evidence exist. Do not accumulate multi-million-line JSON or repeat measurement collection while implementation and release work wait.

## Current verified evidence

Commit `a28199b6` extracts dictionary primitives into `crates/uqa-analysis/src/morphology/` and migrates all Nori consumers and owning tests. It adds no crate or Cargo feature/dependency edge. The algorithms and encoded field order are unchanged; Nori translates shared structural errors into its existing public error variants. Its bundle frame/schema, Korean character tables and morphology interpretation remain language-owned.

The complete analysis/data run passed 169 library tests, 221 analysis integration tests, one data archive test and 20 documentation tests. This includes reconstruction against every pinned neutral-model hash, embedded bundle hash and dictionary identity, existing generic/Nori descriptors, native/common output, source mapping, memory and cancellation coverage. After extracting the neutral character-table reader to satisfy the existing function-size lint, the affected Nori tests passed again: 34 library tests and 51 integration tests. This is functional evidence, not a throughput measurement.

```sh
cargo test -p uqa-analysis -p uqa-nori-data --features uqa-analysis/nori-tools --locked
cargo test -p uqa-analysis --features nori-tools --locked nori
cargo clippy -p uqa-analysis --features nori-tools --all-targets --locked -- -D warnings
cargo clippy -p uqa-analysis --no-default-features --all-targets --locked -- -D warnings
python3 scripts/check-workspace-dependencies.py
python3 scripts/check-workspace-dependencies.py --staged
python3 scripts/check-engine-capabilities.py
python3 scripts/check-integration-test-harnesses.py
python3 scripts/check-rust-file-lines.py
git diff --cached --check
```

All listed checks passed for that implementation unit; `cargo fmt --all` was applied. The dependency policy reports 108 runtime edges across 32 crates; Engine ownership reports 103 adapters and 1,244 Engine-free leaves; integration coverage remains 217 sources in 21 targets. Remote CI for implementation commits is tracked in [PR #108](https://github.com/cognica-io/uqa-engine/pull/108). Earlier documentation-only CI does not certify runtime changes.
