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

The hash-pinned reports in `browser-evidence/manifest.json` contain three enabled and three disabled runs on Chrome 152.0.7977.83, macOS aarch64, and Apple M1 Ultra. They were collected from the stable dirty tree based on `b3c804c0`; their source and executable identities record the measured implementation. Every run passed, providing 165 memory observations, 15 distinct page instances, nine IndexedDB reloads, and 54 full corpus comparisons. The build and browser measurements ran sequentially. The earlier driver trial is not calibration evidence.

| Observation | Minimum | Median | Maximum |
| --- | ---: | ---: | ---: |
| Enabled, first Engine open before Nori use | 37,878,243 bytes | 38,266,763 bytes | 38,292,249 bytes |
| Enabled, first complete Nori diagnostic | 141,093,365 bytes | 141,179,276 bytes | 141,194,226 bytes |
| Enabled, first open restored from IndexedDB | 139,893,323 bytes | 139,895,799 bytes | 139,899,385 bytes |
| Enabled, final cleanup and persistence | 142,310,052 bytes | 142,337,678 bytes | 142,351,353 bytes |
| Disabled, first Engine open | 27,974,411 bytes | 28,024,379 bytes | 28,511,740 bytes |
| Disabled, final cleanup and persistence | 29,526,844 bytes | 29,526,852 bytes | 29,526,920 bytes |

The largest observed value across all enabled points is 143,655,635 bytes; the disabled maximum is 29,526,920 bytes. The generated WASM files are 56,648,788 and 46,267,072 bytes respectively, before HTTP compression. Dictionary initialization and durable descriptor restoration explain why an enabled reopen is larger than the first empty Engine open. The complete reports retain per-corpus values and browser attribution rather than collapsing them into a claimed heap peak.
