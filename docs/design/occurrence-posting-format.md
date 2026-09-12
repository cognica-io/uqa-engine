# Lossless token occurrence format

The common storage library implements the binary term keys, occurrence values, field staging, and version 2 codecs described here. The Memory provider now stores exact term keys and complete occurrence graphs with revision and original source-end metadata. Key/Value and SQLite still write their existing linear representation until atomic source rebuilds install the new contract. This document specifies the implemented memory representation and available codecs; persistent migration and complete Nori retrieval remain pending. The [Nori plan](../plans/0006-nori-analyzer.md) tracks those consumers and durable analyzer revisions.

## Values and ownership

`uqa-core::TokenOccurrence` contains a zero-based `u32` position, a positive `u32` position length, and optional `TokenOffsets`. The end position must fit in `u32`. Offsets contain original UTF-8 start/end and original UTF-16 start/end as `u64`; each start is at most its end. Empty spans are valid. UTF-16 spans may split a surrogate pair while the corresponding UTF-8 range covers its complete original scalar. Source ranges need not increase between occurrences. `validate()` checks these arithmetic invariants; actual scalar boundaries and source lengths require the original source.

Each `(field, term, document)` occurrence list preserves emission order, equal starts, different lengths at one start, and repeated identical edges. Frequency is the list length. `OccurrencePosting::positions()` produces sorted unique starts for compatibility; that projection cannot supply occurrence frequency or reconstruct the graph. `Payload.positions` remains the existing compatibility value and does not acquire implicit term identity.

`uqa-storage::inverted_index::analyze_index_field(&CompiledAnalyzer, text)` stages a complete source field before mutation. It accumulates the analyzer's increments, preserves source spans, groups occurrences under canonical term keys, and retains final source offsets and the final skipped-position increment. Its length follows the compiled descriptor: `EmittedTokens` counts all tokens; `DiscountOverlaps` counts tokens whose increment is positive, without counting removed-position holes. The returned `AnalyzedField` does not itself persist a field binding or mutate an index.

A provider must associate the positional format with the exact analyzer descriptor and length policy in field metadata, and preserve the staged field/document end state in its document metadata. Rebuilds must publish that metadata, occurrences, reverse terms, document lengths, and dependent statistics together. Memory implements this association using `IndexedFieldMetadata` and its retained index-side compiled handle; the persistent providers still require this integration. The binary occurrence payload alone cannot identify an analyzer revision.

## Memory provider access and publication

`MemoryInvertedIndex` runs `analyze_index_field` for every source field and publishes canonical keys, complete occurrence lists, reverse terms, and `IndexedFieldMetadata` with checked field/corpus counters. Metadata contains the exact analyzer fingerprint, occurrence format version, declared length policy and length, original final offsets, and final skipped-position increment. Empty and all-stopped fields retain that metadata and count as indexed fields even when they have no term postings.

`InvertedIndex::get_occurrence_postings` returns complete document-ordered occurrence lists; `get_occurrences` looks up one document. `get_posting_list_key`, `posting_cursor_key`, `doc_freq_key`, `get_term_freq_key`, and `vocabulary_keys` use canonical term identity. Existing string lookup methods select the matching scalar key. Unique positions remain a cached compatibility projection; score cursors and all frequency accessors count the complete occurrence list. String vocabulary projection returns an error if any term contains unpaired units. `IndexStats::doc_freq_utf16` and `set_doc_freq_utf16` preserve those identities while sharing scalar terms with the existing string API.

A populated memory field, including an all-stopped field, rejects changing or removing its index revision without an atomic source rebuild. A search-only assignment preserves its stored graph metadata. `rebuild_with_analyzer_revision` builds an empty candidate from supplied original sources and publishes its selected bindings with all replacement documents after success. Read-only and writable snapshots retain their original data and compiled handles. Point replacement, removal, clearing, and batch replacement update graph/end metadata together; a failed analysis or checked-counter update leaves the previous state. Bulk insertion currently stages through a full provisional copy, whose heap and throughput costs remain part of the planned benchmarks.

Legacy providers' new scalar-key lookup defaults delegate to their existing methods, and unpaired keys fail checked projection. Their occurrence and field-metadata methods return explicit unsupported errors instead of fabricating missing graph information. Graph phrase matching, lossless query terms throughout ranking, and original-source highlighting remain separate consumer work.

## Canonical term keys

`TokenTermKey` owns validated bytes. `from_text` and `from_term` construct keys; `from_bytes` rejects invalid or alternate encodings. `as_bytes` and `into_bytes` expose the persistent key, and `to_term` recovers the original `TokenTerm`.

| First byte | Payload | Constraint |
| --- | --- | --- |
| `0` | UTF-8 bytes | Valid scalar UTF-8; empty payload is the empty term |
| `1` | Big-endian UTF-16 units | Even byte length and at least one unpaired surrogate |

All valid UTF-16 strings, including supplementary pairs, use the UTF-8 form. An empty UTF-16 form or a raw encoding of scalar-only text is rejected. A string containing U+FFFD cannot collide with a raw surrogate term. Field and table names are separate key components. Vocabulary ordering is lexicographic byte order over these canonical keys, not a language collation.

## Cluster score and occurrence values

One externally supplied cluster identity covers 65,536 consecutive document IDs. Document IDs are strictly increasing within the cluster. `encode_occurrence_cluster` returns a score blob and a separate occurrence blob. `decode_occurrence_cluster` requires both to have version 2. The score-only cursor and `decode_all_scores` accept existing version 1 score blobs and new version 2 blobs, including separate clusters of each version in one cursor.

All fixed-width multibyte integers are little-endian. Variable-length integers are unsigned base-128 encodings, least-significant groups first, using a high continuation bit. They are canonical: no redundant final zero group, at most ten bytes for `u64`, and no tenth-byte overflow. Encoded offsets and directory lengths must fit their declared widths.

| Header offset | Width | Score blob | Occurrence blob |
| --- | --- | --- | --- |
| 0 | 4 bytes | `UQCS` | `UQCP` |
| 4 | 1 byte | Version `2` | Version `2` |
| 5 | 3 bytes | Reserved, zero | Reserved, zero |
| 8 | `u32` | Posting count | Posting count |
| 12 | `u32` | Score-block count | Posting count plus one |

A score block contains at most 128 postings. Its 28-byte directory entry stores posting count as `u16`, final document offset within the cluster as `u16`, then six `u32` absolute blob offsets delimiting the document, frequency, and document-length streams. The three streams are contiguous and exhaust the blob in directory order. The first document value in each block is its absolute cluster offset; later values are positive deltas. Frequencies and document lengths use independent positive `u64` values. Version 2 permits frequency greater than document length. Version 1 retains its original frequency/length consistency check.

The occurrence directory contains one `u32` payload-relative offset per posting plus a final end offset. The first is zero, the last is the payload length, and offsets are ordered. Each posting decodes exactly the frequency declared by its score stream. Every occurrence encodes, in order:

1. Start-position delta as a varint, relative to zero for the first occurrence. Zero deltas preserve stacked tokens and multiplicity.
2. Position length as a varint. Both values and the resulting end position must fit in `u32`; the length is positive.
3. One flag byte: zero for absent source coordinates, one for present coordinates. Other values fail.
4. When present, four `u64` varints: UTF-8 start, UTF-8 span length, UTF-16 start, UTF-16 span length. Checked addition reconstructs each end.

Decoding rejects unsupported versions, reserved bits, inconsistent counts/directories, truncated values, overflow, unknown flags, invalid positions, and trailing data. Before allocating occurrence vectors, it bounds the count by the minimum encoded bytes required for that posting. Range arithmetic validation cannot replace original-source validation when a consumer uses the decoded offsets.

The smallest occurrence blob, for one document and one edge at position zero with length one and no source coordinates, is `55 51 43 50 02 00 00 00 01 00 00 00 02 00 00 00 00 00 00 00 03 00 00 00 00 01 00`. A regression pins this encoding and the corresponding score bytes.

## Reverse-document vocabulary

`encode_term_keys` and `decode_term_keys` use magic `UQCT`, version `2`, three reserved zero bytes, and a `u32` term count. Each term then has a `u32` byte length followed by its complete tagged key. Keys are strictly increasing and unique. An empty vocabulary is valid. Invalid keys, duplicates, unordered keys, oversized/truncated counts, and trailing bytes fail. The separate legacy `encode_terms` and `decode_terms` retain version 1 scalar vocabulary for migration reads.

## Scoring and migration

A score cursor retains occurrence frequency and the actual normalization length independently. Default frequency projections consult the index's authoritative frequency accessor instead of counting unique positions. WAND with an index also uses that frequency; cursor WAND consumes the stored values directly. Calibration consumes score cursors without decoding positional payloads. Exhaustive Engine scoring and persisted SQLite block maxima use the stored length without increasing it to the frequency. Standalone materialized WAND without an index retains its existing positional-frequency approximation; complete graph scoring supplies an index or a score cursor.

Existing position bytes cannot recover gaps, overlap multiplicity, long edges, or original source spans. `decode_occurrence_cluster` rejects legacy positional data with a source-rebuild requirement; it never infers unit edges from the unique starts. The legacy cluster decoder remains available for validated old-format reads. Enabling provider graph capability requires an atomic rebuild from retained source under explicit resolved descriptors, including a declared legacy length policy and invalidation of prior score/block metadata. Merely changing a header or converting existing starts does not perform that migration.
