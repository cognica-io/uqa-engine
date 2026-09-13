# Neutral Nori model export

`export_model.py` exports the complete model from the pinned official Lucene jars through `NoriModel.java`, using the Docker runtime and checked resources in `manifest.json`. This is the offline input for the Rust dictionary packer. It is not the production UQA bundle, and no Cargo build or query invokes this tool.

The recorded export contains 774,582 distinct surfaces, 816,283 ordered system word entries, 14 unknown classes and entries, a 3,822-forward by 2,693-backward connection matrix with 10,292,646 costs, 65,536 character-definition entries, and 1,114,112 Java Unicode entries. The five neutral files total 82,581,342 bytes. This size measures the uncompressed neutral export; it does not establish a runtime bundle size or memory requirement.

## Reproduce and verify

Use Python 3.11 or newer and Docker. The host does not need a JVM:

```sh
python3 tests/parity/nori/export_model.py --output target/nori-reference-model
python3 tests/parity/nori/export_model.py --output target/nori-reference-model --verify-only --offline
python3 -m unittest discover -s scripts/tests -p test_nori_model_export.py
```

`--cache-dir` selects the same checksum-validated jar cache as `run_reference.py`. `--offline` requires cached jars and a Docker image. `--platform` accepts `linux/arm64` and `linux/amd64`; the checked-in manifest was generated on arm64 and independently reproduced on both Docker platforms. Every output hash and model value agreed. These executions establish Docker-platform reproducibility, not native-host performance measurements.

Export requires a new output directory. Java writes into a temporary staging directory, reopens every file, and compares every field with the live Lucene model. Python then compares file hashes, counts, runtime, vocabularies, resource provenance, and exporter identity with the reviewed `model_manifest.json` before publishing the directory. Verification of existing output first checks its inventory and hashes, then mounts it read-only for the same exhaustive JVM comparison. Changed, truncated, or extended files are errors.

An intentional exporter or reference update uses `--write-manifest` with a fresh output directory. Review the entire manifest diff, rerun export without that option, and keep the design and implementation plan evidence current. This option cannot be combined with `--verify-only`. Expected hashes must not be rewritten to conceal a Rust mismatch.

## Complete coverage checks

The exporter enumerates the FST in UTF-16 label order and independently resolves every surface through Lucene's runtime `TokenInfoFST`. It checks each ordered word-ID list against the serialized target map and requires coverage of every source ID and word entry. Public `KoMorphData` calls supply both context IDs, signed cost, POS type and tags, optional reading, and every ordered morpheme. Unknown entries use the same checks for all character classes.

Lucene's codec reader supplies matrix dimensions and every delta-encoded cell; all decoded cells are checked against `ConnectionCosts.get(forward, backward)`. Character-definition categories and invoke/group flags are compared with the live model for every UTF-16 unit. Flags for classes without a representative character are retained directly from the checked resource. The Unicode profile records every code point accepted by Java's classification APIs, including surrogate code points needed when the tokenizer inspects individual UTF-16 units. No private-field reflection is used.

The export is compared field by field after writing, and fresh runs must reproduce every recorded SHA-256. This proves model extraction and serialization against the pinned JVM. The [implementation plan](../../../docs/plans/0006-nori-analyzer.md) records native loading/analysis parity, resource packaging, and remaining performance and delivery gates.

## CSV source regeneration

`regenerate_dictionary.py` rebuilds the dictionary through the pinned jar's public `DictionaryBuilder`, using the exact MeCab-ko-dic 2.1.1-20180720 archive, UTF-8, and entry normalization disabled. Python validates the source archive and jar hashes before selecting the 40 top-level CSV files plus `char.def`, `unk.def`, and `matrix.def`. It copies only those builder inputs; it does not execute or unpack the archive's build scripts. All Java execution uses the pinned Docker image and a 1 GiB heap, with input mounted read-only and networking disabled inside the container.

```sh
python3 tests/parity/nori/regenerate_dictionary.py --output target/nori-csv-regeneration
python3 tests/parity/nori/regenerate_dictionary.py --output target/nori-csv-repeat --offline
python3 -m unittest discover -s scripts/tests -p 'test_nori*.py'
```

`--cache-dir` selects the shared jar/source cache. `--offline` requires the cached source archive, jars, and Docker image. `--platform` selects `linux/arm64` or `linux/amd64`. Each run requires a new output directory; after validation it contains `resources/` with the nine generated files and `regeneration_manifest.json` with their provenance. The checked-in [`csv_manifest.json`](csv_manifest.json) records the builder source, reference manifest, archive identity, builder flags, JVM version, and every input/output size and SHA-256. Platform selection does not change this manifest.

The 49,775,061-byte archive supplies 202,683,519 bytes across 43 builder inputs. Fresh arm64 runs and an independent amd64 Docker run produced all nine dictionary resources byte for byte equal to the pinned release jar, totaling 25,039,384 bytes. Equality covers the compiled FST, stable word ordering, dictionary buffers/maps, character classes, and connection matrix; it is stronger than comparing token examples alone. These are reproducibility and artifact-size measurements, not analysis throughput or runtime-memory measurements.

Any input, runtime, file-inventory, size, or checksum difference fails the run before publishing an output directory. Existing output is preserved. `--write-manifest` records an intentional reviewed provenance change only after every regenerated resource matches the reference jar; it cannot accept different resource bytes. The packaged UQA bundle is never replaced by this command. Pre-merge CI's Nori regeneration job runs this comparison, exhaustive neutral model export, and all six reference drivers, then retains the model and regeneration manifests as artifacts.

## Binary format version 1

All integers use big-endian byte order. `i32` is a signed 32-bit integer, `u16` an unsigned 16-bit integer, and `u8` an unsigned byte. Counts and IDs stored as `i32` must be nonnegative except the explicit `-1` null markers. Each file starts with its eight ASCII magic bytes. The final `1` identifies this neutral format version. No padding or trailing bytes are permitted.

`text` is an `i32` UTF-16 unit count followed by that many `u16` units; `-1` denotes absent text, while zero denotes present empty text. It uses neither Java modified UTF-8 nor a platform-dependent string encoding. Runtime packers must validate the text before projecting it to UTF-8.

A `word` record contains seven `i32` fields in this order: Lucene word ID, left context ID, right context ID, signed word cost, POS type ordinal, left POS ordinal, and right POS ordinal. Next comes nullable reading `text`, then an `i32` morpheme count (`-1` for absent). Each present morpheme is an `i32` POS ordinal followed by its surface `text`. The manifest records complete POS vocabularies in ordinal order and retains each tag's separate Lucene code; the code and ordinal are not interchangeable.

| File | Magic | Fields after magic |
| --- | --- | --- |
| `lexicon.bin` | `UQANLEX1` | `i32` surface count, `i32` total word-entry count; for each surface: `i32` source ID, surface `text`, `i32` entry count, ordered `word` records |
| `unknown.bin` | `UQANUNK1` | `i32` class count, `i32` total word-entry count; for each class: `i32` class ID, class-name `text`, `i32` entry count, ordered `word` records |
| `connection_costs.bin` | `UQANCCS1` | `i32` forward count, `i32` backward count; signed 16-bit costs encoded in `u16` two's-complement form, with backward rows outside forward columns |
| `characters.bin` | `UQANCHR1` | `i32` class count, one flags `u8` per class, `i32` character count, then two `u8` values per character: category and morphology flags |
| `unicode.bin` | `UQANUNI1` | `i32` code-point count; for each code point: type `u8`, script ordinal `u16`, flags `u8`, simple lowercase code point `i32` |

Connection lookup uses index `backward * forward_count + forward`, where the predecessor's right context selects the forward column and the next word's left context selects the backward row. Every exported word's context IDs are checked against those dimensions.

Character class flags use bit 0 for invoke and bit 1 for group. Per-character morphology flags use bit 0 for `isHanja`, bit 1 for `isHangul`, and bit 2 for `hasCoda`, as returned by the pinned `CharacterDefinition`. Unicode type values come from `Character.getType`; script ordinals index the complete `unicode_scripts` manifest array. Unicode flags use bit 0 for `Character.isDigit`, bit 1 for `Character.isWhitespace`, and bit 2 for `Character.isSpaceChar`. Lowercase comes from `Character.toLowerCase(int)`, preserving simple per-code-point mapping.
