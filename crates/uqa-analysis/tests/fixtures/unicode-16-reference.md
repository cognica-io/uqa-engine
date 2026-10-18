# Unicode 16 analyzer reference

`unicode-16-reference.rs` uses only the Rust 1.90 standard library, whose `char::UNICODE_VERSION` is `(16, 0, 0)`. It does not import UQA or ICU. For each valid scalar in ascending order, the binary emits the scalar as little-endian `u32`, one whitespace Boolean byte, then the UTF-8 lowercase output for `A{scalar}Σ`, `AΣ{scalar}` and `AΣ{scalar}A`, each preceded by its byte length as little-endian `u64`. The stream covers 1,112,064 scalars and 3,336,192 contextual mappings.

The SHA-256 captured on 2026-10-18 is `b68614b465ba2c74ff335200b4ce7fad2c1105814ed284340e50c017f7b7eb15`. The analyzer test compares each result with whole-string ICU lowercase, then checks that oracle and the runtime whitespace classification against this independently captured digest. The digest therefore also rejects a silent change in the oracle's Unicode data.

Reproduce without a Cargo build or package archive:

```sh
rustc +1.90.0 --edition=2024 -O crates/uqa-analysis/tests/fixtures/unicode-16-reference.rs -o /tmp/uqa-unicode-16-reference
/tmp/uqa-unicode-16-reference | shasum -a 256
```

The implementation uses the existing workspace-pinned ICU4X 2.0 case-mapping and property data. Unicode documents that release's [Unicode 16 data](https://blog.unicode.org/2025/05/icu4x-20-released.html). It preserves the generic analyzer's existing contract; the separate morphology profiles and SQL collation behavior retain their own data owners.
