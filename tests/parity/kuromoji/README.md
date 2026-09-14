# Pinned Lucene Kuromoji dictionary, model and user rules

These Docker tools reproduce the complete Japanese dictionary and export its public morphology model for the [native implementation plan](../../../docs/plans/0007-kuromoji-analyzer.md). They establish reference data and provenance. Native Japanese loading, tokenization and analyzer parity remain implementation work.

[`manifest.json`](manifest.json) pins Lucene 10.5.1, source commit `64ce863a2bea79c69c19c4d56268c26710ff0ff9`, the core/common/Kuromoji jars, Temurin 21.0.10+7, nine dictionary resources and three analysis resources. Both Java compilation and execution run inside the pinned Docker image, with no container network and read-only source/jar mounts. Production builds and queries never run these tools or download dictionaries.

## Reproduce

Use Python 3.11 or newer and Docker from the repository root:

```sh
python3 tests/parity/kuromoji/regenerate_dictionary.py --output target/kuromoji-csv-regeneration
python3 tests/parity/kuromoji/export_model.py --output target/kuromoji-reference-model --offline
python3 tests/parity/kuromoji/export_model.py --output target/kuromoji-reference-model --verify-only --offline
python3 -m unittest scripts.tests.test_kuromoji_reference scripts.tests.test_nori_model_export scripts.tests.test_nori_csv_regeneration
```

The default cache is `uqa-lucene-reference-jars` below Python's temporary directory. `--cache-dir PATH` selects another verified cache, `--offline` requires cached inputs and image, and `--platform linux/amd64` selects the CI architecture instead of the default `linux/arm64`. The checked manifests were generated and verified on arm64; required pre-merge CI independently regenerates and verifies on amd64. CI retains only compact manifests.

Generation and export require new output directories. Every input, file inventory, runtime and hash must match before an output directory is published. Model verification checks hashes first and then mounts the existing export read-only for comparison of every field with Lucene. Corruption, trailing bytes and unexpected files fail verification. Only an intentional reviewed reference change uses `--write-manifest`; it cannot be combined with `--verify-only`, and dictionary generation still must match every resource in the pinned jar.

## Exact dictionary provenance

The [pinned upstream recipe](https://github.com/apache/lucene/blob/64ce863a2bea79c69c19c4d56268c26710ff0ff9/gradle/generation/kuromoji.gradle) uses `mecab-ipadic-2.7.0-20070801`, applies `Noun.proper.csv.patch`, and calls `DictionaryBuilder` with IPADIC format, EUC-JP and entry normalization disabled. The source archive is 12,208,105 bytes with SHA-256 `b62f527d881c504576baed9c6ef6561554658b175ce6ae0096a60307e49e3523`. The original EUC-JP patch is 1,198 bytes with SHA-256 `771970de4df33f53bade6cfaace783bffa46ee41af8958dd7bac2758afd6216b`, matching upstream Git blob `1e0c8d30fca9c4915f66dc54d4cf2017724e79b3`. Its bytes must not be transcoded before application.

[`csv_manifest.json`](csv_manifest.json) records all 29 builder inputs before and after the patch, the exact recipe and builder identities, runtime, and all generated resource hashes. Extraction copies only direct CSV and definition files, rejects traversal, links and duplicate inputs, and never executes archive scripts. The patch must target only `Noun.proper.csv`. Docker regeneration reproduced all nine `.dat` files byte for byte, including the FST, target maps, morphology buffers, character classes and connection costs. The stopword, stop-tag and completion mapping resources are independently validated against the jar inventory.

IPADIC attribution is distinct from the Korean dictionary. The future packaged Japanese data must carry its original `COPYING`, relevant IPADIC/NAIST/ICOT notices, Lucene LICENSE/NOTICE and JDK table notices. The current tooling does not create or distribute a production Japanese data crate.

## Exhaustive model coverage

[`model_manifest.json`](model_manifest.json) records 325,872 UTF-16 surfaces, 392,127 ordered system word entries, 12 unknown classes with 41 entries, 1,316 by 1,316 connection costs, 65,536 character definitions, all 1,114,112 Java code-point values, 109 default stopwords, 27 default stop tags and 329 completion mappings. The six temporary binary files total 52,187,194 bytes. This is the neutral export size, not a production bundle size or runtime memory measurement. The three checked-in JSON manifests total less than 27 KiB; full model binaries stay outside Git.

`KuromojiModel.java` enumerates the FST and independently resolves every surface through the runtime FST, checks every ordered word list against the serialized target map, and requires complete source/word coverage. Public `JaMorphData` supplies context IDs, signed costs and all six nullable Japanese morphology attributes. Every decoded connection-matrix cell and character class/invoke/group flag is checked against the runtime model. Character classification and lowercase cover surrogate code points as well as Unicode scalars. Public analyzer getters supply exact stop sets; every completion-map key and ordered alternative is compared with the real `KatakanaRomanizer`. No private-field reflection is used.

Java reopens the exported files and compares them field by field before returning. A separate Docker invocation repeats comparison against a read-only mount. Python uses the same source extraction, hash validation, staging and model-verification mechanisms as Nori. The existing `NoriModel.java`, Nori reference manifests, model hashes and bundle identities remain unchanged.

## Neutral binary format version 1

Each file begins with the eight ASCII magic bytes below. All integers are big-endian. `text` is an `i32` UTF-16 unit count followed by `u16` units; `-1` means absent and zero means present empty. A word record has four `i32` values (Lucene word ID, left context, right context, signed cost), then six nullable texts in order: POS, base form, reading, pronunciation, inflection type, inflection form. Surface-dependent attributes are materialized through the public API for that exact dictionary surface. No padding or trailing bytes are allowed.

| File | Magic | Fields after magic |
| --- | --- | --- |
| `lexicon.bin` | `UQAJLEX1` | `i32` surface count and total word count; each surface has source ID, text, word count and ordered word records |
| `unknown.bin` | `UQAJUNK1` | `i32` class count and total word count; each class has ID, name text, word count and ordered word records |
| `connection_costs.bin` | `UQAJCCS1` | `i32` forward/backward counts, then signed 16-bit costs; backward rows contain forward columns |
| `characters.bin` | `UQAJCHR1` | `i32` class count, one flags byte per class, `i32` character count, then category and `isKanji` bytes per UTF-16 unit |
| `unicode.bin` | `UQAJUNI1` | `i32` code-point count; each value has type `u8`, script ordinal `u16`, flags `u8`, simple lowercase `i32` |
| `analysis.bin` | `UQAJANA1` | Count and sorted texts for stopwords, count and sorted texts for stop tags, mapping count; each sorted mapping key has text, alternative count and ordered texts |

Class flags use bit 0 for invoke and bit 1 for group. Unicode flags use bit 0 for digit, bit 1 for whitespace and bit 2 for space character; script ordinals index the manifest's full vocabulary. Connection lookup is `backward * forward_count + forward`; a predecessor's right context selects the column, and the next word's left context selects the row. Every exported context is checked against those dimensions. Sorting uses Java UTF-16 ordering; alternative order is never sorted.

## Japanese user dictionaries

`KuromojiUserReference.java` uses the public Japanese user dictionary, FST and morphology APIs to record 62 fixed cases. The corpus covers Java comment/line/whitespace rules, CSV quoting and its unchanged final field, duplicate rejection, segmentation/readings, source and word order, overlapping longest matches, supplementary characters, embedded NUL, empty entries and morphology-access errors. UTF-16 lengths and every available or absent morphology field are compared independently from tokenization. The original Nori drivers use the same language-independent fixture transport and retain their existing inputs, Java sources, manifests and expected bytes.

```sh
python3 tests/parity/kuromoji/run_user_reference.py --offline
python3 tests/parity/kuromoji/run_user_reference.py --offline --platform linux/amd64
cargo test -p uqa-analysis --features nori-tools,kuromoji-tools --locked user
```

The native `kuromoji::UserDictionary` compiles exact retained source against a selected dictionary identity under source/entry/surface limits. It shares lexical construction and prefix traversal with Nori, retains Japanese grammar and duplicate semantics separately, and stores POS once per phrase. Lookup preserves zero-length and whitespace-bearing segment lengths accepted by Lucene. The public reading/POS accessors return a checked error when Lucene's NUL-separated feature access fails; absent base/pronunciation/inflection attributes remain absent. This user-rule verification does not establish Japanese tokenizer or analyzer parity.
