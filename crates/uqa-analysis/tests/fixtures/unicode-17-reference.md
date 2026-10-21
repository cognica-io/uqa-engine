# Retained Unicode 17 reference

These synthetic descriptors and the scalar/context digest were captured from UQA Engine `50255347` before compiler-independent Unicode pinning, using Rust `1.95.0 (59807616e 2026-04-14)` (`char::UNICODE_VERSION == (17, 0, 0)`), regex `1.12.4`, regex-automata `0.4.14`, regex-syntax `0.8.11`, and unicode-normalization `0.1.25`. They contain built-in public configurations only. The historical lowercase pipeline combined Rust's Unicode 17 scalar mappings with regex-syntax's Unicode 16 `Cased` and `Case_Ignorable` properties; replacing it with Unicode 17 string lowercasing would change final-sigma behavior.

`unicode-17-reference.rs` runs against that unmodified historical `uqa-analysis`, not the implementation under test. Create an isolated Cargo executable with a path dependency on that checkout's analysis crate, `sha2 = "0.10"`, and exact versions of the four dependencies above. Copy this source to `src/main.rs`, then run `cargo +1.95.0 run --release -- /tmp/unicode-17-reference`. Its output is `cases=3336192 sha256=549ed6a569bd3f7c57a55eb65a02272fde54b852b0f9b673275225f1dc90733c`. The fixture files must match `unicode-17/*.json` byte for byte. Each scalar contributes its little-endian u32, whitespace boolean, and the little-endian u64 UTF-8 length and output for `A{scalar}Σ`, `AΣ{scalar}`, and `AΣ{scalar}A`.

The pinned 28-entry lowercase delta in `src/unicode/unicode_17.rs` comes from the official [Unicode 16 database](https://www.unicode.org/Public/16.0.0/ucd/) and [Unicode 17 database](https://www.unicode.org/Public/17.0.0/ucd/). Start with identity for every scalar, apply UnicodeData field 13 (simple lowercase), then replace those entries with the full lowercase mappings in unconditional SpecialCasing rows. Comparing the complete resulting functions yields exactly 28 changes, each a single output scalar. `White_Space` from PropList is identical in both versions. The generated table includes the complete Unicode license notice. The following source hashes fix the generator inputs:

| Input | SHA-256 |
| --- | --- |
| 16.0.0/UnicodeData.txt | `ff58e5823bd095166564a006e47d111130813dcf8bf234ef79fa51a870edb48f` |
| 16.0.0/SpecialCasing.txt | `8d5de354eef79f2395a54c9c7dcebbaf3d30fc962d0f85611ea97aa973a0c451` |
| 16.0.0/PropList.txt | `53d614508e2a0b2305a8aa21cd60d993de9326cdf65993660dfcce4503548583` |
| 17.0.0/UnicodeData.txt | `2e1efc1dcb59c575eedf5ccae60f95229f706ee6d031835247d843c11d96470c` |
| 17.0.0/SpecialCasing.txt | `efc25faf19de21b92c1194c111c932e03d2a5eaf18194e33f1156e96de4c9588` |
| 17.0.0/PropList.txt | `130dcddcaadaf071008bdfce1e7743e04fdfbc910886f017d9f9ac931d8c64dd` |

To verify or regenerate the delta, run `python3 scripts/check-retained-unicode-data.py /path/to/downloaded/ucd-files`, where filenames are `<version>-<name>` as above. This verifier checks all six hashes, computes full mappings including unconditional special casing, checks identical whitespace sets, and compares every generated entry to the committed table. It makes no network requests.
