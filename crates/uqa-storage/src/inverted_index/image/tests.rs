//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod ownership;
mod stream;
use crate::{
    inverted_index::{AnalyzerBindings, AnalyzerPhase, RetainedInvertedIndexBuilder},
    memory_capture::MemoryCaptureRegistry,
    InvertedIndex, TokenTermKey,
};
use std::collections::BTreeMap;
use uqa_core::{memory::MemoryError, TokenOccurrence};

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("body".into(), text.into()),
        ("empty".into(), String::new()),
    ])
}

fn builder(control: &StorageReadControl) -> RetainedInvertedIndexBuilder {
    RetainedInvertedIndexBuilder::new(
        &AnalyzerBindings::new(uqa_analysis::keyword_analyzer()),
        control,
    )
    .unwrap()
}

fn encode(index: &dyn InvertedIndex, id: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let written = index
        .write_image_document(id, &mut bytes, &StorageReadControl::with_limit(0))
        .unwrap();
    assert_eq!(written.unwrap_or(0), bytes.len() as u64);
    written.map(|_| bytes)
}

/// Independent wire fixture: one field, one raw UTF-16 key and two equal graph edges without source offsets. No encoder or analyzer creates these expectations.
pub(in crate::inverted_index) fn fixture(discount: bool) -> Vec<u8> {
    let mut bytes = b"UQATXT01".to_vec();
    bytes.extend_from_slice(&1_u64.to_le_bytes());
    bytes.extend_from_slice(&1_u64.to_le_bytes());
    bytes.push(b'x');
    bytes.extend_from_slice(b"UQIM");
    bytes.extend_from_slice(&[1, 2, u8::from(discount), 0]);
    bytes.extend_from_slice(&[7; 32]);
    for n in [if discount { 1 } else { 2 }, 0, 6, 0, 3] {
        bytes.extend_from_slice(&u64::to_le_bytes(n));
    }
    bytes.extend_from_slice(&4_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u64.to_le_bytes());
    bytes.extend_from_slice(&3_u64.to_le_bytes());
    bytes.extend_from_slice(&[1, 0xd8, 0x00]);
    bytes.extend_from_slice(&2_u64.to_le_bytes());
    for _ in 0..2 {
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&3_u32.to_le_bytes());
        bytes.push(0);
    }
    assert_eq!(bytes.len(), 154);
    bytes
}

#[test]
fn evaluated_text_images_do_not_resolve_or_run_deferred_analyzers() {
    let analyzer = uqa_analysis::Analyzer::new(
        uqa_analysis::Tokenizer::Pattern {
            pattern: "[".into(),
        },
        Vec::new(),
        Vec::new(),
    );
    assert!(analyzer.compile().is_err());
    let bindings = AnalyzerBindings::new(analyzer);
    let control = StorageReadControl::with_limit(1 << 20);
    let bytes = fixture(false);
    let mut imported = RetainedInvertedIndexBuilder::new(&bindings, &control).unwrap();
    imported
        .add_image_document(1, &mut bytes.as_slice())
        .unwrap();
    let imported = imported.finish().unwrap();
    assert_eq!(encode(&imported, 1).unwrap(), bytes);
    assert!(imported.index_analyzer_revision("x").is_err());
    assert_eq!(imported.total_field_length("x").unwrap(), 2);
    drop(imported);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn evaluated_text_images_validate_original_offsets_and_graph_edges_before_publication() {
    let mut source = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    source
        .add_document(1, BTreeMap::from([("x".into(), "β a".into())]))
        .unwrap();
    let bytes = encode(&source, 1).unwrap();
    // The first sorted key is scalar "a". Its edge starts after the fixed field header and encoded key.
    let occurrence = 16 + 8 + 1 + 84 + 8 + 8 + 2 + 8;
    assert_eq!(bytes[occurrence + 8], 1);
    let mut invalid = Vec::new();
    for (offset, value) in [(occurrence + 4, 0_u32), (occurrence, u32::MAX)] {
        let mut changed = bytes.clone();
        changed[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        invalid.push(changed);
    }
    for offset in [
        occurrence + 9,
        occurrence + 17,
        occurrence + 25,
        occurrence + 33,
    ] {
        let mut changed = bytes.clone();
        changed[offset..offset + 8].copy_from_slice(&99_u64.to_le_bytes());
        invalid.push(changed);
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let mut imported = builder(&control);
    let baseline = control.memory().used();
    for bytes in &invalid {
        assert!(imported
            .add_image_document(1, &mut bytes.as_slice())
            .is_err());
        assert_eq!(control.memory().used(), baseline);
    }
    imported
        .add_image_document(1, &mut bytes.as_slice())
        .unwrap();
    let imported = imported.finish().unwrap();
    assert_eq!(encode(&imported, 1).unwrap(), bytes);
    drop(imported);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn evaluated_text_images_keep_wire_identity_duplicates_raw_terms_and_historical_metadata() {
    for discount in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let bytes = fixture(discount);
        let mut imported = builder(&control);
        imported
            .add_image_document(37, &mut bytes.as_slice())
            .unwrap();
        let imported = imported.finish().unwrap();
        assert_eq!(encode(&imported, 37).unwrap(), bytes);
        assert!(encode(&imported, 3).is_none());
        let term = TokenTermKey::from_bytes(vec![1, 0xd8, 0x00]).unwrap();
        assert_eq!(
            imported.get_occurrences(37, "x", &term).unwrap(),
            vec![
                TokenOccurrence {
                    position: 1,
                    position_length: 3,
                    offsets: None
                };
                2
            ]
        );
        let metadata = imported.indexed_field_metadata(37, "x").unwrap().unwrap();
        assert_eq!(metadata.analyzer_fingerprint.as_bytes(), &[7; 32]);
        assert_eq!(metadata.length, if discount { 1 } else { 2 });
        assert_eq!(metadata.final_position_increment, 4);
        assert_eq!(metadata.final_offsets.end_utf16, 3);
        assert_eq!(imported.doc_count().unwrap(), 1);
        assert_eq!(imported.field_doc_count("x").unwrap(), 1);
        assert_eq!(imported.total_field_length("x").unwrap(), metadata.length);
        drop(imported);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn evaluated_text_images_preserve_lazy_cuts_and_corpus_statistics_after_source_loss() {
    let config: uqa_analysis::Analyzer = serde_json::from_str(r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"synonym","synonyms":{"a":["a","a"]}}]}"#).unwrap();
    let revision = uqa_analysis::AnalyzerResources::default()
        .compile_with_length_policy(&config, uqa_analysis::TokenLengthPolicy::DiscountOverlaps)
        .unwrap();
    let mut bindings = AnalyzerBindings::new(uqa_analysis::whitespace_analyzer());
    bindings
        .bind_revisions(
            "body",
            revision,
            uqa_analysis::keyword_analyzer().compile().unwrap(),
        )
        .unwrap();
    let mut source = MemoryInvertedIndex::with_bindings(bindings.clone());
    source.add_document(1, fields("a β a")).unwrap();
    source.add_document(2, fields("β other")).unwrap();
    let before = [encode(&source, 1).unwrap(), encode(&source, 2).unwrap()];
    let expected = source.field_stats("body").unwrap();
    let registry = MemoryCaptureRegistry::new();
    let source = registry
        .cell(Box::new(source) as Box<dyn InvertedIndex>)
        .unwrap();
    let retention = StorageReadControl::with_limit(1 << 20);
    let cut = registry.capture(&retention).unwrap();
    source
        .write()
        .add_document(1, fields("replacement"))
        .unwrap();
    source
        .write()
        .set_field_analyzer(
            "body",
            uqa_analysis::whitespace_analyzer(),
            AnalyzerPhase::Search,
        )
        .unwrap();
    source.write().remove_document(2).unwrap();
    let snapshot = source.captured(&cut).unwrap();
    drop(source);
    drop(cut);
    let control = StorageReadControl::with_limit(1 << 20);
    let mut restored = RetainedInvertedIndexBuilder::new(&bindings, &control).unwrap();
    for (index, bytes) in before.iter().enumerate() {
        let id = index as u64 + 1;
        assert_eq!(encode(snapshot.as_ref().as_ref(), id).as_ref(), Some(bytes));
        restored
            .add_image_document(id, &mut bytes.as_slice())
            .unwrap();
    }
    drop(snapshot);
    assert_eq!(retention.memory().used(), 0);
    let restored = restored.finish().unwrap();
    let actual = restored.field_stats("body").unwrap();
    assert_eq!(actual.total_docs, expected.total_docs);
    assert_eq!(actual.avg_doc_length, expected.avg_doc_length);
    for term in ["a", "β", "other", "replacement"] {
        assert_eq!(
            actual.doc_freq("body", term),
            expected.doc_freq("body", term)
        );
    }
    assert_eq!(restored.total_field_length("body").unwrap(), 5);
    assert_eq!(restored.field_doc_count("empty").unwrap(), 2);
    assert_eq!(restored.total_field_length("empty").unwrap(), 0);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn evaluated_text_image_rejection_is_atomic_for_malformed_and_truncated_units() {
    let bytes = fixture(false);
    let mut corrupt = Vec::new();
    for (offset, value) in [
        (0, b'X'),
        (24, 255),
        (25, b'X'),
        (29, 255),
        (31, 2),
        (65, 3),
        (125, 2),
        (140, 0),
        (144, 2),
        (145, 0),
    ] {
        let mut changed = bytes.clone();
        changed[offset] = value;
        corrupt.push(changed);
    }
    for offset in [8, 16, 109, 117, 128] {
        let mut changed = bytes.clone();
        changed[offset..offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        corrupt.push(changed);
    }
    for offset in [8, 128] {
        let mut changed = bytes.clone();
        changed[offset..offset + 8].fill(0);
        corrupt.push(changed);
    }
    let mut duplicate = bytes.clone();
    duplicate[109..117].copy_from_slice(&2_u64.to_le_bytes());
    duplicate.extend_from_slice(&bytes[117..]);
    corrupt.push(duplicate);
    let mut duplicate = bytes.clone();
    duplicate[8..16].copy_from_slice(&2_u64.to_le_bytes());
    duplicate.extend_from_slice(&bytes[16..]);
    corrupt.push(duplicate);
    let mut trailing = bytes.clone();
    trailing.push(0);
    corrupt.push(trailing);
    for invalid in corrupt
        .iter()
        .map(Vec::as_slice)
        .chain((0..bytes.len()).map(|end| &bytes[..end]))
    {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut imported = builder(&control);
        imported
            .add_image_document(1, &mut bytes.as_slice())
            .unwrap();
        let before = control.memory().used();
        assert!(imported.add_image_document(2, &mut &*invalid).is_err());
        assert_eq!(control.memory().used(), before);
        let imported = imported.finish().unwrap();
        assert_eq!(encode(&imported, 1).unwrap(), bytes);
        assert!(encode(&imported, 2).is_none());
        assert_eq!(imported.doc_count().unwrap(), 1);
        drop(imported);
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn evaluated_text_image_admission_preserves_prior_documents_and_exact_retention() {
    let bytes = fixture(false);
    let mut rejected = 0;
    let mut admitted = 0;
    for available in [0, 1, 32, 256, 512, 1024, 2048, 4096, 8192, 16384] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut imported = builder(&control);
        imported
            .add_image_document(1, &mut bytes.as_slice())
            .unwrap();
        let blocker = control
            .memory()
            .reserve(control.memory().limit() - control.memory().used() - available)
            .unwrap();
        let before = control.memory().used();
        match imported.add_image_document(2, &mut bytes.as_slice()) {
            Ok(()) => admitted += 1,
            Err(StorageBackendError::Memory(_)) => {
                rejected += 1;
                assert_eq!(control.memory().used(), before);
            }
            Err(error) => panic!("unexpected admission error: {error}"),
        }
        drop(blocker);
        let before = control.memory().used();
        assert!(imported
            .add_image_document(1, &mut bytes.as_slice())
            .is_err());
        assert_eq!(control.memory().used(), before);
        let imported = imported.finish().unwrap();
        assert_eq!(encode(&imported, 1).unwrap(), bytes);
        drop(imported);
        assert_eq!(control.memory().used(), 0);
    }
    assert!(admitted > 0 && rejected > 0);
}

#[test]
fn evaluated_text_images_stream_large_occurrence_lists_without_export_scratch() {
    struct Sink {
        bytes: usize,
        largest: usize,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes += bytes.len();
            self.largest = self.largest.max(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut source = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
    source
        .add_document(1, fields(&"repeated ".repeat(32_768)))
        .unwrap();
    let control = StorageReadControl::with_limit(0);
    let mut sink = Sink {
        bytes: 0,
        largest: 0,
    };
    let measured = allocation_counter::measure(|| {
        let count = source
            .write_image_document(1, &mut sink, &control)
            .unwrap()
            .unwrap();
        assert_eq!(count, sink.bytes as u64);
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(control.memory().peak(), 0);
    assert!(sink.bytes > 1 << 20);
    assert!(sink.largest <= 4096);
    let mut imported = builder(&StorageReadControl::with_limit(1024));
    assert!(matches!(
        imported.add_image_document(1, &mut encode(&source, 1).unwrap().as_slice()),
        Err(StorageBackendError::Memory(MemoryError::Limit { .. }))
    ));
}

#[test]
fn evaluated_text_images_keep_original_cancellation_between_io_attempts() {
    struct Interrupt {
        token: uqa_core::CancellationToken,
        calls: usize,
        partial: bool,
    }
    impl Read for Interrupt {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.token.cancel();
            if self.partial {
                bytes[0] = b'U';
                Ok(1)
            } else {
                Err(std::io::ErrorKind::Interrupted.into())
            }
        }
    }
    impl Write for Interrupt {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            self.calls += 1;
            self.token.cancel();
            if self.partial {
                Ok(1)
            } else {
                Err(std::io::ErrorKind::Interrupted.into())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for partial in [false, true] {
        let control = StorageReadControl::with_limit(1 << 20);
        let mut io = Interrupt {
            token: control.cancellation().clone(),
            calls: 0,
            partial,
        };
        let mut imported = builder(&control);
        let before = control.memory().used();
        assert!(matches!(
            imported.add_image_document(1, &mut io),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(io.calls, 1);
        assert_eq!(control.memory().used(), before);
        drop(imported);
        control.cancellation().reset();
        let mut source = MemoryInvertedIndex::new(uqa_analysis::whitespace_analyzer());
        source.add_document(1, fields("old")).unwrap();
        let snapshot = source.snapshot_with_control(&control).unwrap();
        io.calls = 0;
        assert!(matches!(
            snapshot.write_image_document(1, &mut io, &StorageReadControl::with_limit(0)),
            Err(StorageBackendError::Cancelled(_))
        ));
        assert_eq!(io.calls, 1);
        drop(snapshot);
        assert_eq!(control.memory().used(), 0);
    }
}
