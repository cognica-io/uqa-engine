# Lucene Nori reference examples

These fixtures support the [Nori analyzer design](../../../docs/design/nori-analyzer.md). The original 31 cases record actual Lucene 10.5.1 tokenizer, filter, analyzer, and normalization results. Separate expanded corpora now compare native Rust user-dictionary compilation and standalone tokenization against Lucene. Full analyzer, storage, binding, and performance acceptance remains in the [implementation plan](../../../docs/plans/0006-nori-analyzer.md).

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

The [complete model exporter](MODEL.md) adds exhaustive dictionary, connection-cost, character-definition, and Java Unicode extraction with a reproducible manifest. It uses the same pinned inputs and Docker-only JVM. The native bundle reconstructs every exported model value. Storage migration, full analyzer and binding execution, and performance measurements remain implementation work.

## Native user dictionaries and tokenization

`NoriUserReference.java` uses the public Lucene user dictionary and FST to record 38 compilation and lookup cases. They include stable duplicate ordering, invalid first definitions, accepted invalid later duplicates, Java whitespace/comment boundaries, trailing-line right contexts, nonmatching or shortened segmentation labels, UTF-16 ordering, embedded NUL, and surrogate-splitting labels. `user_cases.json`, `user_expected.jsonl`, and `user_manifest.json` retain the inputs, complete attributes, and SHA-256 provenance.

`NoriTokenizerReference.java` uses the public Korean tokenizer to record 238 cases independently of downstream filters. The corpus covers all three compound modes for user rules, punctuation and unknown-unigram combinations, Java script/category boundaries, inflections, ambiguous Hangul around the 1,024-unit backtrace boundary, long unknown groups, user words that extend beyond the current forced-backtrace position, and a fixed mixed-script corpus. `tokenizer_cases.json` is the complete checked-in input, with no runtime random generation.

Every token's term and morpheme units, offsets, increments, lengths, POS attributes, and nullable reading/decomposition contribute to a SHA-256 digest of canonical JSON with recursively sorted keys, compact separators, and UTF-16 strings represented by integer arrays. The stream-end offset and increment are included. Short results also retain the full analysis object for review. Java's public token attributes do not expose dictionary origin, so origin is not part of the Lucene comparison. Errors remain explicit case results.

Both drivers run exclusively inside the pinned Docker image. The shared Python runner validates source, input, expected-output, and reference-manifest hashes before execution, transports source text in Base64, verifies the complete ordered case inventory and runtime, and preserves Unicode line terminators. A verification run does not rewrite expected output. The 38 and 238 cases have been reproduced identically on Docker arm64 and amd64.

```sh
python3 tests/parity/nori/run_user_reference.py --offline
python3 tests/parity/nori/run_tokenizer_reference.py --offline
python3 tests/parity/nori/run_user_reference.py --offline --platform linux/amd64
python3 tests/parity/nori/run_tokenizer_reference.py --offline --platform linux/amd64
cargo test -p uqa-analysis --features nori --locked nori_users
cargo test -p uqa-analysis --features nori --locked nori_tokenizer
```

Use `--cache-dir PATH` for a nondefault verified jar cache. Each driver accepts `--write` only for an intentional reviewed reference change. Native tests check fixture identity and compare every recorded attribute, including the complete digests for long streams. Additional native lattice tests isolate forced-backtrace tie order, rebasing, and EOS connection-cost selection. Limits and cancellation tests verify failure without partial successful output and reuse of the same immutable tokenizer.

The accepted rule `🙂a 가 나` demonstrates why raw UTF-16 must survive the native tokenizer: component lengths can split a surrogate pair even though the rule and input are valid UTF-8. Back-anchored component offsets can also fall inside a pair. These cases are part of the differential contract; the common-token bridge, persisted term identity, and safe original-source highlighting must preserve them before full Nori integration is complete.

## Filters, complete analysis, and normalization

`NoriAnalysisReference.java` adds 423 cases for individual POS/reading/lowercase filters, explicitly ordered chains, the actual `KoreanAnalyzer`, and its separate normalization API. It covers custom and empty stop sets, all compound modes, trailing gaps, Hanja and supplementary case conversion, user-rule corner cases, long streams, and source whitespace. `analysis_cases.json`, `analysis_expected.jsonl`, and `analysis_manifest.json` use the same canonical complete-output hashing and reviewed provenance as the tokenizer corpus.

One case constructs every Unicode scalar in order, followed by every surrogate code unit separated by `!`, and applies Lucene `CharacterUtils.toLowerCase`. Its 2,164,736 output units have UTF-16BE SHA-256 `6e429352aa8ccd1ffb2254e036ba64cfe810aca8141bdf51c0a0f057a8994a78`. This checks the native interpretation of the entire pinned lowercase table, scalar-pair traversal, and preservation of unpaired units without checking a multi-megabyte generated string into the repository. The generator is part of the hashed Java source; the native comparison independently constructs the same specified sequence.

```sh
python3 tests/parity/nori/run_analysis_reference.py --offline
python3 tests/parity/nori/run_analysis_reference.py --offline --platform linux/amd64
cargo test -p uqa-analysis --features nori --locked nori_analysis
```

Both Docker platforms produced the same 423 results and the native implementation matches them. The eight original full-analyzer examples also pass directly against their original reference file. Native regressions additionally cover present-empty readings, left-versus-right POS selection, stacked edges, trailing holes, checked position overflow, cancellation, bounds, and strict configuration properties. Korean number composition and generic pipeline/binding integration remain open acceptance items.
