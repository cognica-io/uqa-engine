# Nori indexing measurements

`crates/uqa-storage/benches/nori_storage.rs` measures the public Memory inverted-index API with the pinned Nori analyzer and the same six original corpora as the standalone analysis benchmark. The target requires the dependency feature `uqa-analysis/nori`; it adds no storage runtime feature or dependency. Allocation instrumentation and hashing are development dependencies. JVM execution is unnecessary.

```sh
python3 scripts/run-nori-index-benchmark.py --output target/benchmark-runs/nori-index-native.json
python3 scripts/run-nori-index-benchmark.py --target wasm --output target/benchmark-runs/nori-index-wasm.json
python3 scripts/run-nori-index-benchmark.py --output target/benchmark-runs/nori-index-repeat.json --baseline target/benchmark-runs/nori-index-native.json
```

The runner shares toolchain selection, WASM linker settings, artifact capture, and provenance checks with the [analysis runner](README.md). Run measurements sequentially after other builds finish. Seven independent single-operation samples follow one warmup. Input construction and mutation are timed; initial seed construction/cloning, result destruction, and full graph validation are outside the timer. Allocator updates are disabled for timing and measured in a separate mutation. The index survives the allocation measurement, so net counts include both newly retained data and any released seed allocations. The peak is additional requested Rust heap during the mutation; it excludes the existing index, dictionary, stack, allocator metadata, and JavaScript heap.

Workloads build 256 and 2,048 documents through point insertion, then append batches of 16 documents to indexes containing 0, 256, and 2,048 documents. Document text is the corpus item at `doc_id % 6`, using its declared repetition count; batch text therefore depends on the starting document identity. Comparisons use each exact workload's complete identity. Every indexed document's versioned field metadata and every term's encoded occurrence/score cluster contribute to a length-delimited SHA-256 digest. Field lengths, document counts, and posting counts are checked separately.

Memory batches previously cloned the complete existing index before applying their input. They now prepare a private projection containing affected documents and the relevant global counters, apply the existing point replacement path in order, and publish after all analysis and arithmetic checks succeed. Repeated identities, deletions, field moves, and late failures retain their contract. Untouched documents keep their posting allocations. Full source rebuilds continue to construct complete replacement indexes.

The original native measurement is retained as a comparison artifact with source hashes captured before the implementation change. Current native and WASM reports include full environment and executable provenance. `index-limits.json` pins complete report hashes, identical graph outputs, and separate 32-bit/64-bit allocation ceilings without padding. An optional timing baseline must match the host, target, compiler, flags, analyzer, corpus, benchmark source, and graph output. The timing margin is derived from repeated measurements and recorded in that file; CI enforces allocation and graph contracts on its own host.

These results cover in-memory indexing through the storage owner. [Physical provider measurements](PERSISTENT.md) separately cover SQLite/redb transactions and reopen. SQL/transaction snapshot overhead, phrase scoring, and full browser memory remain separate acceptance measurements.

## Recorded results

Current measurements used an Apple M1 Ultra, Rust 1.90.0, Node 22.12.0, and Emscripten 6.0.3. Both runs per target reproduced every allocation counter exactly, and all five complete graph digests match the original implementation across native and WASM. Maximum bidirectional timing ratios were 1.01696 native and 1.02310 WASM. The optional timing ceiling is 1.13, using the larger measured ratio multiplied by a 1.10 policy margin and rounded upward to two decimal places.

| Workload | Native median | WASM median | Native maximum additional heap | WASM maximum additional heap |
| --- | ---: | ---: | ---: | ---: |
| Build 256 documents | 237.66 ms | 682.90 ms | 9,362,090 bytes | 8,196,798 bytes |
| Build 2,048 documents | 1,958.35 ms | 5,551.65 ms | 65,221,268 bytes | 61,045,520 bytes |
| Append 16 to 2,048 documents | 16.59 ms | 46.28 ms | 2,102,359 bytes | 1,317,263 bytes |

The original native implementation requested a peak of 65,311,546 additional bytes for the last workload; affected-document staging reduces that by approximately 96.8%. The recorded original sample had a 24.22 ms median. Its source/compiler identity was captured before the change, but the initial run did not capture complete CPU/executable metadata, so it is retained as an allocation/output comparison rather than an eligible timing-gate baseline. The gate's regression test explicitly confirms that these original whole-index-copy allocation measurements fail the new ceiling.
