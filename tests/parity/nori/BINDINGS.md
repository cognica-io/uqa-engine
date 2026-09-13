# Persistent Nori binding contract

`bindings.json` supplies the same ordered SQL statements, parameters, expected rows, and errors to the existing Rust Engine integration harness, Python tests, native Node.js tests, and browser WASM package tests. The JavaScript entrypoints share `bindings.mjs`; construction, storage paths, and cleanup remain in each binding's test. Rust runs the persistent contract against both SQLite and redb. Python, Node.js, and WASM use SQLite.

The 47 feature-enabled steps register a Mixed-mode user dictionary, map `首都` to `세종시`, and convert the Hanja reading `學校` to `학교`. The complete diagnostic fixes the analyzer fingerprint, four token terms, morphology, graph increments and lengths, filtered coordinates, original UTF-8/UTF-16 spans, and stream end state. The compound has an original edge of length two and two constituent edges; the character replacement maps all three source spans back to `首都`. This is a shared SQL/binding contract; the independent pinned Lucene differential fixtures remain the morphology oracle.

The fixture checks positive and negative quoted phrases through `fts_match`, including reversed order and a removed-particle gap. `text_match` checks ordinary reading-form term support. Highlighting must return the original source `<b>首都</b> <b>學校</b>`. Unavailable bundles and invalid user segmentation must fail without replacing the existing descriptor or publishing a new name; built-in analyzer replacement must fail as well.

Transaction and savepoint rollbacks must restore the occurrence graph. Replacing the catalog name with a keyword analyzer must leave both field revisions unchanged. Every Engine handle is closed before reopening; a subsequent insert must still use the retained Nori index revision. A search-only change is rolled back independently of the index, and a second close/reopen checks the new postings and restored descriptor before cleanup. No dictionary file, JVM, network fetch, or process-local registration is supplied to the opened database.

Nine additional steps exercise Rust builds without Nori: the built-in is absent, Korean tokenizer/filter configuration and diagnostics fail explicitly, failed registration leaves no catalog name, and a generic keyword analyzer still highlights and reopens. Feature-enabled binding tests always require the full Nori scenario and cannot silently skip it when packaging omits the feature. Native binding builds without Nori and browser IndexedDB persistence remain separate verification requirements; WASM's Node-hosted test uses the module's virtual filesystem and does not establish browser-host durability.

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
