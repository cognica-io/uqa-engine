# Nori in an actual browser

`scripts/verify-nori-browser.py` opens fresh Chrome sessions through pinned Playwright CLI 0.1.19. The browser loads the generated Emscripten package over a loopback HTTP server with cross-origin isolation. Every persistent checkpoint closes the Engine, awaits `UQA.persist()`, checks real IndexedDB storage, and reloads the page through its visible button. The next page constructs a new WASM module and opens the saved database without re-registering its analyzers. The enabled fixture runs all 47 shared binding steps across three page loads; the disabled fixture runs all nine steps across two page loads. Neither path uses a Node filesystem shim.

The enabled page also analyzes all six fixed corpora in each of the None, Discard, and Mixed modes. It compares SHA-256 identities of the complete, recursively key-sorted SQL diagnostics against `browser-contract.json`, including every token, morphology field, graph coordinate, original-source offset, fingerprint, and stream-end attribute. `tests/wasm/export_nori_diagnostics.py` reproduces that contract through an installed native Python binding, and the Python workflow checks it against the frozen file. These native/binding comparisons complement the independent Lucene differential fixtures; they do not substitute a new morphology oracle.

```sh
bash scripts/build-wasm.sh
python3 scripts/verify-nori-browser.py --output output/playwright/nori-enabled/report.json
bash scripts/build-wasm.sh --no-default-features --output-dir target/nori-binding-disabled/wasm
cp crates/uqa-wasm/js/index.mjs crates/uqa-wasm/js/package.json target/nori-binding-disabled/wasm/
python3 scripts/verify-nori-browser.py --nori disabled --bundle target/nori-binding-disabled/wasm/index.mjs --output output/playwright/nori-disabled/report.json
# In an environment with the actual Nori-enabled Python wheel installed:
python tests/wasm/export_nori_diagnostics.py > /tmp/nori-browser-native.json
diff -u benchmarks/nori/browser-contract.json /tmp/nori-browser-native.json
```

Chrome and `npx` must be available. The driver defaults to three fresh browser sessions, validates every fixture step, page identity, IndexedDB checkpoint, complete diagnostic hash, and memory sample, and rejects browser console errors. It records the browser version, host, toolchain, requested compiler flags, runtime dependency closure, source hashes, and actual JS/WASM artifact hashes. Feature-disabled dependency resolution must omit `uqa-nori-data`. JavaScript CI executes both browser configurations and retains their reports alongside the existing native/WASM allocation gates. For manual inspection, run `python3 scripts/serve-wasm-tests.py`, open the printed address, and use the reload buttons; the restart button creates an independent test database.

## Memory observations

Memory comes from `performance.measureUserAgentSpecificMemory()`. This is a browser estimate of page-attributed memory, including JavaScript, DOM, and shared allocations, sampled after garbage collection. The driver uses Chrome's documented `ForceEagerMeasureMemory` flag to request collection without the normal sampling delay. Implementations can exclude memory regions, and results are not comparable across browser versions. See the [Chrome memory measurement explanation](https://web.dev/articles/monitor-total-page-memory-usage). The values are point observations, not process RSS, continuous allocation peaks, or the Rust query allowance. No browser memory ceiling is inferred from an unobserved interval or transferred to another browser/platform.

The page samples before WASM initialization, after Engine open, after the first complete Nori diagnostic, and at persistence/reopen boundaries. Each of the 18 corpus conditions adds a sample with its complete returned diagnostic still live and another after releasing it. The final sample follows cleanup and persistence. There are 48 points per enabled run and seven per disabled run. The exact diagnostic hash is computed after the retained sample so the result remains live while the browser measures it.

Browser reports remain CI artifacts. Verifier unit tests use a small synthetic fixture with sentinel memory and IndexedDB byte counts; those values are not browser observations. The complete SQL diagnostic contract remains in `browser-contract.json`.
