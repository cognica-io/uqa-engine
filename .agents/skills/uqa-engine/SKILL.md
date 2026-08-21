---
name: uqa-engine
description: Author, explain, review, or verify UQA Engine SQL and manual changes. Use for UQA Engine relational SQL, full-text analyzers, BM25, vector or hybrid retrieval, operator joins, graph/Cypher queries, language bindings, and documentation examples; also use when deciding whether a requested PostgreSQL-shaped feature is implemented by UQA Engine.
---

# UQA Engine

Use the repository manual as the product contract and verify executable claims against the implementation.

## Load the smallest authoritative context

1. Locate the repository root with `git rev-parse --show-toplevel`.
2. Read `llms.txt` at that root first.
3. Follow only the manual links needed for the task. Do not load the complete manual by default.
4. Inspect owning implementation and tests when the manual is silent, ambiguous, or may be stale.
5. Treat parser acceptance as weaker than implemented execution support. Unsupported statement shapes must fail explicitly.

## Write UQA Engine SQL

- Use exact SQL function names, analyzer names, and JSON component tags from the manual.
- Keep identifiers as SQL syntax. Positional parameters such as `$1` bind values only; they cannot bind table names, column names, ordering clauses, or SQL fragments.
- Pass the first argument of an operator join as a relation identifier such as `passages` or `app.passages`, never as a string literal.
- Treat `text_similarity_join`, `vector_similarity_join`, `graph_join`, `hybrid_join`, and `cross_paradigm_join` as relation-producing sources. Compose their results through aliases, joins, subqueries, or CTEs; do not pass one operator-join call as a scalar operand to another.
- Project `_score` only from a query that establishes ranked support. Order ranked output by `_score DESC` plus a stable identity tie key.
- Match vector and tensor dimensions exactly and use finite values.
- Require a physical GIN index before full-text search or analyzer field assignment.

## Preserve analyzer vocabulary

- Use `html_strip` and `ascii_folding` as the exact serialized JSON tags. Do not invent aliases such as `h_t_m_l_strip` or `a_s_c_i_i_folding`.
- Use the built-ins `standard`, `whitespace`, `standard_cjk`, and `keyword` by those exact names.
- Describe `standard_cjk` as a character n-gram pipeline for CJK-style and substring-oriented matching, not as a Chinese, Japanese, or Korean morphological segmenter.
- Prefer one durable `both` assignment unless an asymmetric index/search analyzer lifecycle is explicitly tested.
- Do not mix GIN-index-owned analyzer configuration with field-assignment ownership for the same field.

## Update the manual

Keep `docs/manual` as the canonical detailed source. Use `llms.txt` only as a compact discovery map, and update its links when the manual structure changes.

Document a function or feature with this contract order:

1. Syntax
2. Arguments, distinguishing identifiers from values
3. Result shape and types
4. State or transaction effects
5. Validation and error conditions
6. One minimal executable example

Use canonical names in prose and code. Link to an existing detailed chapter instead of duplicating its contract in the skill or another manual page.

Keep each prose paragraph on one physical line. Do not insert line breaks inside paragraphs.

## Maintain binding example parity

Treat [`examples/README.md`](../../../examples/README.md) as the public scenario matrix. The supported targets are Rust, Python, Node.js, and Browser WASM, and each target must provide unified search, vector KNN, graph and Cypher, storage and transactions, and extensibility examples.

- Keep Rust entry points under `examples/rust`, Python entry points under `examples/python`, Node.js entry points under `examples/node`, and Browser WASM entry points under `examples/browser`.
- Equivalent examples must use the same fixture intent, SQL feature, assertions, and lifecycle outcome. Preserve language-idiomatic filenames and APIs.
- Node.js and Browser WASM may share binding-neutral scenario modules under `examples/javascript`, but engine construction, persistence, callback registration, and cleanup belong in their platform entry points.
- When a scenario or binding changes, update the matrix, its platform README files, the relevant manual link, and CI execution in the same change.
- Do not document an example as supported after compilation alone. Execute it through the actual binding artifact.

Classify fenced manual SQL as follows:

- `sql`: compile on every CI run.
- `sql execute`: compile and execute in source order against one in-memory engine per Markdown file.
- `sql compile-fail`: intentionally invalid syntax whose compilation must fail.

Do not use an unverified SQL fence or an unknown fence qualifier.

## Verify claims

For manual SQL changes, run:

```sh
cargo test -p uqa-engine --test integration engine_queries::manual_sql_examples::manual_sql_examples_compile_or_execute
```

Run the subsystem's focused tests as well when behavior, not only documentation, changes. In the final report, distinguish examples that were compiled from those that were executed; never claim runtime verification from compilation alone.

For a complete binding-parity change, execute the five Rust packages, every Python and Node.js entry point, and the five Browser WASM modules after building their respective artifacts. The CI workflows under `.github/workflows/ci.yml`, `python-wheels.yml`, and `javascript-bindings.yml` are the authoritative command definitions.

## Prepare a release

- Keep the workspace package version, every workspace-internal dependency requirement, Python project version, Node.js package version, Browser WASM package version, CLI version assertion, README, manual target, `llms.txt`, citation metadata, and generated Node.js loader on one release version.
- Move user-visible changes from `Unreleased` into a dated `HISTORY.md` section before tagging. The release workflow extracts that exact section as GitHub release notes.
- Regenerate `Cargo.lock` and the Node.js loader after changing package versions, then verify that no public version surface still names the previous release except historical changelog sections.
- Sync crate-local AGPL legal files with `python3 scripts/sync-crate-legal-files.py` and confirm `python3 scripts/check-release-licenses.py` plus `bash scripts/package-publishable-crates.sh`.
- Keep the PostgreSQL 18 parser on the imported `uqa-pg-query` crate. Update it only through `python3 scripts/sync-uqa-pg-query.py` after reviewing the two upstream repositories, then run `--check`.
- Run the manual SQL test, binding tests, all binding-parity examples, package archive license checks, formatting, Clippy with warnings denied, and repository policy scripts against the release tree.
- Create an annotated `v<version>` tag only after the release commit is on `main`, push the commit and tag, monitor `.github/workflows/release.yml` to completion, and verify the GitHub release inventory and archive versions.
- Package all public Rust crates with `bash scripts/package-publishable-crates.sh`, then run the registry-independent crates.io dry-run with `bash scripts/publish-crates.sh`. A live upload requires `--live`, dry-runs each crate immediately before upload, starts with `uqa-pg-query`, and ends with the `uqa` facade after `uqa-engine`. Do not publish `uqa-python`, `uqa-node`, or `uqa-wasm` to crates.io.
