# Persistent Nori binding contract

`bindings.json` supplies the same ordered SQL statements, parameters, expected rows, and errors to the existing Rust Engine integration harness, Python tests, native Node.js tests, and browser WASM package tests. Node-hosted JavaScript entrypoints share [`../bindings.mjs`](../bindings.mjs), and the real browser uses the host-neutral [`../bindings.core.mjs`](../bindings.core.mjs) assertions; construction, storage paths, and cleanup remain in each binding's test. Rust runs the persistent contract against both SQLite and redb. Python, Node.js, and WASM use SQLite.

The 47 feature-enabled steps register a Mixed-mode user dictionary, map `首都` to `세종시`, and convert the Hanja reading `學校` to `학교`. The complete diagnostic fixes the analyzer fingerprint, four token terms, morphology, graph increments and lengths, filtered coordinates, original UTF-8/UTF-16 spans, and stream end state. The compound has an original edge of length two and two constituent edges; the character replacement maps all three source spans back to `首都`. This is a shared SQL/binding contract; the independent pinned Lucene differential fixtures remain the morphology oracle.

The fixture checks positive and negative quoted phrases through `fts_match`, including reversed order and a removed-particle gap. `text_match` checks ordinary reading-form term support. Highlighting must return the original source `<b>首都</b> <b>學校</b>`. Unavailable bundles and invalid user segmentation must fail without replacing the existing descriptor or publishing a new name; built-in analyzer replacement must fail as well.

Transaction and savepoint rollbacks must restore the occurrence graph. Replacing the catalog name with a keyword analyzer must leave both field revisions unchanged. Every Engine handle is closed before reopening; a subsequent insert must still use the retained Nori index revision. A search-only change is rolled back independently of the index, and a second close/reopen checks the new postings and restored descriptor before cleanup. No dictionary file, JVM, network fetch, or process-local registration is supplied to the opened database.

Rust additionally runs the complete enabled and disabled contracts against closed-file backups of both SQLite and redb. At each reopen boundary it closes every Engine handle, copies the database into a fresh directory, deletes the original directory, and opens only the restored copy. The unchanged fixture then verifies retained resources, complete token graphs, queries, new writes, and the second restoration. Both native feature configurations pass this backup/restore test and the original reopen test.

Nine additional steps exercise actual Rust, Python, Node.js, and WASM builds without Nori: the built-in is absent, Korean tokenizer/filter configuration and diagnostics fail explicitly, failed registration leaves no catalog name, and a generic keyword analyzer still highlights and reopens. The default binding test mode always requires the complete Nori scenario. `UQA_TEST_NORI=disabled` explicitly selects the missing-feature contract; builds without either dictionary also set `UQA_TEST_KUROMOJI=disabled`, and any other value fails rather than skipping assertions. Node and WASM tests can load a separately built artifact through `UQA_TEST_PACKAGE`; CommonJS and ESM must resolve the same selected package. Disabled runtime dependency trees exclude `uqa-nori-data`.

Actual Chrome verification is recorded in [browser persistence and memory evidence](../../../benchmarks/nori/BROWSER.md). Three enabled and three disabled sessions pass every fixture step after fresh page/WASM construction, including real IndexedDB synchronization and reload. The enabled browser also matches 18 full native Python SQL diagnostics over the six fixed corpora and three modes. The existing Node-hosted WASM tests continue to check the virtual-filesystem and callback contract independently.

Build the actual binding artifacts before executing the tests. The existing CI jobs already perform these builds, and shared fixture changes select Rust, JavaScript/WASM, and Python pre-merge suites.

```sh
cargo test -p uqa-engine --features nori --test integration --locked nori_binding_contract
cargo test -p uqa-engine --no-default-features --test integration --locked nori_binding_contract
maturin develop --locked --features nori
python -m pytest -q tests/python/test_uqa_python.py -k nori
# In crates/uqa-node:
npx napi build --platform --no-js --profile release-stripped --features nori
# From the repository root:
node --test tests/node/test_uqa_node.mjs
bash scripts/build-wasm.sh
node --test tests/wasm/test_uqa_wasm.mjs
```

Custom builds use `maturin build --locked --no-default-features`, `npx napi build --no-default-features`, or `bash scripts/build-wasm.sh --no-default-features`. The WASM script also accepts `--output-dir DIR` to retain a separate generated pair and respects Cargo's configured target directory. Select an isolated Python environment or the matching Node/WASM package when testing; the normal Nori-enabled artifact must fail the disabled assertions. The [Python](../../../.github/workflows/python-wheels.yml) and [JavaScript](../../../.github/workflows/javascript-bindings.yml) workflows contain the complete build, staging, dependency-tree, and execution commands for both configurations.
