# Fixed SciFact / MiniLM vector fixture

This fixture selects the first 512 corpus rows and first 32 test-query rows in the verified BEIR SciFact source order, before evaluating DiskANN. Each binary contains row-major little-endian IEEE-754 float32 vectors with 384 components. Corpus row numbers 1 through 512 are the SQL document identities; `manifest.json` preserves the original SciFact identifiers and source hashes. The fixture contains transformed vectors and identifiers, without the original text or model weights.

The source dataset is [SciFact](https://github.com/allenai/scifact), introduced by David Wadden, Shanchuan Lin, Kyle Lo, Lucy Lu Wang, Madeleine van Zuylen, Arman Cohan and Hannaneh Hajishirzi in *Fact or Fiction: Verifying Scientific Claims*. The [authors' license statement](https://github.com/allenai/scifact/blob/master/LICENSE.md) identifies claims and annotations as CC BY 4.0 and the S2ORC abstracts as ODC-By 1.0. This fixture derives query vectors from those claims and corpus vectors from those abstracts; retain this attribution and the source identifiers when redistributing it.

Vectors were generated with the existing `scripts/prepare-beir-benchmark.py` contract: sentence-transformers 5.2.2, normalized CPU embeddings from `sentence-transformers/all-MiniLM-L6-v2` at revision `1110a243fdf4706b3f48f1d95db1a4f5529b4d41`, with a maximum sequence length of 256. The [pinned model card](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/blob/1110a243fdf4706b3f48f1d95db1a4f5529b4d41/README.md) records Apache-2.0 for the model. The manifest records the archive, complete prepared-input and final binary hashes, including the prefix selection and float32 conversion.

The workload checks ANN membership against exact SQL over this fixed subset and scores against independent literal/numerical expectations. It is neither a full SciFact relevance evaluation nor an empirical calibration or performance claim. Reports belong in ignored output directories; verifier unit tests use small independently specified fixtures.

Ordinary correctness checks use the checked-in bytes and require no embedding model download. To reproduce them from the verified prepared BEIR data, run the following from the repository root; the freezer rejects changed dataset/model identities, source hashes, prefix identifiers, dimensions or final float32 bytes before writing its output:

```sh
python3 scripts/freeze-diskann-fixture.py \
  --specification benchmarks/vector-search/fixtures/scifact-minilm-prefix512-v1/manifest.json \
  --output target/benchmark-runs/diskann-fixtures/reproduced
```

Prepare the original pinned inputs with [`scripts/prepare-beir-benchmark.py`](../../../../scripts/prepare-beir-benchmark.py) when needed, following the existing [BEIR requirements](../../../beir/README.md). This command prepares embeddings; the timing benchmark runner is separate.
