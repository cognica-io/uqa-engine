# Kuromoji runtime bundle format

Status: The native dictionary loader, offline packer, complete model verifier and immutable data crate are implemented. Japanese user dictionaries, tokenization, analyzer chains, compiled resource caches and binding delivery remain in the [implementation plan](../plans/0007-kuromoji-analyzer.md).

The independent `kuromoji` feature of `uqa-analysis` exposes `KuromojiDictionary::from_bytes` and enables the `uqa-kuromoji-data` dependency. `kuromoji-tools` adds offline conversion and exhaustive verification. Loading validates the full bundle before returning `Arc<KuromojiDictionary>` and performs no file, network or JVM operations. The public queries expose exact surfaces and prefixes, ordered words, all six Japanese morphology attributes, contexts/costs, unknown classes, Unicode properties and analyzer default resources. Loading a dictionary does not register a Japanese analyzer.

## Input and regeneration

The packer consumes the six binary files and manifest from the [pinned Docker exporter](../../tests/parity/kuromoji/README.md). It validates the source inventory, every length/hash, reference identities, ordered vocabulary and encoded value. It builds the shared UTF-16 lexicon, pools morphology strings, compacts Java Unicode properties into intervals and encodes the sections below. Before publishing, it reloads the bundle through the production loader and reconstructs every byte of all six original streams into their expected SHA-256 hashes. Every enumerated surface is also resolved through the runtime lexicon.

```sh
python3 tests/parity/kuromoji/regenerate_dictionary.py --output target/kuromoji-csv-regeneration
python3 tests/parity/kuromoji/export_model.py --output target/kuromoji-reference-model --offline
cargo run -p uqa-analysis --features kuromoji-tools --locked --example pack_kuromoji -- pack target/kuromoji-reference-model target/kuromoji.uqak
cargo run -p uqa-analysis --features kuromoji-tools --locked --example pack_kuromoji -- verify target/kuromoji.uqak
```

Every Java operation runs in Docker, including source compilation and model extraction. The Rust packer requires no JVM. Its CLI publishes through an atomic, non-clobbering file operation only after complete verification. Keep full intermediate binaries outside Git; committed manifests identify the source, model and final package.

## Framing and identity

The shared `uqa-analysis::morphology` implementation supplies endian-explicit I/O, bounded frame validation, lexicon/surface ranges, string pools, matrices, Unicode intervals, provenance primitives and offline reconstruction. Kuromoji owns its frame schema, semantic identity domain, Japanese word records, character classes and default resources. Nori keeps its original schema, identity and byte-for-byte reproducible bundle.

All bundle integers are little-endian. The 56-byte header stores eight bytes `UQAKURO\0`, version `u32 = 1`, section count `u32 = 8`, total decompressed bytes `u64`, and a 32-byte semantic identity. Eight 72-byte directory entries follow in section-kind order. Each stores kind `u32`, codec `u32`, absolute offset `u64`, stored length `u64`, decompressed length `u64`, record count `u64` and decompressed SHA-256. Payloads are contiguous and end exactly at EOF.

Codec zero stores raw bytes; codec one stores one zlib-wrapped DEFLATE stream. The loader requires exact compressed consumption and output length, successful termination and a matching section hash. The packer uses the lockfile-pinned `miniz_oxide` at level 9 and selects raw storage when compression is not smaller. Unrecognized versions, codecs or sections, reordered/overlapping payloads, trailing bytes, invalid totals and address-space overflows fail validation before publication.

The semantic SHA-256 input begins with ASCII `UQA Kuromoji semantic dictionary` and one zero byte, then version `u32`. For each ordered section it appends kind `u32`, decompressed length `u64`, record count `u64` and decompressed SHA-256. These integers are little-endian. Compression and physical offsets do not affect identity; schema, ordered content and provenance do. This detects content changes and does not authenticate a publisher.

## Sections

| Kind | Content | Recorded count in the bundled model |
| --- | --- | --- |
| 1 | Minimized UTF-16 lexicon and surface-to-word ranges | 325,872 surfaces |
| 2 | System words followed by unknown words | 392,168 words |
| 3 | Shared UTF-8 morphology string pool | 264,182 strings |
| 4 | Dense signed connection costs | 1,731,856 cells |
| 5 | Japanese unknown classes and UTF-16 character properties | 65,536 units |
| 6 | Java Unicode intervals | 1,114,112 code points |
| 7 | Stopwords, stop tags and completion mappings | 329 mapping keys |
| 8 | Canonical neutral-model provenance JSON | 1 manifest |

The lexicon, canonical source-ID permutation, contiguous surface word ranges, string encoding, matrix and Unicode interval encodings use the shared formats described in the [Nori bundle specification](nori-bundle-format.md#sections). They preserve lexical rank separately from Lucene source ID. System-word ranges cover exactly the first 392,127 words; the remaining 41 words belong to the 12 unknown classes.

Section 2 starts with known-word count `u32` and total-word count `u32`. Each 36-byte record stores original-word-ID delta `i32`, left context `u16`, right context `u16`, signed word cost `i16`, two zero reserved bytes, then six string references `u32` in order: POS, base form, reading, pronunciation, inflection type and inflection form. Original IDs remain nonnegative signed 32-bit values, and contexts must address the matrix. Reference `0xffffffff` means absent; POS is required. A reference to an empty string remains distinct from absence. Surface-dependent attributes are materialized through the pinned dictionary API for the exact surface.

Section 3 contains string count `u32`, then UTF-8 byte length `u32` and bytes for each string. It contains no Korean morpheme table. Section 4 stores forward/backward dimensions and signed `i16` cells in backward-row, forward-column order; the model is 1,316 by 1,316.

Section 5 contains class count `u32 = 12`, one invoke/group flags byte per class, one `(start u32, count u32)` unknown-word range per class, character count `u32 = 65536`, then `(class u8, is_kanji u8)` per unit. Classes are `NGRAM`, `DEFAULT`, `SPACE`, `SYMBOL`, `NUMERIC`, `ALPHA`, `CYRILLIC`, `GREEK`, `HIRAGANA`, `KATAKANA`, `KANJI`, `KANJINUMERIC`. The Kanji byte is one exactly for the final two classes. Unknown ranges are nonempty, contiguous and cover the unknown suffix. Other flags and inconsistent Japanese attributes are rejected.

Section 7 stores counted UTF-8 string lists for the 109 default stopwords and 27 default stop tags, then mapping count `u32`. Each mapping contains a UTF-8 key and a counted UTF-8 alternative list. Stop-list entries and mapping keys must be nonempty, unique and strictly ordered by UTF-16; alternatives preserve Lucene order and must be present and nonempty. This table belongs to completion romanization, independently of the reading-form romanization algorithm.

Section 8 contains compact UTF-8 JSON with sorted object keys, preserved array order and no trailing newline. It retains all six model file hashes, source/archive/patch/generation identities, runtime, jar/resource inventories, Japanese morphology field order, character/script vocabularies and model counts. The loader validates complete inventories, nonempty identities, matching runtimes, canonical serialization and counts against every decoded table. Declared URLs are never resolved by the loader.

## Bounds and packaged identity

Default `DictionaryLimits` are 128 MiB encoded input, 256 MiB total decompressed bytes, 1 MiB manifest JSON, 65,535 UTF-16 units per surface/string and 1,000,000 decoded strings. The string limit includes morphology strings plus stopwords, stop tags, completion keys and alternatives. Counts and references are checked before access, and failing loads leave retained immutable dictionaries usable. These are input/representation bounds; Rust vectors and lookup structures also consume memory.

The verified model produces a 6,654,306-byte bundle with 25,852,251 decompressed section bytes, artifact SHA-256 `bd3dd53f609006e72d0aa6e94ec6067400ac2be328ca0c39263ab36c3197aa97`, and semantic identity `dd2691ffee7f5a0d2c29c1a6e66dee249116767208a8cd8e783a6aa479100c8f`. These are deterministic artifact sizes and identities, not throughput or heap-memory measurements.

`uqa-kuromoji-data` embeds these exact bytes in a `no_std` library with no runtime dependencies or build scripts. Its resource manifest covers the bundle, original model manifest, complete Lucene LICENSE/NOTICE, original IPADIC COPYING and the pinned JDK Unicode notice. The release checker validates every resource and attribution in source and the actual `.crate` archive. The new data leaf has one permitted incoming runtime edge from `uqa-analysis`; the four analyzer feature configurations must include exactly their enabled language data dependencies.
