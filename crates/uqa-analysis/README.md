# uqa-analysis

`uqa-analysis` is the UQA Engine crate for tokenizers, character filters, token filters, and analyzers.

The optional `nori` feature exposes a validated immutable Korean dictionary loader and lookups; `nori-tools` adds the offline packer and complete neutral-model verifier. The tokenizer and public analyzer registration are still under development. See the [bundle format](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/nori-bundle-format.md) for the representation, limits, and reproducible commands.

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
