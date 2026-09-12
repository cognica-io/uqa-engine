# Nori runtime bundle format

Status: Implemented dictionary codec and offline packer; tokenizer, analyzer registration, bundled-resource distribution, and end-to-end search integration remain tracked in the [implementation plan](../plans/0006-nori-analyzer.md).

The `nori` feature of `uqa-analysis` exposes `NoriDictionary::from_bytes` and immutable dictionary queries. The `nori-tools` feature additionally exposes the offline packer and exhaustive neutral-model verifier. Neither feature registers a Nori tokenizer yet. Loading bytes does not read files, resolve URLs, download a model, or execute a JVM. Callers retain and clone the returned `Arc<NoriDictionary>`; process-wide resource interning is a separate compilation task.

## Input and regeneration

The packer consumes the six-file directory produced by the [Docker model exporter](../../tests/parity/nori/MODEL.md). It checks the exact inventory, lengths, SHA-256 hashes, provenance, vocabulary, and every encoded field. The runtime model preserves all ordered homographs, original Lucene word IDs, context IDs, costs, absent versus empty readings and morpheme lists, character definitions, and pinned Java Unicode values.

```sh
cargo run -p uqa-analysis --release --locked --features nori-tools --example pack_nori -- pack /tmp/nori-neutral /tmp/nori.uqan
cargo run -p uqa-analysis --release --locked --features nori-tools --example pack_nori -- verify /tmp/nori.uqan
```

Packing builds the UTF-16 transducer, pools strings, compacts Unicode into intervals, serializes the sections below, and decodes the result through the production loader. The verifier reconstructs every byte of all five original big-endian streams and compares their recorded lengths and hashes. It also enumerates every accepted surface and verifies its exact lookup rank. Only a completely verified bundle is atomically published; the command refuses an existing output. Runtime loading validates the bundle itself; exhaustive neutral reconstruction belongs to the offline tool.

## Framing and identity

All bundle integers use little-endian fixed widths unless explicitly marked variable. There are no native-layout structs, pointers, platform-sized integers, or implicit alignment bytes. A variable `u32` uses shortest-form unsigned LEB128 and occupies at most five bytes. A signed source-ID delta is transformed with signed 32-bit Zigzag before variable encoding; negative original-word-ID deltas use fixed signed `i32` instead.

The 56-byte header contains the eight bytes `UQANORI\0`, version `u32 = 1`, section count `u32 = 7`, total decompressed section bytes `u64`, and the 32-byte semantic dictionary identity. Exactly seven 72-byte directory entries follow, ordered by section kind. Each entry contains kind `u32`, codec `u32`, absolute payload offset `u64`, stored length `u64`, decompressed length `u64`, record count `u64`, and SHA-256 of the decompressed section bytes. Payloads are contiguous, begin immediately after the directory, and end exactly at EOF.

Codec `0` is raw bytes with equal stored and decompressed lengths. Codec `1` is exactly one zlib-wrapped DEFLATE stream. The loader requires full input consumption, exact output length, successful stream termination, and the section hash. The packer uses `miniz_oxide` at level 9, choosing raw bytes when compression is not smaller; the reviewed lockfile pins the encoder implementation. Unknown versions or codecs, extra streams, trailing bytes, overlapping sections, and unchecked length conversions are rejected.

The semantic identity is SHA-256 over the ASCII bytes `UQA Nori semantic dictionary` followed by a zero byte, version `u32`, then each section's kind `u32`, decompressed length `u64`, record count `u64`, and decompressed SHA-256. All integers in this identity stream are little-endian. Transport codec, stored length, and offsets are excluded, so equivalent raw and compressed framing has one identity. Section content, ordered metadata, and provenance are included. This identifies content and detects corruption; it does not authenticate a publisher.

## Sections

| Kind | Content | Directory record count |
| --- | --- | --- |
| 1 | UTF-16 lexicon and surface-to-word ranges | Accepted surfaces |
| 2 | System words followed by unknown words | All word entries |
| 3 | UTF-8 string pool and morphemes | Morpheme records |
| 4 | Connection cost matrix | Matrix cells |
| 5 | Unknown classes and UTF-16 character properties | 65,536 character entries |
| 6 | Pinned Java Unicode intervals | 1,114,112 code points |
| 7 | Canonical neutral-model provenance JSON | 1 |

The lexicon starts with root, node count, and arc count, each `u32`. Each node stores outgoing arc count `u32` and terminal flag `u8` in `{0,1}`. Nodes are bottom-up, the root is last, and every arc targets an earlier node. All arcs follow the nodes, grouped in node order, with label `u16` and target `u32`. Labels within a node are strictly increasing. The loader derives contiguous arc offsets, accepting-suffix counts, and rank outputs with checked arithmetic. Every state must be reachable; every accepted path must be valid UTF-16 and respect the configured surface-length limit. Equivalent suffix states may be shared without conflating their lexical ranks.

After the arcs, a surface count `u32` precedes one row per accepted surface in UTF-16 lexical order. A row holds a variable Zigzag source-ID delta from the preceding ID, initially zero, and a nonzero variable word count. Decoded source IDs are a permutation of `0..surface_count`. Word ranges are derived cumulatively from zero and must exactly cover the system-word prefix of section 2. This preserves source identity and homograph order independently of rank and excludes redundant starting offsets.

The word section begins with system-word count `u32` and total-word count `u32`. Each 32-byte entry contains original-word-ID delta `i32` from the previous original ID, initially zero; left context `u16`; right context `u16`; signed cost `i16`; POS type ordinal `u8`; left POS ordinal `u8`; right POS ordinal `u8`; three zero reserved bytes; reading string ID `u32`; morpheme start `u32`; morpheme count `u32`; and four zero reserved bytes. Original IDs must remain nonnegative signed 32-bit values. Left contexts address the backward matrix dimension and right contexts address the forward dimension. Reading ID or morpheme start `0xffffffff` means absent; an absent morpheme list requires count zero. Present empty strings and present zero-length lists remain distinct.

The morphology section begins with string count `u32`. Each string stores UTF-8 byte length `u32` and validated UTF-8 bytes. A morpheme count `u32` follows, then records of string ID `u32` and POS ordinal `u8`. Every string reference and morpheme range must address its corresponding table. POS ordinals are the exact ordered type/tag vocabulary in the manifest, not the separate numeric Lucene POS codes; both names and codes are validated.

The matrix section contains forward dimension `u32`, backward dimension `u32`, then `forward × backward` signed `i16` costs. Cell `(forward_id, backward_id)` is at `backward_id × forward_dimension + forward_id`. Both dimensions are nonzero and at most 65,536. The loader checks their product against the section before allocating.

The character section contains class count `u32 = 14`, 14 flag bytes, one `(start u32, count u32)` unknown-word range per class, character count `u32 = 65536`, and `(class u8, morphology_flags u8)` per UTF-16 unit. Unknown-word ranges are nonempty, ordered, contiguous, and cover exactly the suffix of section 2. Class flags use bit 0 for invoke and bit 1 for group. Morphology flags use bit 0 for Hanja, bit 1 for Hangul, and bit 2 for the pinned `hasCoda` arithmetic; the complete table includes non-Hangul inputs to that arithmetic. Other bits and inconsistent values are rejected.

The Unicode section contains code-point count `u32 = 1114112`, interval count `u32`, then `(exclusive_end u32, Java_category u8, flags u8, script_ordinal u16, lowercase_delta i32)` per interval. Starts are implicit from zero and the preceding end. Flags encode digit, whitespace, and space-character predicates in bits 0, 1, and 2 respectively. Adjacent identical properties are merged. The table must cover every code point exactly once, including explicitly classified surrogate code points; scalar lowercase mappings must remain scalar and in range. Script ordinals index the validated ordered manifest vocabulary. This retains the reference JVM's classification and simple lowercase independently of the host's Unicode tables.

Provenance is UTF-8 JSON with lexicographically sorted object keys, preserved array order, compact separators, and no final newline. It retains the neutral manifest, including exporter hash, pinned Docker image, complete JVM identity, Lucene version and commit, three jar hashes and lengths, nine resource hashes and lengths, dictionary source/license declarations, all five neutral file hashes and lengths, vocabulary, and model counts. The loader rejects missing identities, duplicate or incomplete inventories, inconsistent runtimes, noncanonical JSON, and counts that differ from decoded sections.

## Limits and evidence

Default limits are 128 MiB encoded input, 256 MiB total decompressed section bytes, 1 MiB provenance JSON, 65,535 UTF-16 units per surface or pooled string, and 1,000,000 pooled strings. `DictionaryLimits` can lower or raise these explicitly. These are input/representation limits, not a measured heap-memory guarantee; decoded vectors and lookup structures also consume memory. Counts and ranges are checked before their allocations or accesses, and no dictionary is published on failure.

The reviewed model produces 9,829,534 bytes with artifact SHA-256 `0d920523991ed60909972d85df630c65747ff5d49e1138e4998ca39f0079538d` and semantic identity `ee85ee5796c706ea7288e44e64c7f58ba7bd22cea5683c1f4196c29a2435d74a`. Independent neutral exports from Docker arm64 and amd64 packed to identical bundle bytes and each passed full reconstruction. An initial LZ4 layout with redundant arc/rank and absolute-ID fields occupied 27,648,214 bytes; the final compact representation and zlib framing reduced that measured artifact size while preserving every neutral hash. Native/WASM runtime memory, cold loading, tokenizer throughput, and distribution archive gates remain in the implementation plan.
