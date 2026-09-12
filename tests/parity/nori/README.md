# Lucene Nori reference examples

These fixtures support the proposed [Nori analyzer design](../../../docs/design/nori-analyzer.md). They record actual Lucene 10.5.1 results for 31 small cases, including an invalid user dictionary and the separate normalization API. They do not compare against a UQA Nori implementation and do not establish complete compatibility.

`manifest.json` pins the Lucene source commit, three Maven Central jar hashes, the Docker image digest, the JVM runtime, and the dictionary resources. `NoriReference.java` uses Lucene's real `KoreanAnalyzer`, tokenizer, filters, and token attributes. `expected.jsonl` records the runtime followed by the case results; offsets are UTF-16 code units. The dictionary archive's `COPYING` was inspected and identifies Apache-2.0. The fixture runner only downloads the three jars; it does not download or rebuild the dictionary archive.

## Reproduce

Use Python 3.11 or newer and Docker. The JVM runs exclusively inside Docker; a host JDK is unnecessary. From the repository root:

```sh
python3 tests/parity/nori/run_reference.py
```

The runner fetches checksum-pinned jars into a temporary cache and compares fresh Docker output with the checked-in file. The container has no network and mounts the source and jars read-only. Subsequent checks can require cached inputs:

```sh
python3 tests/parity/nori/run_reference.py --offline
```

The 31 cases were reproduced on both `linux/arm64` and `linux/amd64` Docker platforms with Temurin 21.0.10+7; select the latter with `--platform linux/amd64`. Regenerate an intentionally changed reference with `--write`, inspect the full output diff, and update the manifest and design evidence together. Do not regenerate expected results merely to make a future Rust mismatch disappear.

## Interpretation

The cases cover compound modes, inflections, POS stop gaps and trailing positions, Hanja readings, unknown unigrams, punctuation and spaces, user dictionary precedence and segmentation rules, Unicode lowercase and supplementary characters, decomposed Hangul, Korean numbers, empty input, and `KoreanAnalyzer.normalize`. For example, shortened user segmentations retain Lucene's surprising offsets, and number composition after punctuation removal can produce an unintended value. These observations constrain the port and the documented configuration examples.

The [complete model exporter](MODEL.md) adds exhaustive dictionary, connection-cost, character-definition, and Java Unicode extraction with a reproducible manifest. It uses the same pinned inputs and Docker-only JVM. The larger differential corpus, Rust morphology runtime and bundle, storage migration, binding execution, and performance measurements remain implementation work.
