# Nori indexing measurements

`crates/uqa-storage/benches/nori_storage.rs` measures the public Memory inverted-index API with the pinned Nori analyzer and the same six original corpora as the standalone analysis benchmark. The target requires the dependency feature `uqa-analysis/nori`; it adds no storage runtime feature or dependency. Allocation instrumentation and hashing are development dependencies. JVM execution is unnecessary.

```sh
python3 scripts/run-nori-index-benchmark.py --allocation-only --output target/benchmark-runs/nori-index-native.json
python3 scripts/run-nori-index-benchmark.py --allocation-only --target wasm --output target/benchmark-runs/nori-index-wasm.json
```

`--allocation-only` verifies one allocation sample per workload and the same complete expected outputs without warmups, pilots, clocks or timing samples. Reports identify this protocol explicitly, contain no timing fields, and cannot be compared with a timing baseline. Native CI runs analysis, indexing, phrase, SQLite and redb checks independently; a failure cannot hide later checks. Selected WASM checks use the same untimed mode. Allocation ceilings and output fixtures are unchanged.

The runner shares toolchain selection, WASM linker settings, artifact capture, and provenance checks with the [analysis runner](README.md). Optional timing collection omits `--allocation-only` and requires the controlled-host policy below; `--baseline` must reference a timing report. Seven independent single-operation samples follow one warmup. Input construction and mutation are timed; initial seed construction/cloning, result destruction, and full graph validation are outside the timer. Allocator updates are disabled for timing and measured in a separate mutation. The index survives the allocation measurement, so net counts include both newly retained data and any released seed allocations. The peak is additional requested Rust heap during the mutation; it excludes the existing index, dictionary, stack, allocator metadata, and JavaScript heap.

Workloads build 256 and 2,048 documents through point insertion, then append batches of 16 documents to indexes containing 0, 256, and 2,048 documents. Document text is the corpus item at `doc_id % 6`, using its declared repetition count; batch text therefore depends on the starting document identity. Comparisons use each exact workload's complete identity. Every indexed document's versioned field metadata and every term's encoded occurrence/score cluster contribute to a length-delimited SHA-256 digest. Field lengths, document counts, and posting counts are checked separately.

Memory batches previously cloned the complete existing index before applying their input. They now prepare a private projection containing affected documents and the relevant global counters, apply the existing point replacement path in order, and publish after all analysis and arithmetic checks succeed. Repeated identities, deletions, field moves, and late failures retain their contract. Untouched documents keep their posting allocations. Full source rebuilds continue to construct complete replacement indexes.

`index-limits.json` retains complete expected graph outputs, separate 32-bit/64-bit allocation ceilings without padding, and source references for the original observations. An optional timing baseline must match the host, target, compiler, flags, analyzer, corpus, benchmark source, and graph output. CI enforces allocation and graph contracts on its own host.

These results cover in-memory indexing through the storage owner. [Physical provider measurements](PERSISTENT.md) separately cover SQLite/redb transactions and reopen. SQL/transaction snapshot overhead, phrase scoring, and full browser memory remain separate acceptance measurements.

Generated reports stay in ignored output directories and CI artifacts. See the [report storage and timing policy](README.md#report-storage-and-timing-interpretation); historical source references in the limits do not establish performance acceptance on an uncontrolled host.
