# uqa-nori-data

`uqa-nori-data` is the UQA Engine crate for the pinned portable Nori dictionary and its provenance.

The Rust wrapper uses the workspace license. The converted dictionary retains its upstream notices in `THIRD-PARTY/`, including the complete Lucene license and notice, MeCab-ko-dic COPYING, and the pinned JDK Unicode notice. `data/resource_manifest.json` records hashes for the bundle, original export manifest, and attribution files. Conversion changes storage layout while preserving the exported model values. This crate exposes immutable bytes only; the optional analysis feature resolves them through `NoriResources`. Retrieval integration and official binding delivery remain under development. See the [bundle format and regeneration commands](https://github.com/cognica-io/uqa-engine/blob/main/docs/design/nori-bundle-format.md).

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
