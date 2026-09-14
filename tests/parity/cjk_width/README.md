# CJK width reference

The [manifest](manifest.json) pins Lucene 10.5.1 core/common jars and the same Docker JDK used by Nori. `CJKWidthReference.java` invokes Lucene's public `CJKWidthCharFilter`; Java source execution, including compilation, runs only in Docker. The shared `tests/parity/lucene_runtime.py` validates jar sizes/hashes and supplies a read-only container with an explicit classpath and networking disabled. Nori retains its language-specific resource validator and existing tool entry points.

The [expected result](expected.json) contains 19 fixed examples, their complete UTF-16 units and corrected boundary offsets, and two SHA-256 identities. One identity covers the stream of all 1,112,064 Unicode scalar values; the other covers 1,672 three-unit inputs combining every U+3000–U+3100 and U+FF00–U+FFA0 value with both choices of first and second halfwidth voiced marks. These cover repeated-mark emission and both sides of the conversion ranges. Exhaustive output is hashed in memory and never written as a model dump.

Each hashed record writes the input UTF-16 unit count as a big-endian u32, its units as big-endian u16 values, the output in the same format, then the boundary count and every `correctOffset` result as big-endian u32 values. The scalar stream is one record; combination records follow range order, ascending initial scalar, and U+FF9E then U+FF9F for each following mark. The expected result also records the Java source hash and runtime identity. Rust tests independently run the public character-filter path and hash the complete output and original-source boundary projections using the same record format.

```sh
python3 tests/parity/cjk_width/run_reference.py
python3 tests/parity/cjk_width/run_reference.py --offline
cargo test -p uqa-analysis --locked --lib char_filter::width
```

Use `--platform linux/amd64` on an amd64 Docker host. `--write-expected` records an intentional reviewed reference change; ordinary verification rejects a changed runtime, corpus count, source or result. The subprocess has a 120-second bound. The normal reference CI job reruns this verifier, and analysis unit tests cover the same identities without requiring a JVM. This is deterministic compatibility verification, not performance measurement or evidence that the Japanese tokenizer is implemented.
