# uqa-analysis

`uqa-analysis` is the UQA Engine crate for tokenizers, character filters, token filters, and analyzers.

The optional `nori` feature exposes a validated immutable Korean dictionary, user-rule compiler, and native rolling Viterbi tokenizer with lossless UTF-16 morphology and graph attributes. `nori-tools` adds the offline packer and complete neutral-model verifier. The standalone tokenizer is verified against pinned Lucene fixtures; filters, generic analyzer registration, and retrieval integration remain under development. Ported code retains the Lucene license, notice, and modification attribution in `THIRD-PARTY/`. See the [analyzer reference](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/reference/06-text-analyzers.md#standalone-korean-tokenization) and [bundle format](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/nori-bundle-format.md).

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
